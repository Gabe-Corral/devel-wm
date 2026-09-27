//! Keyboard and pointer input routing.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent, PointerButtonEvent,
    },
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::SERIAL_COUNTER,
};

use crate::compositor::Compositor;
use crate::config::Action;
use crate::wm::WmCommand;

impl Compositor {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let serial = SERIAL_COUNTER.next_serial();
                let keyboard = self.seat.get_keyboard().expect("keyboard");
                let code = event.key_code();
                let state = event.state();
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
                        let raw = key.raw_latin_sym_or_raw_current_sym();
                        if state == KeyState::Pressed {
                            tracing::debug!(?code, ?modifiers, ?raw, modified = ?key.modified_sym(), "nested key press");
                        }
                        if state == KeyState::Pressed
                            && let Some(action) = raw.and_then(|sym| compositor.keybindings.lookup(modifiers, sym))
                        {
                            compositor.intercepted_keys.insert(code);
                            FilterResult::Intercept(Some(action))
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
                if let Some(Some(action)) = action
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
                        Action::ToggleFloating => None,
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
                let Some(output) = self.space.outputs().next() else {
                    return;
                };
                let Some(geometry) = self.space.output_geometry(output) else {
                    return;
                };
                let pos = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                let pointer = self.seat.get_pointer().expect("pointer");
                let pending_title_drag = self.move_title_drag(pos);
                let dragging = self.is_floating_drag_active();
                let resizing = self.is_floating_resize_active();
                if dragging && !pending_title_drag {
                    self.move_floating_drag(pos);
                }
                if resizing {
                    self.move_floating_resize(pos);
                }
                let under = if dragging || pending_title_drag || resizing {
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
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event, .. } => {
                let pointer = self.seat.get_pointer().expect("pointer");
                let serial = SERIAL_COUNTER.next_serial();
                let end_move = event.button() == Some(MouseButton::Left)
                    && (self.is_floating_drag_active() || self.is_title_drag_pending());
                let end_resize =
                    event.button() == Some(MouseButton::Right) && self.is_floating_resize_active();
                if event.state() == ButtonState::Released && (end_move || end_resize) {
                    self.end_title_drag();
                    self.end_floating_drag();
                    self.end_floating_resize();
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
                {
                    return;
                }
                if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
                    let location = pointer.current_location();
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
                        let alt_drag = left
                            && self
                                .seat
                                .get_keyboard()
                                .expect("keyboard")
                                .modifier_state()
                                .alt
                            && self.begin_floating_drag(&window, location);
                        let alt_resize = event.button() == Some(MouseButton::Right)
                            && self
                                .seat
                                .get_keyboard()
                                .expect("keyboard")
                                .modifier_state()
                                .alt
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
                        self.focus_window(None, serial);
                    }
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
            }
            InputEvent::PointerAxis { event, .. } => {
                if self.is_floating_drag_active() {
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
