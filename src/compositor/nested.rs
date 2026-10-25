//! Nested winit output: works as a window on an X11/i3 desktop.

use std::{
    ffi::CStr,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        SwapBuffersError,
        renderer::{
            damage::OutputDamageTracker,
            gles::{GlesRenderer, ffi},
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::layer_map_for_output,
    input::pointer::CursorIcon,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    reexports::wayland_server::Resource,
    utils::Transform,
};

use crate::LoopData;

use super::{Compositor, cursor::CursorElements};

pub struct NestedOutput {
    backend: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage_tracker: OutputDamageTracker,
}

impl NestedOutput {
    pub fn request_redraw(&self) {
        self.backend.window().request_redraw();
    }

    fn resized(
        &mut self,
        state: &mut Compositor,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) {
        let mode = Mode {
            size,
            refresh: 60_000,
        };
        self.output
            .change_current_state(Some(mode), None, None, None);
        self.output.set_preferred(mode);
        layer_map_for_output(&self.output).arrange();
        state.update_usable_area(&self.output);
        self.request_redraw();
    }

    fn repaint(&mut self, state: &mut Compositor) -> Result<(), SwapBuffersError> {
        let start = state.perf.as_ref().map(|_| Instant::now());
        // Smithay exposes buffer age only after bind(), but its returned
        // framebuffer borrows the backend so we cannot safely query it here.
        // Zero is required until the backend offers age with the bind result.
        let age = 0;
        let damage = {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            let cursor = state
                .cursor_position
                .map(|position| {
                    let icon = state
                        .column_resize_cursor_at(position)
                        .then_some(CursorIcon::EwResize);
                    state
                        .cursor
                        .elements(renderer, position.to_physical(1.0), 1.0, icon)
                })
                .unwrap_or_default();
            smithay::desktop::space::render_output::<_, CursorElements<GlesRenderer>, _, _>(
                &self.output,
                renderer,
                &mut framebuffer,
                1.0,
                age,
                [&state.space],
                &cursor,
                &mut self.damage_tracker,
                [0.1, 0.1, 0.1, 1.0],
            )
            .map_err(|err| SwapBuffersError::TemporaryFailure(Box::new(err)))?
            .damage
            .cloned()
        };

        let after_render = start.map(|_| Instant::now());

        tracing::debug!(damaged = damage.is_some(), "nested repaint");
        if let Some(ref damage) = damage {
            self.backend.submit(Some(damage))?;
        }
        if let Some(start) = start {
            let pixels = damage
                .as_ref()
                .map(|rects| {
                    rects
                        .iter()
                        .map(|rect| (i64::from(rect.size.w) * i64::from(rect.size.h)).max(0) as u64)
                        .sum()
                })
                .unwrap_or(0);
            let output = self
                .output
                .current_mode()
                .map(|mode| (i64::from(mode.size.w) * i64::from(mode.size.h)).max(0) as u64)
                .unwrap_or(0);
            state.perf.as_mut().expect("performance enabled").frame(
                after_render.expect("render timer").duration_since(start),
                Instant::now().duration_since(after_render.expect("render timer")),
                pixels,
                output,
                damage.is_some(),
            );
        }
        for window in state.space.elements() {
            window.window.send_frame(
                &self.output,
                state.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(self.output.clone()),
            );
        }
        for layer in layer_map_for_output(&self.output).layers() {
            layer.send_frame(
                &self.output,
                state.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(self.output.clone()),
            );
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &state.cursor.image
            && surface.is_alive()
        {
            smithay::desktop::utils::send_frames_surface_tree(
                surface,
                &self.output,
                state.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(self.output.clone()),
            );
        }
        Ok(())
    }
}

pub fn run_nested(
    event_loop: &mut EventLoop<LoopData>,
    data: &mut LoopData,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    backend.window().set_cursor_visible(false);

    let (vendor, renderer, version) = backend.renderer().with_context(|gl| unsafe {
        // SAFETY: the GL context is current for the duration of with_context.
        let value = |name| {
            CStr::from_ptr(gl.GetString(name).cast())
                .to_string_lossy()
                .into_owned()
        };
        (
            value(ffi::VENDOR),
            value(ffi::RENDERER),
            value(ffi::VERSION),
        )
    })?;
    println!("GLES vendor: {vendor}; renderer: {renderer}; version: {version}");
    if renderer.to_ascii_lowercase().contains("llvmpipe")
        || renderer.to_ascii_lowercase().contains("softpipe")
    {
        eprintln!("Warning: software OpenGL renderer detected");
    }

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    let output = Output::new(
        "nested".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "devel-wm".into(),
            model: "winit".into(),
        },
    );
    let _global = output.create_global::<crate::compositor::Compositor>(&data.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    data.compositor.space.map_output(&output, (0, 0));
    data.compositor.register_output(&output);
    let damage_tracker = OutputDamageTracker::from_output(&output);
    data.nested = Some(NestedOutput {
        backend,
        output,
        damage_tracker,
    });
    data.nested
        .as_ref()
        .expect("nested backend")
        .request_redraw();

    event_loop
        .handle()
        .insert_source(winit, move |event, _, data| {
            let state = &mut data.compositor;
            let nested = data.nested.as_mut().expect("nested backend");
            match event {
                WinitEvent::Resized { size, .. } => nested.resized(state, size),
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::CloseRequested => state.loop_signal.stop(),
                WinitEvent::Redraw => {
                    if let Err(err) = nested.repaint(state) {
                        eprintln!("Render failed: {err}");
                        state.loop_signal.stop();
                    }
                }
                _ => {}
            }
        })?;
    Ok(())
}
