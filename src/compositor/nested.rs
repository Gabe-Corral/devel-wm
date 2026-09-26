//! Nested winit output: works as a window on an X11/i3 desktop.

use std::{ffi::CStr, time::Duration};

use smithay::{
    backend::{
        SwapBuffersError,
        renderer::{
            damage::OutputDamageTracker,
            element::surface::WaylandSurfaceRenderElement,
            gles::{GlesRenderer, ffi},
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

use crate::LoopData;

use super::Compositor;

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
        for window in state.space.elements() {
            if let Some(top) = window.toplevel() {
                top.with_pending_state(|pending| pending.size = Some(size.to_logical(1)));
                top.send_pending_configure();
            }
        }
        self.request_redraw();
    }

    fn repaint(&mut self, state: &Compositor) -> Result<(), SwapBuffersError> {
        // Winit does not expose a reliable buffer age before bind here; a zero
        // age correctly redraws the full output when a repaint is requested.
        let age = 0;
        let damage = {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            smithay::desktop::space::render_output::<
                _,
                WaylandSurfaceRenderElement<GlesRenderer>,
                _,
                _,
            >(
                &self.output,
                renderer,
                &mut framebuffer,
                1.0,
                age,
                [&state.space],
                &[],
                &mut self.damage_tracker,
                [0.1, 0.1, 0.1, 1.0],
            )
            .map_err(|err| SwapBuffersError::TemporaryFailure(Box::new(err)))?
            .damage
            .cloned()
        };

        tracing::debug!(damaged = damage.is_some(), "nested repaint");
        if let Some(damage) = damage {
            self.backend.submit(Some(&damage))?;
        }
        for window in state.space.elements() {
            window.send_frame(
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
