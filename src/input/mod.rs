//! Keyboard and pointer input routing.

use std::{io, process::Command};

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
        PointerMotionEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
};

use crate::compositor::Compositor;
use crate::config::Action;
use crate::wm::WmCommand;

// Clamp to the nearest mapped output rather than trapping relative motion on
// whichever output happened to be registered first. Empty gaps remain empty.
fn clamp_to_outputs(
    target: Point<f64, Logical>,
    outputs: impl IntoIterator<Item = Rectangle<i32, Logical>>,
) -> Option<Point<f64, Logical>> {
    outputs
        .into_iter()
        .filter(|rect| rect.size.w > 0 && rect.size.h > 0)
        .map(|rect| {
            let point = Point::from((
                target.x.clamp(
                    f64::from(rect.loc.x),
                    f64::from(rect.loc.x + rect.size.w - 1),
                ),
                target.y.clamp(
                    f64::from(rect.loc.y),
                    f64::from(rect.loc.y + rect.size.h - 1),
                ),
            ));
            let dx = target.x - point.x;
            let dy = target.y - point.y;
            (point, dx * dx + dy * dy)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(point, _)| point)
}

fn clamp_motion_to_outputs(
    current: Point<f64, Logical>,
    target: Point<f64, Logical>,
    outputs: impl IntoIterator<Item = Rectangle<i32, Logical>>,
) -> Option<Point<f64, Logical>> {
    let outputs: Vec<_> = outputs.into_iter().collect();
    if let Some(here) = outputs.iter().find(|rect| rect.to_f64().contains(current)) {
        let right = here.loc.x + here.size.w;
        if target.x >= f64::from(right)
            && let Some(neighbor) = outputs.iter().find(|rect| rect.loc.x == right)
        {
            return clamp_to_outputs(target, [*neighbor]);
        }
        if target.x < f64::from(here.loc.x)
            && let Some(neighbor) = outputs
                .iter()
                .find(|rect| rect.loc.x + rect.size.w == here.loc.x)
        {
            return clamp_to_outputs(target, [*neighbor]);
        }
    }
    clamp_to_outputs(target, outputs)
}

impl Compositor {
    #[cfg(any(feature = "native", test))]
    pub(crate) fn clamp_pointer_to_mapped_outputs(&mut self) {
        let Some(current) = self.cursor_position else {
            return;
        };
        let next = clamp_to_outputs(
            current,
            self.space
                .outputs()
                .filter_map(|output| self.space.output_geometry(output)),
        );
        match next {
            Some(next) if current != next => {
                self.move_pointer_to(next, self.start_time.elapsed().as_millis() as u32)
            }
            None => self.cursor_position = None,
            _ => {}
        }
    }

    fn launch_terminal(&mut self) -> io::Result<()> {
        let (program, args) = self.keybindings.terminal_command();
        let child = Command::new(program)
            .args(args)
            .env_remove("DISPLAY")
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .spawn()?;
        self.terminal_children.push(child);
        Ok(())
    }

    /// Reap terminal processes that have exited without stopping running clients.
    pub fn reap_terminal_children(&mut self) {
        self.terminal_children
            .retain_mut(|child| match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::debug!(pid = child.id(), ?status, "terminal exited");
                    false
                }
                Ok(None) => true,
                Err(error) => {
                    tracing::warn!(pid = child.id(), %error, "cannot reap terminal");
                    true
                }
            });
    }

    pub(crate) fn process_relative_motion(&mut self, delta: Point<f64, Logical>, time: u32) {
        let current = self.cursor_position.or_else(|| {
            let output = self.space.outputs().next()?;
            let geometry = self.space.output_geometry(output)?;
            Some(Point::from((
                f64::from(geometry.loc.x + geometry.size.w / 2),
                f64::from(geometry.loc.y + geometry.size.h / 2),
            )))
        });
        let Some(current) = current else { return };
        let Some(pos) = clamp_motion_to_outputs(
            current,
            Point::from((current.x + delta.x, current.y + delta.y)),
            self.space
                .outputs()
                .filter_map(|output| self.space.output_geometry(output)),
        ) else {
            return;
        };
        self.move_pointer_to(pos, time);
    }

    fn move_pointer_to(&mut self, pos: Point<f64, Logical>, time: u32) {
        let pointer = self.seat.get_pointer().expect("pointer");
        self.needs_repaint |= self.cursor_position != Some(pos);
        self.cursor_position = Some(pos);
        let pending_title_drag = self.move_title_drag(pos);
        let dragging = self.is_floating_drag_active();
        let resizing = self.is_floating_resize_active();
        let column_resizing = self.is_column_resize_active();
        if dragging && !pending_title_drag {
            self.move_floating_drag(pos);
        }
        if resizing {
            self.move_floating_resize(pos);
        }
        if column_resizing {
            self.move_column_resize(pos);
        }
        let under = if dragging || pending_title_drag || resizing || column_resizing {
            None
        } else {
            self.surface_under(pos)
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let keyboard = self.seat.get_keyboard().expect("keyboard");
                let code = event.key_code();
                let state = event.state();
                if state == KeyState::Pressed {
                    self.last_forwarded_key_serial = None;
                }
                let action = keyboard.input::<Option<Action>, _>(
                    self,
                    code,
                    state,
                    serial,
                    event.time_msec(),
                    |compositor, modifiers, key| {
                        if state == KeyState::Released && compositor.intercepted_keys.remove(&code)
                        {
                            return FilterResult::Intercept(None);
                        }
                        // The popup keyboard grab owns input until the client
                        // dismisses it; do not switch workspaces underneath it.
                        if compositor.popup_grab.as_ref().is_some_and(|grab| !grab.has_ended()) {
                            if state == KeyState::Pressed {
                                compositor.last_forwarded_key_serial = Some(serial);
                            }
                            return FilterResult::Forward;
                        }
                        let raw = key.raw_latin_sym_or_raw_current_sym();
                        if state == KeyState::Pressed {
                            tracing::debug!(?code, ?modifiers, ?raw, modified = ?key.modified_sym(), "key press");
                        }
                        if state == KeyState::Pressed
                            && let Some(action) = raw.and_then(|sym| compositor.keybindings.lookup(modifiers, sym))
                        {
                            compositor.intercepted_keys.insert(code);
                            FilterResult::Intercept(Some(action))
                        } else {
                            if state == KeyState::Pressed {
                                compositor.last_forwarded_key_serial = Some(serial);
                            }
                            FilterResult::Forward
                        }
                    },
                );
                if let Some(Some(Action::LaunchTerminal)) = action {
                    if let Err(error) = self.launch_terminal() {
                        tracing::error!(%error, "failed to launch configured terminal");
                        eprintln!("Failed to launch configured terminal: {error}");
                    }
                } else if let Some(Some(Action::CloseFocused)) = action {
                    self.close_focused_window();
                } else if let Some(Some(Action::AdjustMasterRatio(direction))) = action
                    && let Some(monitor) = self.wm.active_monitor()
                    && let Some(workspace) = self.wm.active_workspace(monitor)
                    && self.wm.tiling_mode(monitor, workspace)
                        == Some(crate::wm::TilingMode::SideBySide)
                {
                    match self.resize_focused_column(monitor, direction) {
                        Ok(effect) => tracing::info!(?effect, "column resized"),
                        Err(error) => tracing::warn!(?error, "column resize rejected"),
                    }
                } else if let Some(Some(action)) = action
                    && let Some(monitor) = self.wm.active_monitor()
                {
                    let command = match action {
                        Action::SwitchWorkspace(workspace) => {
                            Some(WmCommand::SwitchWorkspace { monitor, workspace })
                        }
                        Action::MoveFocusedToWorkspace(workspace) => {
                            Some(WmCommand::MoveFocusedToWorkspace { monitor, workspace })
                        }
                        Action::FocusTile(direction) => {
                            Some(WmCommand::FocusTile { monitor, direction })
                        }
                        Action::SwapTile(direction) => {
                            Some(WmCommand::SwapTile { monitor, direction })
                        }
                        Action::PromoteFocusedToMaster => {
                            Some(WmCommand::PromoteFocusedToMaster { monitor })
                        }
                        Action::AdjustMasterRatio(direction) => {
                            Some(WmCommand::AdjustMasterRatio { monitor, direction })
                        }
                        Action::ToggleTilingMode => Some(WmCommand::ToggleTilingMode { monitor }),
                        Action::ToggleFloating => None,
                        Action::LaunchTerminal => None,
                        Action::CloseFocused => None,
                    };
                    if let Some(command) = command {
                        match self.apply_command(command) {
                            Ok(effect) => tracing::info!(?command, ?effect, "WM command applied"),
                            Err(error) => tracing::warn!(?command, ?error, "WM command rejected"),
                        }
                    } else {
                        match self.toggle_focused_floating(monitor) {
                            Ok(effect) => tracing::info!(?effect, "floating toggled"),
                            Err(error) => tracing::warn!(?error, "floating toggle rejected"),
                        }
                    }
                }
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                // Absolute devices lack per-device output mapping for now;
                // use the active output rather than always warping to the first.
                let Some(output) = self
                    .space
                    .outputs()
                    .find(|output| self.monitor_for_output(output) == self.wm.active_monitor())
                    .or_else(|| self.space.outputs().next())
                else {
                    return;
                };
                let Some(geometry) = self.space.output_geometry(output) else {
                    return;
                };
                let pos = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                self.move_pointer_to(pos, event.time_msec());
            }
            InputEvent::PointerMotion { event, .. } => {
                self.process_relative_motion(event.delta(), event.time_msec());
            }
            InputEvent::PointerButton { event, .. } => {
                if event.state() == ButtonState::Pressed {
                    self.last_forwarded_key_serial = None;
                    self.last_forwarded_button_serial = None;
                }
                let pointer = self.seat.get_pointer().expect("pointer");
                let serial = SERIAL_COUNTER.next_serial();
                let end_move = event.button() == Some(MouseButton::Left)
                    && (self.is_floating_drag_active() || self.is_title_drag_pending());
                let end_resize =
                    event.button() == Some(MouseButton::Right) && self.is_floating_resize_active();
                let end_column_resize =
                    event.button() == Some(MouseButton::Left) && self.is_column_resize_active();
                if event.state() == ButtonState::Released
                    && (end_move || end_resize || end_column_resize)
                {
                    self.end_title_drag();
                    if self.end_floating_drag() {
                        self.last_forwarded_button_serial = None;
                        pointer.button(
                            self,
                            &ButtonEvent {
                                button: event.button_code(),
                                state: ButtonState::Released,
                                serial,
                                time: event.time_msec(),
                            },
                        );
                        pointer.frame(self);
                    }
                    self.end_floating_resize();
                    self.end_column_resize();
                    let location = pointer.current_location();
                    let under = self.surface_under(location);
                    pointer.motion(
                        self,
                        under,
                        &MotionEvent {
                            location,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                    return;
                }
                if self.is_floating_drag_active()
                    || self.is_title_drag_pending()
                    || self.is_floating_resize_active()
                    || self.is_column_resize_active()
                {
                    return;
                }
                if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
                    let location = pointer.current_location();
                    let alt = self
                        .seat
                        .get_keyboard()
                        .expect("keyboard")
                        .modifier_state()
                        .alt;
                    if event.button() == Some(MouseButton::Left)
                        && !alt
                        && self.begin_column_resize(location)
                    {
                        pointer.motion(
                            self,
                            None,
                            &MotionEvent {
                                location,
                                serial,
                                time: event.time_msec(),
                            },
                        );
                        pointer.frame(self);
                        return;
                    }
                    let target = self.surface_under(location).map(|(surface, _)| surface);
                    let clicked_window =
                        self.space
                            .element_under(location)
                            .and_then(|(window, origin)| {
                                let title_hit = window.title_hit(location - origin.to_f64());
                                (title_hit
                                    || window
                                        .window
                                        .surface_under(
                                            location - origin.to_f64(),
                                            smithay::desktop::WindowSurfaceType::ALL,
                                        )
                                        .filter(|(surface, _)| Some(surface) == target.as_ref())
                                        .is_some())
                                .then(|| (window.window.clone(), title_hit))
                            });
                    if let Some((window, title_hit)) = clicked_window {
                        self.focus_window(Some(&window), serial);
                        let left = event.button() == Some(MouseButton::Left);
                        let alt_drag = left && alt && self.begin_floating_drag(&window, location);
                        let alt_resize = event.button() == Some(MouseButton::Right)
                            && alt
                            && self.begin_floating_resize(&window, location);
                        if (left && (alt_drag || title_hit)) || alt_resize {
                            if left && title_hit && !alt_drag {
                                self.start_title_drag(&window, location);
                            }
                            pointer.motion(
                                self,
                                None,
                                &MotionEvent {
                                    location,
                                    serial,
                                    time: event.time_msec(),
                                },
                            );
                            pointer.frame(self);
                            return;
                        }
                    } else if target.is_none() {
                        self.activate_monitor_at(location);
                        self.focus_window(None, serial);
                    }
                }
                if event.state() == ButtonState::Pressed {
                    self.last_forwarded_button_serial = Some(serial);
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: event.button_code(),
                        state: event.state(),
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
                if event.state() == ButtonState::Pressed
                    && self
                        .popup_grab
                        .as_ref()
                        .is_some_and(|grab| grab.has_ended())
                {
                    self.popup_grab = None;
                    let location = pointer.current_location();
                    let target = self.surface_under(location).map(|(surface, _)| surface);
                    let clicked =
                        self.space
                            .element_under(location)
                            .and_then(|(window, origin)| {
                                (window.title_hit(location - origin.to_f64())
                                    || window
                                        .window
                                        .surface_under(
                                            location - origin.to_f64(),
                                            smithay::desktop::WindowSurfaceType::ALL,
                                        )
                                        .is_some_and(|(surface, _)| Some(surface) == target))
                                .then(|| window.window.clone())
                            });
                    self.focus_window(clicked.as_ref(), serial);
                }
            }
            InputEvent::PointerAxis { event, .. } => {
                if self.is_floating_drag_active() || self.is_column_resize_active() {
                    return;
                }
                let source = event.source();
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let amount = event
                        .amount(axis)
                        .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                    }
                    if let Some(v120) = event.amount_v120(axis) {
                        frame = frame.v120(axis, v120 as i32);
                    }
                    if source == AxisSource::Finger && event.amount(axis) == Some(0.0) {
                        frame = frame.stop(axis);
                    }
                }
                let pointer = self.seat.get_pointer().expect("pointer");
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_pointer_can_cross_outputs_and_clamps_to_nearest_edge() {
        let outputs = [
            Rectangle::new((0, 0).into(), (3840, 2160).into()),
            Rectangle::new((3840, 0).into(), (1920, 1080).into()),
        ];
        assert_eq!(
            clamp_to_outputs((3839.0, 400.0).into(), outputs),
            Some((3839.0, 400.0).into())
        );
        assert_eq!(
            clamp_to_outputs((3840.0, 400.0).into(), outputs),
            Some((3840.0, 400.0).into())
        );
        assert_eq!(
            clamp_to_outputs((6000.0, 400.0).into(), outputs),
            Some((5759.0, 400.0).into())
        );
        assert_eq!(
            clamp_to_outputs((3840.0, 1500.0).into(), outputs),
            Some((3839.0, 1500.0).into())
        );
        assert_eq!(clamp_to_outputs((30.0, 30.0).into(), []), None);
        assert_eq!(
            clamp_motion_to_outputs((3839.0, 1500.0).into(), (3840.0, 1500.0).into(), outputs),
            Some((3840.0, 1079.0).into())
        );
        assert_eq!(
            clamp_motion_to_outputs((3840.0, 1079.0).into(), (3839.0, 1079.0).into(), outputs),
            Some((3839.0, 1079.0).into())
        );
    }
}
