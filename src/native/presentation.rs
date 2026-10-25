//! Bounded, explicitly requested, single-output GPU presentation test.

use std::{
    cell::RefCell,
    error::Error,
    ffi::CStr,
    fs::File,
    io,
    ops::{Deref, DerefMut},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, GbmBufferedSurface},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            Bind, Frame, Renderer,
            gles::{GlesRenderer, ffi},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::all_gpus,
    },
    reexports::{
        calloop::{EventLoop, LoopHandle, RegistrationToken},
        drm::control::{self, Device as _, ModeTypeFlags, connector, crtc},
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Rectangle, Transform},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

use super::restore::AtomicRestore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CleanupReport {
    NotModified,
    Restored,
    SkippedInactive,
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
enum CleanupAction {
    NoChanges,
    Restore,
    SkipInactive,
}

fn cleanup_action(session_active: bool, device_active: bool, snapshot: bool) -> CleanupAction {
    if !session_active || !device_active {
        CleanupAction::SkipInactive
    } else if snapshot {
        CleanupAction::Restore
    } else {
        CleanupAction::NoChanges
    }
}

fn combine_results(results: impl IntoIterator<Item = Result<()>>) -> Result<()> {
    let errors: Vec<_> = results
        .into_iter()
        .filter_map(|result| result.err().map(|err| err.to_string()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")).into())
    }
}

pub(super) struct ManagedDrm {
    pub(super) device: DrmDevice,
    pub(super) session: LibSeatSession,
    pub(super) saved: Option<AtomicRestore>,
    pub(super) additional_saved: Vec<AtomicRestore>,
    pub(super) report: Rc<RefCell<CleanupReport>>,
}

impl ManagedDrm {
    /// An unplugged connector cannot be restored to its pre-takeover scanout.
    /// Forget only its snapshot, leaving surviving CRTCs' snapshots intact.
    pub fn forget_connector(&mut self, connector: connector::Handle) {
        if self
            .saved
            .as_ref()
            .is_some_and(|saved| saved.connector() == connector)
        {
            self.saved.take();
        }
        self.additional_saved
            .retain(|saved| saved.connector() != connector);
    }

    pub fn save_connector(&mut self, saved: AtomicRestore) {
        if self.saved.is_none() {
            self.saved = Some(saved);
        } else {
            self.additional_saved.push(saved);
        }
    }
}

impl Deref for ManagedDrm {
    type Target = DrmDevice;
    fn deref(&self) -> &DrmDevice {
        &self.device
    }
}

impl DerefMut for ManagedDrm {
    fn deref_mut(&mut self) -> &mut DrmDevice {
        &mut self.device
    }
}

impl Drop for ManagedDrm {
    fn drop(&mut self) {
        let report = match cleanup_action(
            self.session.is_active(),
            self.device.is_active(),
            self.saved.is_some() || !self.additional_saved.is_empty(),
        ) {
            CleanupAction::Restore => {
                let errors: Vec<_> = self
                    .saved
                    .iter()
                    .chain(&self.additional_saved)
                    .filter_map(|saved| saved.restore().err().map(|err| err.to_string()))
                    .collect();
                if errors.is_empty() {
                    CleanupReport::Restored
                } else {
                    let message = errors.join("; ");
                    eprintln!("Native display cleanup failed: {message}");
                    CleanupReport::Failed(message)
                }
            }
            CleanupAction::SkipInactive => {
                println!(
                    "Native restore skipped: session/device inactive; no cleanup modeset issued"
                );
                CleanupReport::SkippedInactive
            }
            CleanupAction::NoChanges => CleanupReport::NotModified,
        };
        // We explicitly restored (or reported failure), or never changed the
        // hardware. Suppress Smithay 0.7's unchecked card-wide destructor replay.
        self.device.pause();
        *self.report.borrow_mut() = report;
    }
}

#[derive(Default)]
struct Events {
    paused: bool,
    presented: bool,
    error: Option<String>,
}

// Removing the DRM event source releases its device reference before the
// DrmDevice is dropped. ManagedDrm performs the explicit scoped restoration.
struct DrmSource {
    handle: LoopHandle<'static, Events>,
    token: RegistrationToken,
}

impl Drop for DrmSource {
    fn drop(&mut self) {
        self.handle.remove(self.token);
    }
}

pub(super) struct StopSignals {
    pub(super) stop: Arc<AtomicBool>,
    ids: Vec<signal_hook::SigId>,
}

impl StopSignals {
    pub(super) fn new() -> io::Result<Self> {
        let mut signals = Self {
            stop: Arc::new(AtomicBool::new(false)),
            ids: Vec::new(),
        };
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signals
                .ids
                .push(signal_hook::flag::register(signal, signals.stop.clone())?);
        }
        Ok(signals)
    }
}

impl Drop for StopSignals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

// Linux console ioctls from <linux/kd.h>. Restore even on early returns.
pub(super) struct GraphicsVt {
    tty: File,
    previous_mode: Option<libc::c_int>,
}

impl GraphicsVt {
    pub(super) fn enter() -> io::Result<Self> {
        let tty = File::options().read(true).write(true).open("/dev/tty")?;
        let mut previous_mode = 0;
        // SAFETY: this is a console fd; KDGETMODE writes a single c_int.
        if unsafe { libc::ioctl(tty.as_raw_fd(), 0x4b3b, &mut previous_mode) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: KDSETMODE takes KD_GRAPHICS (1) as an integer argument.
        if unsafe { libc::ioctl(tty.as_raw_fd(), 0x4b3a, 1 as libc::c_int) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            tty,
            previous_mode: Some(previous_mode),
        })
    }

    pub(super) fn restore(&mut self) -> io::Result<()> {
        if let Some(mode) = self.previous_mode {
            // SAFETY: the owned console fd and saved KDGETMODE value are valid.
            if unsafe { libc::ioctl(self.tty.as_raw_fd(), 0x4b3a, mode) } < 0 {
                return Err(io::Error::last_os_error());
            }
            self.previous_mode = None;
            println!("Native VT console mode restored");
        }
        Ok(())
    }
}

impl Drop for GraphicsVt {
    fn drop(&mut self) {
        if let Err(err) = self.restore() {
            eprintln!("Failed to restore VT console mode: {err}");
        }
    }
}

pub(super) fn preferred_mode(modes: &[control::Mode]) -> io::Result<control::Mode> {
    modes
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "target connector has no modes"))
}

pub(super) fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// Select one seat GPU with connected, mode-bearing outputs. Never silently
/// modeset a second card; multi-GPU output ownership needs an explicit policy.
pub(super) fn find_connected_card(
    session: &mut LibSeatSession,
) -> Result<(PathBuf, Vec<connector::Handle>)> {
    let mut selected = None;
    for path in all_gpus(session.seat())? {
        let card = super::ReadOnlyCard(session.open(&path, OFlags::RDONLY | OFlags::CLOEXEC)?);
        let result = (|| -> io::Result<Vec<connector::Handle>> {
            let mut connected = Vec::new();
            for handle in card.resource_handles()?.connectors() {
                let info = card.get_connector(*handle, false)?;
                if info.state() == connector::State::Connected && !info.modes().is_empty() {
                    connected.push(*handle);
                }
            }
            connected.sort_by_key(|handle| u32::from(*handle));
            Ok(connected)
        })();
        session.close(card.0)?;
        let connected = result?;
        if !connected.is_empty() {
            if selected.is_some() {
                return Err(io::Error::other("connected outputs span multiple DRM cards; use named connectors until multi-GPU support exists").into());
            }
            selected = Some((path, connected));
        }
    }
    selected.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no connected outputs with modes on this seat; no native session started",
        )
        .into()
    })
}

// Query cards without constructing a DrmDevice (which can change device state).
pub(super) fn find_target(
    session: &mut LibSeatSession,
    name: &str,
) -> Result<(PathBuf, connector::Handle)> {
    let mut found = None;
    for path in all_gpus(session.seat())? {
        let card = super::ReadOnlyCard(session.open(&path, OFlags::RDONLY | OFlags::CLOEXEC)?);
        let result = (|| -> io::Result<Vec<connector::Handle>> {
            let mut matches = Vec::new();
            for handle in card.resource_handles()?.connectors() {
                let info = card.get_connector(*handle, false)?;
                if connector_name(&info).eq_ignore_ascii_case(name)
                    && info.state() == connector::State::Connected
                {
                    matches.push(*handle);
                }
            }
            Ok(matches)
        })();
        session.close(card.0)?;
        for handle in result? {
            if found.is_some() {
                return Err(io::Error::other(
                    "connector name is ambiguous across cards; refusing to modeset",
                )
                .into());
            }
            found = Some((path.clone(), handle));
        }
    }
    found.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("connected connector {name} not found; no fallback output will be used"),
        )
        .into()
    })
}

pub(super) fn choose_crtc(
    device: &DrmDevice,
    target: &connector::Info,
) -> io::Result<crtc::Handle> {
    choose_crtc_excluding(device, target, &[])
}

pub(super) fn choose_crtc_excluding(
    device: &DrmDevice,
    target: &connector::Info,
    reserved: &[crtc::Handle],
) -> io::Result<crtc::Handle> {
    let resources = device.resource_handles()?;
    let mut occupied_by_others = Vec::new();
    for handle in resources.connectors() {
        if *handle != target.handle() {
            let info = device.get_connector(*handle, false)?;
            if let Some(encoder) = info.current_encoder()
                && let Some(crtc) = device.get_encoder(encoder)?.crtc()
            {
                occupied_by_others.push(crtc);
            }
        }
    }
    let mut candidates = Vec::new();
    if let Some(encoder) = target.current_encoder()
        && let Some(crtc) = device.get_encoder(encoder)?.crtc()
    {
        candidates.push(crtc);
    }
    for encoder in target.encoders() {
        candidates.extend(resources.filter_crtcs(device.get_encoder(*encoder)?.possible_crtcs()));
    }
    occupied_by_others.extend_from_slice(reserved);
    unshared_crtc(candidates, &occupied_by_others).ok_or_else(|| {
        io::Error::other(
            "no compatible unshared CRTC for target; refusing to take another output's CRTC",
        )
    })
}

fn unshared_crtc(
    candidates: impl IntoIterator<Item = crtc::Handle>,
    occupied: &[crtc::Handle],
) -> Option<crtc::Handle> {
    candidates.into_iter().find(|crtc| !occupied.contains(crtc))
}

#[allow(clippy::too_many_arguments)]
fn run_display(
    fd: DeviceFd,
    path: &Path,
    connector: connector::Handle,
    event_loop: &mut EventLoop<'static, Events>,
    events: &mut Events,
    signals: &StopSignals,
    session: &LibSeatSession,
    switch_vt: Option<i32>,
) -> Result<()> {
    if events.paused || signals.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut vt = GraphicsVt::enter()?;
    let report = Rc::new(RefCell::new(CleanupReport::NotModified));
    let result = run_frame(
        fd,
        path,
        connector,
        event_loop,
        events,
        signals,
        session,
        report.clone(),
        switch_vt,
    );
    // run_frame's rendering resources and ManagedDrm have now been dropped.
    let console = vt.restore();
    let display = match &*report.borrow() {
        CleanupReport::Failed(err) => Err(io::Error::other(err.clone()).into()),
        _ => Ok(()),
    };
    combine_results([
        result,
        display,
        console.map_err(|err| {
            io::Error::other(format!("VT console restoration failed: {err}")).into()
        }),
    ])
}

#[allow(clippy::too_many_arguments)]
fn run_frame(
    fd: DeviceFd,
    path: &Path,
    connector: connector::Handle,
    event_loop: &mut EventLoop<'static, Events>,
    events: &mut Events,
    signals: &StopSignals,
    session: &LibSeatSession,
    report: Rc<RefCell<CleanupReport>>,
    switch_vt: Option<i32>,
) -> Result<()> {
    let fd = DrmDeviceFd::new(fd);
    let (device, notifier) = DrmDevice::new(fd.clone(), false)?;
    let mut drm = ManagedDrm {
        device,
        session: session.clone(),
        saved: None,
        additional_saved: Vec::new(),
        report,
    };
    if !drm.is_atomic() {
        return Err(
            io::Error::other("checked native restoration currently requires atomic DRM").into(),
        );
    }
    let info = drm.get_connector(connector, false)?;
    if info.state() != connector::State::Connected {
        return Err(io::Error::other("target disconnected before presentation").into());
    }
    let mode = preferred_mode(info.modes())?;
    let crtc = choose_crtc(&drm, &info)?;
    println!(
        "Native target: {} on {}, CRTC {:?}, mode {:?}",
        connector_name(&info),
        path.display(),
        crtc,
        mode
    );
    let token =
        event_loop
            .handle()
            .insert_source(notifier, move |event, _, events| match event {
                DrmEvent::VBlank(handle) if handle == crtc => events.presented = true,
                DrmEvent::Error(err) => events.error = Some(err.to_string()),
                _ => {}
            })?;
    let _source = DrmSource {
        handle: event_loop.handle(),
        token,
    };
    let gbm = GbmDevice::new(fd)?;
    // SAFETY: EGLDisplay retains a clone of the owned GBM device for its lifetime.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_device = EGLDevice::device_for_display(&display)?;
    if egl_device.is_software() {
        return Err(
            io::Error::other("native EGL device is software; refusing presentation").into(),
        );
    }
    println!(
        "Native EGL render node: {:?}",
        egl_device.try_get_render_node()?
    );
    let context = EGLContext::new(&display)?;
    // SAFETY: the context is owned by the new renderer and used on this thread.
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let (vendor, name, version) = renderer.with_context(|gl| unsafe {
        // SAFETY: with_context makes the GLES context current for these queries.
        let value = |key| {
            CStr::from_ptr(gl.GetString(key).cast())
                .to_string_lossy()
                .into_owned()
        };
        (
            value(ffi::VENDOR),
            value(ffi::RENDERER),
            value(ffi::VERSION),
        )
    })?;
    println!("Native GLES vendor: {vendor}; renderer: {name}; version: {version}");
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|software| name.to_ascii_lowercase().contains(software))
    {
        return Err(
            io::Error::other("software native GLES renderer; refusing presentation").into(),
        );
    }

    let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let surface = drm.create_surface(crtc, mode, &[connector])?;
    match AtomicRestore::capture(drm.device_fd(), connector, crtc, surface.plane()) {
        Ok(saved) => drm.saved = Some(saved),
        Err(err) => {
            // create_surface has not committed yet. Prevent its destructor from
            // clearing the unchanged target if we cannot validate its inverse.
            drm.pause();
            return Err(io::Error::other(format!(
                "native restore preflight failed; refusing presentation: {err}"
            ))
            .into());
        }
    }
    let mut surface = GbmBufferedSurface::<_, ()>::new(
        surface,
        allocator,
        &[Fourcc::Argb8888, Fourcc::Abgr8888],
        renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied(),
    )?;
    let (mut buffer, _) = surface.next_buffer()?;
    let size = (i32::from(mode.size().0), i32::from(mode.size().1)).into();
    let sync = {
        let mut framebuffer = renderer.bind(&mut buffer)?;
        let mut frame = renderer.render(&mut framebuffer, size, Transform::Normal)?;
        frame.clear(
            [0.05, 0.15, 0.35, 1.0].into(),
            &[Rectangle::from_size(size)],
        )?;
        // A contrasting center rectangle makes successful rendering obvious.
        frame.clear(
            [0.1, 0.8, 0.6, 1.0].into(),
            &[Rectangle::new(
                (size.w / 4, size.h / 4).into(),
                (size.w / 2, size.h / 2).into(),
            )],
        )?;
        frame.finish()?
    };

    // Drain pending seat events before the first commit.
    event_loop.dispatch(Some(Duration::ZERO), events)?;
    if events.paused || signals.stop.load(Ordering::Relaxed) {
        if events.paused {
            drm.pause();
        }
        return Ok(());
    }
    surface.queue_buffer(Some(sync), None, ())?;
    let submitted_at = Instant::now();
    let mut presented_at = None;
    let mut switch_requested = false;
    loop {
        event_loop.dispatch(Some(Duration::from_millis(50)), events)?;
        if events.paused {
            // Never try to restore or modeset after handing this seat to i3.
            drm.pause();
            println!("Session paused; ending native display test");
            return Ok(());
        }
        if let Some(error) = events.error.take() {
            return Err(io::Error::other(error).into());
        }
        if events.presented && presented_at.is_none() {
            surface.frame_submitted()?;
            presented_at = Some(Instant::now());
            println!(
                "Native frame presented; holding for five seconds (SIGINT/SIGTERM to exit early)"
            );
        }
        if let (Some(vt), Some(presented)) = (switch_vt, presented_at)
            && !switch_requested
            && presented.elapsed() >= Duration::from_secs(1)
            && !signals.stop.load(Ordering::Relaxed)
        {
            println!("Requesting VT switch to tty{vt} via libseat");
            session.clone().change_vt(vt)?;
            switch_requested = true;
        }
        if switch_requested
            && presented_at.is_some_and(|time| time.elapsed() >= Duration::from_secs(4))
        {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "libseat VT switch requested but no session pause was observed within three seconds").into());
        }
        if presented_at.is_some() && signals.stop.load(Ordering::Relaxed) {
            println!("Native test exit requested by signal");
            break;
        }
        if presented_at.is_some_and(|time| time.elapsed() >= Duration::from_secs(5)) {
            println!("Native test five-second hold completed");
            break;
        }
        if presented_at.is_none() && submitted_at.elapsed() >= Duration::from_secs(3) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no DRM page-flip completion within three seconds",
            )
            .into());
        }
    }
    Ok(())
}

/// Explicit modesetting smoke test, not yet a Wayland compositor session.
pub fn present(connector: &str) -> Result<()> {
    present_with_switch(connector, None)
}

/// Explicit presentation followed by a libseat VT switch to an existing session.
pub fn present_switch_test(connector: &str, vt: i32) -> Result<()> {
    present_with_switch(connector, Some(vt))
}

fn present_with_switch(connector: &str, switch_vt: Option<i32>) -> Result<()> {
    super::require_text_vt()?;
    if let Some(vt) = switch_vt {
        let source_vt = super::validate_switch_vt(vt)?;
        println!(
            "VT-switch test: tty{source_vt} -> tty{vt}; ensure tty{vt} hosts the intended graphical session"
        );
    }
    let signals = StopSignals::new()?;
    let (mut session, notifier) = LibSeatSession::new()?;
    let mut event_loop = EventLoop::<Events>::try_new()?;
    event_loop
        .handle()
        .insert_source(notifier, |event, _, events| {
            if matches!(event, SessionEvent::PauseSession) {
                events.paused = true;
            }
        })?;
    let mut events = Events::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !session.is_active() && Instant::now() < deadline && !signals.stop.load(Ordering::Relaxed)
    {
        event_loop.dispatch(Some(Duration::from_millis(100)), &mut events)?;
    }
    if signals.stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    if !session.is_active() || events.paused {
        return Err(io::Error::other("libseat session is not active").into());
    }
    let (path, connector) = find_target(&mut session, connector)?;
    let fd = session.open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NONBLOCK)?;
    let fd = DeviceFd::from(fd);
    let result = run_display(
        fd.clone(),
        &path,
        connector,
        &mut event_loop,
        &mut events,
        &signals,
        &session,
        switch_vt,
    );
    // All DRM/GBM/EGL references must be gone before returning the original fd
    // to libseat. Close even when initialization, rendering, or flipping fails.
    let owned = fd
        .try_into()
        .map_err(|_| io::Error::other("native display cleanup retained a device reference"))?;
    let close = session.close(owned);
    combine_results([
        result,
        close.map_err(|err| io::Error::other(format!("libseat device close failed: {err}")).into()),
    ])?;
    println!("Native presentation test ended; cleanup completed; no client session started");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_selects_another_outputs_crtc() {
        let first = control::from_u32::<crtc::Handle>(1).unwrap();
        let second = control::from_u32::<crtc::Handle>(2).unwrap();
        assert_eq!(unshared_crtc([first, second], &[]), Some(first));
        assert_eq!(unshared_crtc([first, second], &[first]), Some(second));
        assert_eq!(unshared_crtc([first], &[first]), None);
        assert_eq!(unshared_crtc([], &[]), None);
    }

    #[test]
    fn empty_modes_fail_instead_of_panicking() {
        assert!(preferred_mode(&[]).is_err());
    }

    #[test]
    fn cleanup_never_restores_a_paused_device_or_inactive_session() {
        assert_eq!(cleanup_action(true, true, true), CleanupAction::Restore);
        assert_eq!(cleanup_action(true, true, false), CleanupAction::NoChanges);
        for (session, device) in [(false, false), (false, true), (true, false)] {
            assert_eq!(
                cleanup_action(session, device, true),
                CleanupAction::SkipInactive
            );
        }
    }

    #[test]
    fn cleanup_errors_fail_even_after_successful_rendering() {
        let error = || -> Result<()> { Err(io::Error::other("restore failed").into()) };
        assert!(combine_results([Ok(()), error()]).is_err());
        assert!(combine_results([error(), Ok(())]).is_err());
        assert_eq!(
            combine_results([error(), error()]).unwrap_err().to_string(),
            "restore failed; restore failed"
        );
        assert!(combine_results([Ok(()), Ok(())]).is_ok());
    }
}
