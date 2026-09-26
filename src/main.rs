use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use devel_wm::{
    LoopData,
    compositor::{Compositor, run_nested},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut event_loop: EventLoop<LoopData> = EventLoop::try_new()?;
    let display = Display::<Compositor>::new()?;
    let display_handle = display.handle();
    let compositor = Compositor::new(&mut event_loop, display)?;
    let mut data = LoopData {
        compositor,
        display_handle,
        nested: None,
    };

    run_nested(&mut event_loop, &mut data)?;
    println!(
        "WAYLAND_DISPLAY={}",
        data.compositor.socket_name.to_string_lossy()
    );
    println!(
        "To test: env -u DISPLAY WAYLAND_DISPLAY={} alacritty",
        data.compositor.socket_name.to_string_lossy()
    );

    event_loop.run(None, &mut data, |data| {
        let window_count = data.compositor.space.elements().len();
        data.compositor.space.refresh();
        data.compositor.cleanup_windows();
        if data.compositor.space.elements().len() != window_count {
            data.compositor.needs_repaint = true;
        }
        data.compositor.popups.cleanup();
        if let Err(err) = data.display_handle.flush_clients() {
            eprintln!("Failed to flush Wayland clients: {err}");
            data.compositor.loop_signal.stop();
        }
        if data.compositor.needs_repaint {
            data.compositor.needs_repaint = false;
            if let Some(nested) = &data.nested {
                nested.request_redraw();
            }
        }
    })?;
    Ok(())
}
