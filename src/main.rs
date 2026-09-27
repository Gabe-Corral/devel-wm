use std::{ffi::OsStr, io, path::PathBuf, time::Instant};

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use devel_wm::{
    LoopData,
    compositor::{Compositor, run_nested},
    config::Keybindings,
    ipc::IpcServer,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let explicit_config = match args.next() {
        None => None,
        Some(flag) if flag == OsStr::new("--config") => {
            Some(PathBuf::from(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--config requires a TOML file path",
                )
            })?))
        }
        Some(flag) if flag == OsStr::new("--help") => {
            println!("Usage: devel-wm [--config PATH]");
            return Ok(());
        }
        Some(flag) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown option: {}", flag.to_string_lossy()),
            )
            .into());
        }
    };
    if args.next().is_some() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "unexpected extra argument").into(),
        );
    }
    let keybindings = Keybindings::load(explicit_config.as_deref())?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut event_loop: EventLoop<LoopData> = EventLoop::try_new()?;
    let display = Display::<Compositor>::new()?;
    let display_handle = display.handle();
    let compositor = Compositor::new(&mut event_loop, display, keybindings)?;
    let mut data = LoopData {
        compositor,
        display_handle,
        nested: None,
        ipc: None,
    };

    run_nested(&mut event_loop, &mut data)?;
    let ipc = IpcServer::start(&data.compositor.ipc_snapshot())?;
    println!("DEVEL_WM_IPC={}", ipc.path().display());
    data.ipc = Some(ipc);
    println!(
        "WAYLAND_DISPLAY={}",
        data.compositor.socket_name.to_string_lossy()
    );
    println!(
        "To test: env -u DISPLAY WAYLAND_DISPLAY={} alacritty",
        data.compositor.socket_name.to_string_lossy()
    );

    let mut last_fallback_cleanup = Instant::now();
    event_loop.run(None, &mut data, move |data| {
        let measured = data.compositor.perf.is_some();
        let start = measured.then(Instant::now);
        let window_count = data.compositor.space.elements().len();
        data.compositor.space.refresh();
        let after_refresh = measured.then(Instant::now);
        // xdg-shell removal is event-driven. Keep an infrequent fallback for
        // unexpected lifetimes (including surfaces already evicted by Space).
        if data.compositor.space.elements().len() != window_count
            || last_fallback_cleanup.elapsed() >= std::time::Duration::from_secs(5)
        {
            data.compositor.cleanup_windows();
            last_fallback_cleanup = Instant::now();
        }
        let after_cleanup = measured.then(Instant::now);
        if let Some(snapshot) = data.compositor.take_ipc_update()
            && let Some(ipc) = &data.ipc
        {
            ipc.publish(&snapshot);
        }
        let after_ipc = measured.then(Instant::now);
        if data.compositor.space.elements().len() != window_count {
            data.compositor.needs_repaint = true;
        }
        data.compositor.popups.cleanup();
        if let Err(err) = data.display_handle.flush_clients() {
            eprintln!("Failed to flush Wayland clients: {err}");
            data.compositor.loop_signal.stop();
        }
        let redraw = data.compositor.needs_repaint;
        if redraw {
            data.compositor.needs_repaint = false;
            if let Some(nested) = &data.nested {
                nested.request_redraw();
            }
        }
        if let Some(perf) = data.compositor.perf.as_mut() {
            let start = start.expect("performance timer");
            let after_refresh = after_refresh.expect("performance timer");
            let after_cleanup = after_cleanup.expect("performance timer");
            let after_ipc = after_ipc.expect("performance timer");
            perf.event(
                after_refresh.duration_since(start),
                after_cleanup.duration_since(after_refresh),
                after_ipc.duration_since(after_cleanup),
                redraw,
            );
        }
    })?;
    Ok(())
}
