//! Smithay integration: Wayland surfaces, outputs, and event loop.

pub(crate) mod decorations;
mod handlers;
mod nested;

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::Arc,
    time::Instant,
};

use smithay::{
    desktop::{
        PopupManager, Space, Window, WindowSurfaceType, find_popup_root_surface,
        layer_map_for_output,
    },
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState, with_states},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::wlr_layer::WlrLayerShellState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use decorations::{DecoratedWindow, insets};
pub use nested::{NestedOutput, run_nested};

use crate::config::Keybindings;
use crate::layout::{self, LayoutSettings, Rect, ResizeSide};
use crate::perf::PerfStats;
use crate::wm::{
    CommandEffect, CommandError, MonitorId, WindowId, WindowOwner, WmCommand, WmState,
};

struct LayerEntry {
    output: smithay::output::Output,
    layer: smithay::desktop::LayerSurface,
    layout_state: smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState,
}

struct FloatingDrag {
    window: WindowId,
    start: Point<f64, Logical>,
    origin: Rect,
}

struct FloatingResize {
    window: WindowId,
    start: Point<f64, Logical>,
    origin: Rect,
    horizontal: ResizeSide,
    vertical: ResizeSide,
}

struct TitleDrag {
    window: Window,
    start: Point<f64, Logical>,
}

fn title_drag_exceeded(start: Point<f64, Logical>, position: Point<f64, Logical>) -> bool {
    let dx = position.x - start.x;
    let dy = position.y - start.y;
    dx * dx + dy * dy >= 16.0
}

/// Keep a small strip reachable rather than constraining the entire window.
/// A tiled window can be nearly as large as the output when dragging starts.
fn drag_position(
    origin: Rect,
    start: Point<f64, Logical>,
    pointer: Point<f64, Logical>,
    area: Rectangle<i32, Logical>,
) -> (i32, i32) {
    let axis = |initial: i32, delta: f64, edge: i32, extent: i32, size: i32, grip: i32| {
        let visible = grip.min(size).min(extent.max(0));
        let min = (i64::from(edge) + i64::from(visible) - i64::from(size)).max(i64::from(i32::MIN));
        let max = (i64::from(edge) + i64::from(extent) - i64::from(visible))
            .min(i64::from(i32::MAX) - i64::from(size))
            .max(min);
        (i64::from(initial) + delta.round() as i64).clamp(min, max) as i32
    };
    (
        axis(
            origin.x,
            pointer.x - start.x,
            area.loc.x,
            area.size.w,
            origin.width,
            64,
        ),
        axis(
            origin.y,
            pointer.y - start.y,
            area.loc.y,
            area.size.h,
            origin.height,
            48,
        ),
    )
}

pub struct Compositor {
    pub needs_repaint: bool,
    pub perf: Option<PerfStats>,
    ipc_dirty: bool,
    pub socket_name: OsString,
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub space: Space<DecoratedWindow>,
    pub wm: WmState,
    output_monitors: HashMap<smithay::output::Output, MonitorId>,
    window_ids: HashMap<Window, WindowId>,
    id_windows: HashMap<WindowId, Window>,
    scene_windows: HashMap<WindowId, DecoratedWindow>,
    surface_windows: HashMap<WlSurface, Window>,
    requested_geometry: HashMap<WindowId, Rect>,
    mapped_windows: HashSet<WindowId>,
    activated_window: Option<WindowId>,
    floating_drag: Option<FloatingDrag>,
    floating_resize: Option<FloatingResize>,
    title_drag: Option<TitleDrag>,
    layout_settings: LayoutSettings,
    pub(crate) keybindings: Keybindings,
    pub(crate) intercepted_keys: HashSet<smithay::input::keyboard::Keycode>,
    layers: HashMap<WlSurface, LayerEntry>,
    usable_areas: HashMap<smithay::output::Output, Rectangle<i32, Logical>>,
    pub popups: PopupManager,
    pub loop_signal: smithay::reexports::calloop::LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
}

impl Compositor {
    pub(crate) fn surface_affects_visible_scene(&self, root: &WlSurface) -> bool {
        if let Some(window) = self.surface_windows.get(root) {
            return self
                .window_ids
                .get(window)
                .is_some_and(|&id| self.wm.is_window_visible(id));
        }
        if let Some(entry) = self.layers.get(root) {
            return self.space.outputs().any(|output| output == &entry.output);
        }
        if let Some(popup) = self.popups.find_popup(root)
            && let Ok(parent) = find_popup_root_surface(&popup)
        {
            return self.surface_windows.get(&parent).is_some_and(|window| {
                self.window_ids
                    .get(window)
                    .is_some_and(|&id| self.wm.is_window_visible(id))
            }) || self
                .layers
                .get(&parent)
                .is_some_and(|entry| self.space.outputs().any(|output| output == &entry.output));
        }
        false
    }
    pub fn ipc_snapshot(&self) -> crate::ipc::Snapshot {
        crate::ipc::Snapshot::from_wm(&self.wm, |monitor| {
            self.output_monitors
                .iter()
                .find_map(|(output, &id)| (id == monitor).then(|| output.name()))
                .unwrap_or_default()
        })
    }

    pub fn take_ipc_update(&mut self) -> Option<crate::ipc::Snapshot> {
        if !std::mem::take(&mut self.ipc_dirty) {
            return None;
        }
        Some(self.ipc_snapshot())
    }

    pub fn new(
        event_loop: &mut EventLoop<crate::LoopData>,
        display: Display<Self>,
        keybindings: Keybindings,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let decoration_state =
            smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let mut seat = seat_state.new_wl_seat(&dh, "winit");
        seat.add_keyboard(Default::default(), 200, 25)?;
        seat.add_pointer();

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        event_loop
            .handle()
            .insert_source(socket, |stream, _, data| {
                if let Err(err) = data
                    .display_handle
                    .insert_client(stream, Arc::new(CompositorClient::default()))
                {
                    eprintln!("Failed to accept Wayland client: {err}");
                }
            })?;
        event_loop.handle().insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, data| {
                // SAFETY: the display remains inside this event source for its lifetime.
                unsafe { display.get_mut().dispatch_clients(&mut data.compositor) }
                    .map_err(std::io::Error::other)?;
                Ok(PostAction::Continue)
            },
        )?;

        Ok(Self {
            needs_repaint: false,
            perf: (std::env::var("DEVEL_WM_PERF").as_deref() == Ok("1")).then(PerfStats::default),
            ipc_dirty: true,
            socket_name,
            start_time: Instant::now(),
            display_handle: dh,
            space: Space::default(),
            wm: WmState::new(),
            output_monitors: HashMap::new(),
            window_ids: HashMap::new(),
            id_windows: HashMap::new(),
            scene_windows: HashMap::new(),
            surface_windows: HashMap::new(),
            requested_geometry: HashMap::new(),
            mapped_windows: HashSet::new(),
            activated_window: None,
            floating_drag: None,
            floating_resize: None,
            title_drag: None,
            layout_settings: LayoutSettings::default(),
            keybindings,
            intercepted_keys: HashSet::new(),
            layers: HashMap::new(),
            usable_areas: HashMap::new(),
            popups: PopupManager::default(),
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
            decoration_state,
            layer_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            seat,
        })
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        for output in self.space.outputs() {
            let Some(geometry) = self.space.output_geometry(output) else {
                continue;
            };
            let local = pos - geometry.loc.to_f64();
            let layers = layer_map_for_output(output);
            for level in [
                smithay::wayland::shell::wlr_layer::Layer::Overlay,
                smithay::wayland::shell::wlr_layer::Layer::Top,
            ] {
                if let Some(layer) = layers.layer_under(level, local)
                    && let Some(layer_geometry) = layers.layer_geometry(layer)
                    && let Some((surface, offset)) = layer
                        .surface_under(local - layer_geometry.loc.to_f64(), WindowSurfaceType::ALL)
                {
                    return Some((
                        surface,
                        (offset + layer_geometry.loc + geometry.loc).to_f64(),
                    ));
                }
            }
        }
        if let Some((window, location)) = self.space.element_under(pos) {
            // A server-drawn titlebar consumes input without a wl_surface.
            return window
                .window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, offset)| (surface, (offset + location).to_f64()));
        }
        for output in self.space.outputs() {
            let geometry = self.space.output_geometry(output)?;
            let local = pos - geometry.loc.to_f64();
            let layers = layer_map_for_output(output);
            for level in [
                smithay::wayland::shell::wlr_layer::Layer::Bottom,
                smithay::wayland::shell::wlr_layer::Layer::Background,
            ] {
                if let Some(layer) = layers.layer_under(level, local)
                    && let Some(layer_geometry) = layers.layer_geometry(layer)
                    && let Some((surface, offset)) = layer
                        .surface_under(local - layer_geometry.loc.to_f64(), WindowSurfaceType::ALL)
                {
                    return Some((
                        surface,
                        (offset + layer_geometry.loc + geometry.loc).to_f64(),
                    ));
                }
            }
        }
        None
    }

    pub fn register_output(&mut self, output: &smithay::output::Output) {
        let monitor = self.wm.add_monitor();
        self.output_monitors.insert(output.clone(), monitor);
        self.ipc_dirty = true;
        self.update_usable_area(output);
    }

    /// Relayout only when the usable logical area actually changes.
    pub fn update_usable_area(&mut self, output: &smithay::output::Output) {
        let area = layer_map_for_output(output).non_exclusive_zone();
        if self.usable_areas.insert(output.clone(), area) == Some(area) {
            return;
        }
        self.layout_monitor(self.output_monitors[output]);
        self.needs_repaint = true;
    }

    /// Compute the pure master/stack layout and request new sizes only when
    /// they change. Smithay Space tracks the rendered scene, not WM ordering.
    fn layout_monitor(&mut self, monitor: MonitorId) {
        let Some((output, _)) = self.output_monitors.iter().find(|(_, id)| **id == monitor) else {
            return;
        };
        let Some(area) = self.usable_areas.get(output) else {
            return;
        };
        let Some(active) = self.wm.active_workspace(monitor) else {
            return;
        };
        let tiled = self
            .wm
            .tiled_windows(monitor, active)
            .expect("active workspace");
        let usable = Rect {
            x: area.loc.x,
            y: area.loc.y,
            width: area.size.w,
            height: area.size.h,
        };
        let settings = LayoutSettings {
            master_ratio: self
                .wm
                .master_ratio(monitor, active)
                .expect("active workspace"),
            ..self.layout_settings
        };
        let placements = match layout::master_stack(usable, tiled, settings) {
            Ok(placements) => placements,
            Err(error) => {
                tracing::warn!(?error, "Unable to lay out output");
                return;
            }
        };
        tracing::debug!(
            ?monitor,
            windows = placements.len(),
            "arranging master/stack"
        );
        let mut tiled_moved = false;
        for placement in placements {
            tiled_moved |= self.place_window(placement.window, placement.content);
        }
        let floating = self
            .wm
            .floating_windows(monitor, active)
            .expect("active workspace")
            .to_vec();
        for id in floating {
            if let Some(rect) = self.wm.floating_rect(id) {
                self.place_window(id, rect);
                if tiled_moved && let Some(window) = self.scene_windows.get(&id) {
                    self.space.raise_element(window, false);
                }
            }
        }
    }

    fn place_window(&mut self, id: WindowId, content: Rect) -> bool {
        let Some(window) = self.id_windows.get(&id) else {
            return false;
        };
        let scene = &self.scene_windows[&id];
        let decorated = window.toplevel().is_some_and(|top| {
            let state = top.current_state();
            state.decoration_mode == Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide)
                && !state.states.contains(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen)
        });
        let title = window
            .toplevel()
            .and_then(|top| {
                with_states(top.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok().and_then(|state| state.title.clone()))
                })
            })
            .unwrap_or_default();
        let mode_changed = scene.decorated() != decorated;
        if mode_changed {
            tracing::debug!(?id, decorated, "window decoration mode changed");
        }
        let changed = scene.update(
            decorated,
            title,
            self.wm.keyboard_focus() == Some(id),
            content.width,
            content.height,
        );
        self.needs_repaint |= changed && self.wm.is_window_visible(id);
        let (side, title_height) = insets(decorated);
        let position = (content.x, content.y);
        let previous = self.requested_geometry.insert(id, content);
        let mapped = self.mapped_windows.insert(id)
            || mode_changed
            || previous.is_some_and(|rect| (rect.x, rect.y) != position);
        if mapped {
            self.space.map_element(scene.clone(), position, false);
            self.needs_repaint = true;
        }
        if (mode_changed
            || previous
                .is_none_or(|rect| (rect.width, rect.height) != (content.width, content.height)))
            && let Some(top) = window.toplevel()
        {
            top.with_pending_state(|pending| {
                pending.size = Some(
                    (
                        (content.width - side * 2).max(1),
                        (content.height - title_height - side).max(1),
                    )
                        .into(),
                );
            });
            if top.is_initial_configure_sent() {
                top.send_pending_configure();
            }
            self.needs_repaint = true;
        }
        mapped
    }

    pub fn toggle_focused_floating(
        &mut self,
        monitor: MonitorId,
    ) -> Result<CommandEffect, CommandError> {
        let window = self
            .wm
            .active_workspace(monitor)
            .and_then(|workspace| self.wm.focused_window(monitor, workspace))
            .ok_or(CommandError::NoFocusedWindow)?;
        let rect = self
            .requested_geometry
            .get(&window)
            .copied()
            .ok_or(CommandError::InvalidGeometry)?;
        self.apply_command(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: rect,
        })
    }

    pub fn begin_floating_drag(&mut self, window: &Window, start: Point<f64, Logical>) -> bool {
        let Some(&id) = self.window_ids.get(window) else {
            return false;
        };
        let Some(owner) = self.wm.owner(id) else {
            return false;
        };
        if !self.wm.is_window_visible(id) {
            return false;
        }
        if self.wm.floating_rect(id).is_none() {
            let Some(rect) = self.requested_geometry.get(&id).copied() else {
                return false;
            };
            if self
                .apply_command(WmCommand::FloatFocusedAt {
                    monitor: owner.monitor,
                    rect,
                })
                .is_err()
            {
                return false;
            }
        }
        let origin = self.wm.floating_rect(id).expect("floating window");
        self.space.raise_element(&self.scene_windows[&id], false);
        self.floating_drag = Some(FloatingDrag {
            window: id,
            start,
            origin,
        });
        true
    }

    pub fn is_floating_drag_active(&self) -> bool {
        self.floating_drag.is_some()
    }

    pub fn start_title_drag(&mut self, window: &Window, start: Point<f64, Logical>) {
        self.title_drag = Some(TitleDrag {
            window: window.clone(),
            start,
        });
    }

    pub fn is_title_drag_pending(&self) -> bool {
        self.title_drag.is_some()
    }

    /// Consume motion over a pressed titlebar; float only after a real drag.
    pub fn move_title_drag(&mut self, position: Point<f64, Logical>) -> bool {
        let Some(pending) = &self.title_drag else {
            return false;
        };
        if title_drag_exceeded(pending.start, position) {
            let pending = self.title_drag.take().expect("pending title drag");
            if self.begin_floating_drag(&pending.window, pending.start) {
                self.move_floating_drag(position);
            }
        }
        true
    }

    pub fn end_title_drag(&mut self) -> bool {
        self.title_drag.take().is_some()
    }

    pub fn move_floating_drag(&mut self, position: Point<f64, Logical>) {
        let Some(drag) = &self.floating_drag else {
            return;
        };
        let Some(owner) = self.wm.owner(drag.window) else {
            return;
        };
        if !self.wm.is_window_visible(drag.window) {
            return;
        }
        let Some((output, _)) = self
            .output_monitors
            .iter()
            .find(|(_, id)| **id == owner.monitor)
        else {
            return;
        };
        let Some(area) = self.usable_areas.get(output) else {
            return;
        };
        let (x, y) = drag_position(drag.origin, drag.start, position, *area);
        let command = WmCommand::MoveFloating {
            monitor: owner.monitor,
            window: drag.window,
            x,
            y,
        };
        let _ = self.apply_command(command);
    }

    pub fn end_floating_drag(&mut self) {
        self.floating_drag = None;
    }

    /// Alt+right-button resize is for already-floating windows only.
    pub fn begin_floating_resize(&mut self, window: &Window, start: Point<f64, Logical>) -> bool {
        let Some(&id) = self.window_ids.get(window) else {
            return false;
        };
        if !self.wm.is_window_visible(id) {
            return false;
        }
        let Some(origin) = self.wm.floating_rect(id) else {
            return false;
        };
        let horizontal = if start.x - f64::from(origin.x) < f64::from(origin.width) / 2.0 {
            ResizeSide::Start
        } else {
            ResizeSide::End
        };
        let vertical = if start.y - f64::from(origin.y) < f64::from(origin.height) / 2.0 {
            ResizeSide::Start
        } else {
            ResizeSide::End
        };
        self.space.raise_element(&self.scene_windows[&id], false);
        self.floating_resize = Some(FloatingResize {
            window: id,
            start,
            origin,
            horizontal,
            vertical,
        });
        true
    }

    pub fn is_floating_resize_active(&self) -> bool {
        self.floating_resize.is_some()
    }

    pub fn move_floating_resize(&mut self, pointer: Point<f64, Logical>) {
        let Some(resize) = &self.floating_resize else {
            return;
        };
        if !self.wm.is_window_visible(resize.window) {
            return;
        }
        let Some(owner) = self.wm.owner(resize.window) else {
            return;
        };
        let rect = layout::resize_floating(
            resize.origin,
            (pointer.x - resize.start.x).round() as i64,
            (pointer.y - resize.start.y).round() as i64,
            resize.horizontal,
            resize.vertical,
        );
        let _ = self.apply_command(WmCommand::ResizeFloating {
            monitor: owner.monitor,
            window: resize.window,
            rect,
        });
    }

    pub fn end_floating_resize(&mut self) {
        self.floating_resize = None;
    }

    pub fn register_window(&mut self, window: &Window) {
        let output = self.space.outputs().next().expect("mapped output");
        let monitor = self.output_monitors[output];
        let workspace = self
            .wm
            .active_workspace(monitor)
            .expect("registered monitor");
        let id = self
            .wm
            .add_window(WindowOwner { monitor, workspace })
            .expect("active workspace exists");
        self.window_ids.insert(window.clone(), id);
        self.id_windows.insert(id, window.clone());
        self.scene_windows
            .insert(id, DecoratedWindow::new(window.clone()));
        if let Some(top) = window.toplevel() {
            self.surface_windows
                .insert(top.wl_surface().clone(), window.clone());
        }
        self.ipc_dirty = true;
        self.layout_monitor(monitor);
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
    }

    /// Apply policy once, then reflect its visibility/focus change in Smithay.
    pub fn apply_command(&mut self, command: WmCommand) -> Result<CommandEffect, CommandError> {
        let effect = self.wm.execute(command)?;
        if effect != CommandEffect::Unchanged
            && !matches!(
                effect,
                CommandEffect::GeometryChanged { .. } | CommandEffect::LayoutChanged { .. }
            )
        {
            self.ipc_dirty = true;
        }
        match effect {
            CommandEffect::Unchanged => {}
            CommandEffect::FocusChanged { .. } => {
                self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
            }
            CommandEffect::TileOrderChanged { monitor } => {
                self.layout_monitor(monitor);
                if matches!(
                    command,
                    WmCommand::ToggleFocusedFloating { .. } | WmCommand::FloatFocusedAt { .. }
                ) && let Some(id) = self
                    .wm
                    .active_workspace(monitor)
                    .and_then(|ws| self.wm.focused_window(monitor, ws))
                    && self.wm.floating_rect(id).is_some()
                    && let Some(window) = self.scene_windows.get(&id)
                {
                    self.space.raise_element(window, false);
                }
            }
            CommandEffect::LayoutChanged { monitor } => self.layout_monitor(monitor),
            CommandEffect::GeometryChanged { window, .. } => {
                if self.wm.is_window_visible(window)
                    && let Some(rect) = self.wm.floating_rect(window)
                {
                    let _ = self.place_window(window, rect);
                }
            }
            CommandEffect::WorkspaceSwitched { monitor }
            | CommandEffect::WindowMoved { monitor, .. } => self.reconcile_monitor(monitor),
        }
        Ok(effect)
    }

    /// Keep hidden windows in the registry, not in Space (the rendered scene).
    fn reconcile_monitor(&mut self, monitor: MonitorId) {
        for (&id, window) in &self.scene_windows {
            if self
                .wm
                .owner(id)
                .is_some_and(|owner| owner.monitor == monitor)
                && !self.wm.is_window_visible(id)
                && self.mapped_windows.remove(&id)
            {
                self.space.unmap_elem(window);
            }
        }
        self.layout_monitor(monitor);
        let serial = SERIAL_COUNTER.next_serial();
        self.sync_keyboard_focus(serial);
        let pointer = self.seat.get_pointer().expect("pointer");
        let location = pointer.current_location();
        let under = self.surface_under(location);
        let time = self.start_time.elapsed().as_millis() as u32;
        pointer.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.needs_repaint = true;
    }

    pub fn focus_window(&mut self, window: Option<&Window>, serial: smithay::utils::Serial) {
        let previous = self.wm.keyboard_focus();
        let monitor = match window {
            Some(window) => {
                let Some(&id) = self.window_ids.get(window) else {
                    return;
                };
                let owner = self.wm.owner(id).expect("registered window");
                if !self.wm.set_focus(owner.monitor, Some(id)) {
                    return;
                }
                owner.monitor
            }
            None => {
                let Some(monitor) = self.wm.active_monitor() else {
                    return;
                };
                self.wm.set_focus(monitor, None);
                monitor
            }
        };
        debug_assert_eq!(self.wm.active_monitor(), Some(monitor));
        if previous != self.wm.keyboard_focus() {
            self.ipc_dirty = true;
            self.sync_keyboard_focus(serial);
        }
    }

    fn sync_keyboard_focus(&mut self, serial: smithay::utils::Serial) {
        let focused_id = self.wm.keyboard_focus();
        let previous_id = self.activated_window;
        let focused = focused_id.and_then(|id| {
            self.id_windows
                .get(&id)
                .and_then(|window| window.toplevel().map(|top| top.wl_surface().clone()))
        });
        if self.activated_window != focused_id {
            if let Some(old) = self.activated_window
                && let Some(window) = self.id_windows.get(&old)
            {
                Self::configure_activation(window, false);
            }
            if let Some(new) = focused_id
                && let Some(window) = self.scene_windows.get(&new)
            {
                self.space.raise_element(window, false);
                Self::configure_activation(&window.window, true);
            }
            self.activated_window = focused_id;
            self.needs_repaint = true;
            for id in [previous_id, focused_id].into_iter().flatten() {
                if let Some(rect) = self.requested_geometry.get(&id).copied() {
                    self.place_window(id, rect);
                }
            }
        }
        let keyboard = self.seat.get_keyboard().expect("keyboard");
        if keyboard.current_focus() != focused {
            keyboard.set_focus(self, focused, serial);
        }
    }

    fn configure_activation(window: &Window, active: bool) {
        if window.set_activated(active)
            && let Some(top) = window.toplevel()
            && top.is_initial_configure_sent()
        {
            top.send_pending_configure();
        }
    }

    /// Reconcile dead windows, including those hidden from Space, with the WM.
    pub fn cleanup_windows(&mut self) {
        let removed: Vec<_> = self
            .window_ids
            .iter()
            .filter(|(window, _)| !window.alive())
            .map(|(window, &id)| (window.clone(), id))
            .collect();
        if removed.is_empty() {
            return;
        }
        let mut affected = HashSet::new();
        for (window, id) in removed {
            affected.extend(self.remove_registered_window(&window, id));
        }
        for monitor in affected {
            self.layout_monitor(monitor);
        }
        self.ipc_dirty = true;
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
    }

    pub(crate) fn unregister_toplevel(&mut self, surface: &WlSurface) {
        let Some(window) = self.surface_windows.get(surface).cloned() else {
            return;
        };
        let id = self.window_ids[&window];
        let monitor = self.remove_registered_window(&window, id);
        self.surface_windows.remove(surface);
        if let Some(monitor) = monitor {
            self.layout_monitor(monitor);
        }
        self.ipc_dirty = true;
        self.needs_repaint = true;
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
    }

    fn remove_registered_window(&mut self, window: &Window, id: WindowId) -> Option<MonitorId> {
        if self
            .floating_drag
            .as_ref()
            .is_some_and(|drag| drag.window == id)
        {
            self.floating_drag = None;
        }
        if self
            .floating_resize
            .as_ref()
            .is_some_and(|resize| resize.window == id)
        {
            self.floating_resize = None;
        }
        if self
            .title_drag
            .as_ref()
            .is_some_and(|drag| &drag.window == window)
        {
            self.title_drag = None;
        }
        self.window_ids.remove(window);
        self.id_windows.remove(&id);
        if let Some(top) = window.toplevel() {
            self.surface_windows.remove(top.wl_surface());
        }
        if let Some(scene) = self.scene_windows.remove(&id) {
            self.space.unmap_elem(&scene);
        }
        self.requested_geometry.remove(&id);
        self.mapped_windows.remove(&id);
        self.wm.remove_window(id).map(|owner| owner.monitor)
    }

    pub(crate) fn refresh_toplevel_decoration(&mut self, surface: &WlSurface) {
        let Some(id) = self
            .surface_windows
            .get(surface)
            .and_then(|window| self.window_ids.get(window))
            .copied()
        else {
            return;
        };
        if let Some(rect) = self.requested_geometry.get(&id).copied() {
            self.place_window(id, rect);
        }
    }
}

#[derive(Default)]
struct CompositorClient {
    compositor_state: CompositorClientState,
}

impl ClientData for CompositorClient {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_keeps_the_grab_offset_even_for_output_sized_windows() {
        let area = Rectangle::new((0, 0).into(), (800, 600).into());
        let origin = Rect {
            x: 10,
            y: 10,
            width: 780,
            height: 580,
        };
        let start = (600.0, 300.0).into();
        assert_eq!(
            drag_position(origin, start, (700.0, 350.0).into(), area),
            (110, 60)
        );
        assert_eq!(
            drag_position(origin, start, (550.0, 250.0).into(), area),
            (-40, -40)
        );
        // Only a reachable strip must remain inside the output.
        assert_eq!(
            drag_position(origin, start, (3000.0, 3000.0).into(), area),
            (736, 552)
        );
    }

    #[test]
    fn title_click_needs_four_pixels_of_motion_to_become_a_drag() {
        let start = (25.0, 30.0).into();
        assert!(!title_drag_exceeded(start, (28.0, 32.0).into()));
        assert!(title_drag_exceeded(start, (29.0, 30.0).into()));
        assert!(title_drag_exceeded(start, (25.0, 26.0).into()));
    }
}
