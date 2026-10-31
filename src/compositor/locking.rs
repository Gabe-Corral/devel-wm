//! Fail-closed ext-session-lock: only the confirmed owner can unlock.

use smithay::{
    delegate_session_lock,
    input::pointer::MotionEvent,
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};

use super::{Compositor, LockSession};

impl Compositor {
    pub(crate) fn is_locked(&self) -> bool {
        self.lock_screen.is_some()
    }

    pub(crate) fn lock_surface_for_output(&self, output: &Output) -> Option<&LockSurface> {
        self.lock_screen
            .as_ref()?
            .surfaces
            .get(output)
            .filter(|surface| surface.alive())
    }

    pub(crate) fn lock_surface_at(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (output, surface) =
            self.lock_screen
                .as_ref()?
                .surfaces
                .iter()
                .find(|(output, surface)| {
                    surface.alive()
                        && self
                            .space
                            .output_geometry(output)
                            .is_some_and(|area| area.to_f64().contains(position))
                })?;
        Some((
            surface.wl_surface().clone(),
            self.space.output_geometry(output)?.loc.to_f64(),
        ))
    }

    pub(crate) fn lock_frame_presented(&mut self, output: &Output) {
        let Some(session) = self.lock_screen.as_mut() else {
            return;
        };
        session.covered.insert(output.clone());
        if self
            .output_monitors
            .keys()
            .all(|output| session.covered.contains(output))
            && let Some(confirmation) = session.confirmation.take()
        {
            confirmation.lock();
        }
    }
}

impl SessionLockHandler for Compositor {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        if self.is_locked() {
            // The forked Smithay handler rejects concurrent lock owners before
            // this point; never replace a pending or confirmed session.
            drop(confirmation);
            return;
        }
        self.lock_screen = Some(LockSession {
            confirmation: Some(confirmation),
            surfaces: Default::default(),
            covered: Default::default(),
        });
        self.ipc_dirty = true;
        self.needs_repaint = true;
        self.floating_drag = None;
        self.floating_resize = None;
        self.column_resize = None;
        self.title_drag = None;
        self.last_forwarded_key_serial = None;
        self.last_forwarded_button_serial = None;
        let serial = SERIAL_COUNTER.next_serial();
        self.seat.get_keyboard().expect("keyboard").unset_grab(self);
        self.seat
            .get_pointer()
            .expect("pointer")
            .unset_grab(self, serial, 0);
        self.popup_grab = None;
        self.intercepted_keys.clear();
        self.seat
            .get_keyboard()
            .expect("keyboard")
            .set_focus(self, None, serial);
        let pointer = self.seat.get_pointer().expect("pointer");
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: pointer.current_location(),
                serial,
                time: 0,
            },
        );
        pointer.frame(self);
        if self.output_monitors.is_empty() {
            // With no active outputs there can be no stale desktop frame.
            if let Some(confirmation) = self
                .lock_screen
                .as_mut()
                .and_then(|s| s.confirmation.take())
            {
                confirmation.lock();
            }
        }
    }

    fn unlock(&mut self) {
        // Only invoked by the patched protocol handler for the *confirmed*
        // active lock. A dead locker can never trigger this path.
        self.lock_screen = None;
        self.ipc_dirty = true;
        self.needs_repaint = true;
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        if let Some(area) = self.space.output_geometry(&output) {
            surface.with_pending_state(|state| {
                state.size = Some((area.size.w as u32, area.size.h as u32).into())
            });
        }
        if let Some(session) = self.lock_screen.as_mut() {
            session.surfaces.insert(output, surface.clone());
            self.needs_repaint = true;
            self.seat.get_keyboard().expect("keyboard").set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
            let pointer = self.seat.get_pointer().expect("pointer");
            let location = pointer.current_location();
            let serial = SERIAL_COUNTER.next_serial();
            let target = self.lock_surface_at(location);
            pointer.motion(
                self,
                target,
                &MotionEvent {
                    location,
                    serial,
                    time: 0,
                },
            );
            pointer.frame(self);
        }
    }
}

delegate_session_lock!(Compositor);
