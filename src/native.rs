//! Native bring-up: discovery, presentation probes, and opt-in output sessions.

mod presentation;
mod restore;
mod runtime;
pub use presentation::{present, present_switch_test};
pub use runtime::{NativeOutput, run_session, run_session_auto, run_session_pair};

use std::{
    error::Error,
    io::{self, IsTerminal},
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::Path,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        session::{Session, libseat::LibSeatSession},
        udev::{self, UdevBackend},
    },
    reexports::{
        calloop::EventLoop,
        drm::{self, control::Device as _},
        rustix::fs::OFlags,
    },
};

// The drm crate's query-only Device traits need just an fd. Do not use
// Smithay's DrmDeviceFd here: constructing it attempts to become DRM master.
struct ReadOnlyCard(OwnedFd);

impl AsFd for ReadOnlyCard {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for ReadOnlyCard {}
impl drm::control::Device for ReadOnlyCard {}

fn is_text_vt(path: &Path) -> bool {
    path.parent() == Some(Path::new("/dev"))
        && path.file_name().is_some_and(|name| {
            name.to_str().is_some_and(|name| {
                name.strip_prefix("tty")
                    .and_then(|number| number.parse::<u8>().ok())
                    .is_some_and(|number| (1..=63).contains(&number))
            })
        })
}

fn require_text_vt() -> io::Result<()> {
    let stdin = io::stdin();
    let path = std::fs::read_link("/proc/self/fd/0")?;
    if !stdin.is_terminal() || !is_text_vt(&path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native probe requires a separate Linux text login/VT (/dev/ttyN), not X11, Wayland, SSH, or a terminal emulator",
        ));
    }
    Ok(())
}

fn validate_switch_vt(vt: i32) -> io::Result<i32> {
    require_text_vt()?;
    let source = std::fs::read_link("/proc/self/fd/0")?;
    let source_vt: i32 = source
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("tty"))
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| io::Error::other("cannot determine current text VT"))?;
    if !(1..=63).contains(&vt) || vt == source_vt {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "switch target must be a different VT (1–63)",
        ));
    }
    Ok(source_vt)
}

fn inspect_card(card: &ReadOnlyCard, path: &Path) -> io::Result<()> {
    let resources = card.resource_handles()?;
    println!(
        "{}: {} connector(s)",
        path.display(),
        resources.connectors().len()
    );
    for handle in resources.connectors() {
        // No forced connector reprobe: only query the current kernel state.
        let info = card.get_connector(*handle, false)?;
        println!(
            "  {}-{}: {:?}, {} mode(s)",
            info.interface().as_str(),
            info.interface_id(),
            info.state(),
            info.modes().len()
        );
    }
    Ok(())
}

/// Acquires a libseat session, queries seat GPUs and connectors, and exits.
/// This does not create a Wayland socket or a display output.
pub fn probe() -> Result<(), Box<dyn Error>> {
    require_text_vt()?;

    let (mut session, notifier) = LibSeatSession::new()?;
    let mut event_loop = EventLoop::<()>::try_new()?;
    event_loop.handle().insert_source(notifier, |event, _, _| {
        println!("session: {event:?}");
    })?;

    // libseat may activate asynchronously; bound the wait so this never
    // strands a text login if the seat cannot become active.
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.is_active() && Instant::now() < deadline {
        event_loop.dispatch(Some(Duration::from_millis(100)), &mut ())?;
    }
    if !session.is_active() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "libseat did not activate the session",
        )
        .into());
    }

    let seat = session.seat();
    println!("active seat: {seat}");
    let udev = UdevBackend::new(&seat)?;
    let mut devices: Vec<_> = udev
        .device_list()
        .map(|(id, path)| (id, path.to_path_buf()))
        .collect();
    devices.sort_by(|a, b| a.1.cmp(&b.1));
    println!("DRM cards on {seat}: {}", devices.len());
    if devices.is_empty() {
        return Err(
            io::Error::new(io::ErrorKind::NotFound, "no DRM card devices on this seat").into(),
        );
    }
    let mut failures = 0;
    for (id, path) in devices {
        let driver = udev::driver(id)?;
        println!(
            "{}: driver {}",
            path.display(),
            driver.as_deref().unwrap_or_default().to_string_lossy()
        );
        let fd = match session.open(&path, OFlags::RDONLY | OFlags::CLOEXEC) {
            Ok(fd) => fd,
            Err(err) => {
                eprintln!("{}: failed to open via libseat: {err}", path.display());
                failures += 1;
                continue;
            }
        };
        let card = ReadOnlyCard(fd);
        let result = inspect_card(&card, &path);
        // Give libseat the same fd it opened, including on query failures.
        session.close(card.0)?;
        if let Err(err) = result {
            eprintln!("{}: DRM query failed: {err}", path.display());
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(
            io::Error::other(format!("native probe failed for {failures} DRM card(s)")).into(),
        );
    }
    println!("native device probe complete; no modeset or client session started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_real_linux_text_vts() {
        assert!(is_text_vt(Path::new("/dev/tty1")));
        assert!(is_text_vt(Path::new("/dev/tty63")));
        for path in [
            "/dev/tty",
            "/dev/tty0",
            "/dev/tty64",
            "/dev/pts/1",
            "/tmp/tty1",
            "/dev/tty1/other",
        ] {
            assert!(!is_text_vt(Path::new(path)), "{path}");
        }
    }
}
