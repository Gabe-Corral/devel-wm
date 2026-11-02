//! Smithay integration: Wayland surfaces, outputs, and event loop.

pub(crate) mod cursor;
pub(crate) mod decorations;
mod handlers;
mod locking;
mod nested;

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    sync::{Arc, mpsc},
    time::Instant,
};

use smithay::{
    backend::renderer::element::{
        Kind,
        solid::{SolidColorBuffer, SolidColorRenderElement},
    },
    desktop::{
        PopupGrab, PopupManager, Space, Window, WindowSurfaceType, find_popup_root_surface,
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
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Scale},
    wayland::{
        compositor::{CompositorClientState, CompositorState, with_states},
        output::OutputManagerState,
        selection::data_device::{DataDeviceState, clear_data_device_selection},
        session_lock::{LockSurface, SessionLockManagerState, SessionLocker},
        shell::wlr_layer::WlrLayerShellState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};

use cursor::Cursor;
use decorations::{DecoratedWindow, TitleFont, insets};
pub use nested::{NestedOutput, run_nested};

use crate::config::Keybindings;
use crate::layout::{self, LayoutSettings, Rect, ResizeSide};
use crate::perf::PerfStats;
use crate::wm::{
    CommandEffect, CommandError, Direction, MonitorId, TilingMode, WindowId, WindowOwner,
    WmCommand, WmState,
};

struct LayerEntry {
    output: smithay::output::Output,
    layer: smithay::desktop::LayerSurface,
    layout_state: smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState,
}

pub(crate) struct LockSession {
    confirmation: Option<SessionLocker>,
    surfaces: HashMap<smithay::output::Output, LockSurface>,
    covered: HashSet<smithay::output::Output>,
}

struct FloatingDrag {
    window: WindowId,
    start: Point<f64, Logical>,
    origin: Rect,
    /// Client-initiated xdg moves begin after the pointer press was sent to
    /// the client. Forward its eventual release to end the implicit grab.
    forward_release: bool,
}

struct FloatingResize {
    window: WindowId,
    start: Point<f64, Logical>,
    origin: Rect,
    horizontal: ResizeSide,
    vertical: ResizeSide,
}

struct ColumnResize {
    monitor: MonitorId,
    workspace: crate::wm::WorkspaceId,
    kind: DividerResize,
    last_x: f64,
}

#[derive(Clone, Copy)]
enum DividerResize {
    Columns {
        left: WindowId,
        right: WindowId,
    },
    MasterStack {
        start_ratio: u16,
        start_x: f64,
        shared_width: i32,
    },
}

struct TitleDrag {
    window: Window,
    start: Point<f64, Logical>,
    started: bool,
    target: Option<TitleDrop>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TitleDrop {
    Insert(WindowId, bool, DropEdge),
    Group(WindowId),
}

const DROP_INDICATOR_WIDTH: i32 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// The insertion slot is shown across the whole column/stack, not merely on
/// the titlebar or border of the tile under the pointer.
fn drop_indicator_rect(frame: Rect, area: Rect, edge: DropEdge, gap: i32) -> Option<Rect> {
    let vertical = matches!(edge, DropEdge::Left | DropEdge::Right);
    let boundary = match edge {
        DropEdge::Left => frame.x.saturating_sub(gap / 2),
        DropEdge::Right => frame.x.saturating_add(frame.width).saturating_add(gap / 2),
        DropEdge::Top => frame.y.saturating_sub(gap / 2),
        DropEdge::Bottom => frame.y.saturating_add(frame.height).saturating_add(gap / 2),
    };
    let (start, end) = if vertical {
        (area.x, area.x.saturating_add(area.width))
    } else {
        (area.y, area.y.saturating_add(area.height))
    };
    if end <= start {
        return None;
    }
    let thickness = DROP_INDICATOR_WIDTH.min(end - start);
    let position = boundary
        .saturating_sub(thickness / 2)
        .clamp(start, end - thickness);
    Some(if vertical {
        Rect {
            x: position,
            y: area.y,
            width: thickness,
            height: area.height,
        }
    } else {
        Rect {
            x: frame.x,
            y: position,
            width: frame.width,
            height: thickness,
        }
    })
}

fn title_drag_exceeded(start: Point<f64, Logical>, position: Point<f64, Logical>) -> bool {
    let dx = position.x - start.x;
    let dy = position.y - start.y;
    dx * dx + dy * dy >= 16.0
}

fn server_decorated(window: &Window) -> bool {
    window.toplevel().is_some_and(|top| {
        let state = top.current_state();
        state.decoration_mode == Some(smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide)
            && !state.states.contains(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Fullscreen)
    })
}

fn window_title(window: &Window) -> String {
    window
        .toplevel()
        .and_then(|top| {
            with_states(top.wl_surface(), |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok().and_then(|state| state.title.clone()))
            })
        })
        .unwrap_or_default()
}

fn global_usable_area(
    area: Rectangle<i32, Logical>,
    output_origin: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    Rectangle::new(area.loc + output_origin, area.size)
}

fn migration_area(
    geometry: Rectangle<i32, Logical>,
    local_usable: Rectangle<i32, Logical>,
) -> Rect {
    let area = if local_usable.size.w > 0 && local_usable.size.h > 0 {
        global_usable_area(local_usable, geometry.loc)
    } else {
        geometry
    };
    Rect {
        x: area.loc.x,
        y: area.loc.y,
        width: area.size.w,
        height: area.size.h,
    }
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
    // Layout slots can be rooted at a hidden group tab. Never put a slot's
    // frame in requested_geometry until that particular client is configured.
    tile_geometry: HashMap<WindowId, Rect>,
    requested_geometry: HashMap<WindowId, Rect>,
    mapped_windows: HashSet<WindowId>,
    activated_window: Option<WindowId>,
    floating_drag: Option<FloatingDrag>,
    floating_resize: Option<FloatingResize>,
    column_resize: Option<ColumnResize>,
    #[cfg(feature = "native")]
    pub(crate) coalesce_resize_configures: bool,
    #[cfg(feature = "native")]
    deferred_resize_configures: HashSet<WindowId>,
    title_drag: Option<TitleDrag>,
    drop_indicator: SolidColorBuffer,
    layout_settings: LayoutSettings,
    title_font: TitleFont,
    pub(crate) keybindings: Keybindings,
    pub(crate) launched_children: Vec<std::process::Child>,
    pub(crate) menu_child: Option<std::process::Child>,
    pub(crate) menu_close_requested: bool,
    pub(crate) intercepted_keys: HashSet<smithay::input::keyboard::Keycode>,
    pub(crate) last_forwarded_key_serial: Option<smithay::utils::Serial>,
    pub(crate) last_forwarded_button_serial: Option<smithay::utils::Serial>,
    layers: HashMap<WlSurface, LayerEntry>,
    usable_areas: HashMap<smithay::output::Output, Rectangle<i32, Logical>>,
    pub popups: PopupManager,
    pub(crate) popup_grab: Option<PopupGrab<Self>>,
    pub loop_signal: smithay::reexports::calloop::LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub decoration_state: smithay::wayland::shell::xdg::decoration::XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub session_lock_state: SessionLockManagerState,
    pub(crate) lock_screen: Option<LockSession>,
    pub shm_state: ShmState,
    // Clients can render high-DPI buffers into logical-size surfaces instead
    // of exposing their pixel dimensions as the window's logical bbox.
    _viewporter_state: ViewporterState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    #[cfg(feature = "native")]
    pub(crate) dmabuf_state: smithay::wayland::dmabuf::DmabufState,
    clipboard_owner: Option<ClientId>,
    disconnected_clients: mpsc::Receiver<ClientId>,
    pub seat: Seat<Self>,
    pub(crate) cursor: Cursor,
    pub(crate) cursor_position: Option<Point<f64, Logical>>,
    pub(crate) hovered_window: Option<Window>,
}

impl Compositor {
    pub(crate) fn menu_is_open(&self) -> bool {
        self.layers
            .values()
            .any(|entry| entry.layer.namespace() == "devel-menu")
    }

    /// Ask the currently focused xdg toplevel to close. The client decides
    /// when to exit (and can prompt to save work); never destroy it here.
    pub(crate) fn close_focused_window(&self) {
        let Some(id) = self.wm.keyboard_focus() else {
            return;
        };
        if let Some(top) = self.id_windows.get(&id).and_then(Window::toplevel) {
            top.send_close();
        }
    }

    fn clear_disconnected_clipboard(&mut self) {
        let mut disconnected = false;
        for client in self.disconnected_clients.try_iter() {
            disconnected = true;
            if self.clipboard_owner.as_ref() == Some(&client) {
                self.clipboard_owner = None;
                clear_data_device_selection(&self.display_handle, &self.seat);
            }
        }
        if disconnected && self.is_locked() {
            self.needs_repaint = true;
            self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
        }
    }

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
        if self.is_locked() {
            return crate::ipc::Snapshot {
                version: 2,
                active_monitor: None,
                monitors: Vec::new(),
            };
        }
        crate::ipc::Snapshot::from_wm_with_titles(
            &self.wm,
            |monitor| {
                self.output_monitors
                    .iter()
                    .find_map(|(output, &id)| (id == monitor).then(|| output.name()))
                    .unwrap_or_default()
            },
            |id| self.id_windows.get(&id).map(window_title),
        )
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
        seat_name: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let (inner_gap, outer_gap) = keybindings.layout_gaps();
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let decoration_state =
            smithay::wayland::shell::xdg::decoration::XdgDecorationState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        // Do not advertise a lock protocol before its full security workflow
        // has been tested with a real locker on nested and native outputs.
        let experimental_lock =
            std::env::var("DEVEL_WM_EXPERIMENTAL_SESSION_LOCK").as_deref() == Ok("1");
        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&dh, move |_| experimental_lock);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let (disconnect_sender, disconnected_clients) = mpsc::channel();
        let mut seat = seat_state.new_wl_seat(&dh, seat_name);
        seat.add_keyboard(Default::default(), 200, 25)?;
        seat.add_pointer();

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        event_loop
            .handle()
            .insert_source(socket, move |stream, _, data| {
                if let Err(err) = data.display_handle.insert_client(
                    stream,
                    Arc::new(CompositorClient {
                        compositor_state: CompositorClientState::default(),
                        disconnect_sender: disconnect_sender.clone(),
                    }),
                ) {
                    eprintln!("Failed to accept Wayland client: {err}");
                }
            })?;
        event_loop.handle().insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, data| {
                let started = data.compositor.perf.as_ref().map(|_| Instant::now());
                // SAFETY: the display remains inside this event source for its lifetime.
                unsafe { display.get_mut().dispatch_clients(&mut data.compositor) }
                    .map_err(std::io::Error::other)?;
                if let Some(started) = started {
                    data.compositor
                        .perf
                        .as_mut()
                        .expect("performance enabled")
                        .wayland(started.elapsed());
                }
                data.compositor.clear_disconnected_clipboard();
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
            tile_geometry: HashMap::new(),
            requested_geometry: HashMap::new(),
            mapped_windows: HashSet::new(),
            activated_window: None,
            floating_drag: None,
            floating_resize: None,
            column_resize: None,
            #[cfg(feature = "native")]
            coalesce_resize_configures: false,
            #[cfg(feature = "native")]
            deferred_resize_configures: HashSet::new(),
            title_drag: None,
            drop_indicator: SolidColorBuffer::new(
                (1, 1),
                keybindings.decoration_colors().focused_border.solid(),
            ),
            layout_settings: LayoutSettings {
                inner_gap,
                outer_gap,
                ..LayoutSettings::default()
            },
            title_font: TitleFont::load(keybindings.title_font_settings()),
            keybindings,
            launched_children: Vec::new(),
            menu_child: None,
            menu_close_requested: false,
            intercepted_keys: HashSet::new(),
            last_forwarded_key_serial: None,
            last_forwarded_button_serial: None,
            layers: HashMap::new(),
            usable_areas: HashMap::new(),
            popups: PopupManager::default(),
            popup_grab: None,
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
            decoration_state,
            layer_shell_state,
            session_lock_state,
            lock_screen: None,
            shm_state,
            _viewporter_state: viewporter_state,
            output_manager_state,
            seat_state,
            data_device_state,
            #[cfg(feature = "native")]
            dmabuf_state: smithay::wayland::dmabuf::DmabufState::new(),
            clipboard_owner: None,
            disconnected_clients,
            seat,
            cursor: Cursor::new(),
            cursor_position: None,
            hovered_window: None,
        })
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.is_locked() {
            return self.lock_surface_at(pos);
        }
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
            let Some(geometry) = self.space.output_geometry(output) else {
                continue;
            };
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
        self.reattach_output(output, monitor);
    }

    pub(crate) fn reattach_output(&mut self, output: &smithay::output::Output, monitor: MonitorId) {
        if self.output_monitors.is_empty()
            && let Some(geometry) = self.space.output_geometry(output)
        {
            let area = migration_area(geometry, layer_map_for_output(output).non_exclusive_zone());
            for old in self
                .wm
                .monitors()
                .filter(|&old| old != monitor)
                .collect::<Vec<_>>()
            {
                self.wm
                    .evacuate_monitor(old, Some((monitor, area)))
                    .expect("known monitor");
            }
            self.wm.set_active_monitor(monitor);
        }
        self.output_monitors.insert(output.clone(), monitor);
        self.ipc_dirty = true;
        self.update_usable_area(output);
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
    }

    #[cfg(any(feature = "native", test))]
    pub(crate) fn detach_output(&mut self, output: &smithay::output::Output) {
        let Some(&monitor) = self.output_monitors.get(output) else {
            return;
        };
        let survivor = self
            .output_monitors
            .iter()
            .filter(|(candidate, _)| *candidate != output)
            .find_map(|(candidate, &id)| {
                let geometry = self.space.output_geometry(candidate)?;
                let usable = self
                    .usable_areas
                    .get(candidate)
                    .copied()
                    .unwrap_or_else(|| smithay::utils::Rectangle::from_size(geometry.size));
                Some((id, migration_area(geometry, usable)))
            });
        self.wm
            .evacuate_monitor(monitor, survivor)
            .expect("known mapped monitor");
        // A layer surface is bound to its output. Close and unmap it before
        // reconnecting this Output, otherwise its old exclusive zone survives
        // and a restarted bar can reserve the edge twice.
        self.layers.retain(|_, entry| {
            if &entry.output == output {
                layer_map_for_output(output).unmap_layer(&entry.layer);
                entry.layer.layer_surface().send_close();
                false
            } else {
                true
            }
        });
        self.output_monitors.remove(output);
        if let Some(session) = self.lock_screen.as_mut() {
            session.surfaces.remove(output);
            session.covered.remove(output);
        }
        self.usable_areas.remove(output);
        self.space.unmap_output(output);
        if let Some((survivor, _)) = survivor {
            self.layout_monitor(survivor);
        }
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
        self.ipc_dirty = true;
        self.needs_repaint = true;
        self.clamp_pointer_to_mapped_outputs();
    }

    #[cfg(any(feature = "native", test))]
    pub(crate) fn move_output(&mut self, output: &smithay::output::Output, x: i32) {
        let Some(old) = self.space.output_geometry(output) else {
            return;
        };
        if old.loc.x == x {
            return;
        }
        let Some(&monitor) = self.output_monitors.get(output) else {
            return;
        };
        let dx = x - old.loc.x;
        output.change_current_state(None, None, None, Some((x, 0).into()));
        self.space.map_output(output, (x, 0));
        let usable = self
            .usable_areas
            .get(output)
            .copied()
            .unwrap_or_else(|| smithay::utils::Rectangle::from_size(old.size));
        let geometry = self.space.output_geometry(output).expect("moved output");
        self.wm
            .shift_monitor_floats(monitor, dx, 0, migration_area(geometry, usable));
        self.layout_monitor(monitor);
        self.ipc_dirty = true;
        self.needs_repaint = true;
        self.clamp_pointer_to_mapped_outputs();
    }

    pub(crate) fn monitor_at(&self, position: Point<f64, Logical>) -> Option<MonitorId> {
        self.output_monitors.iter().find_map(|(output, &monitor)| {
            self.space
                .output_geometry(output)
                .filter(|geometry| geometry.to_f64().contains(position))
                .map(|_| monitor)
        })
    }

    pub(crate) fn monitor_for_output(&self, output: &smithay::output::Output) -> Option<MonitorId> {
        self.output_monitors.get(output).copied()
    }

    pub(crate) fn activate_monitor_at(&mut self, position: Point<f64, Logical>) {
        if let Some(monitor) = self.monitor_at(position)
            && self.wm.active_monitor() != Some(monitor)
            && self.wm.set_active_monitor(monitor)
        {
            self.ipc_dirty = true;
            self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
        }
    }

    /// Relayout only when the usable logical area actually changes.
    pub fn update_usable_area(&mut self, output: &smithay::output::Output) {
        let area = layer_map_for_output(output).non_exclusive_zone();
        if self.usable_areas.insert(output.clone(), area) == Some(area) {
            return;
        }
        self.layout_monitor(self.output_monitors[output]);
        for entry in self.layers.values().filter(|entry| &entry.output == output) {
            self.refresh_reactive_popups(entry.layer.wl_surface());
        }
        self.needs_repaint = true;
    }

    /// Compute the selected pure tile layout and request new sizes only when
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
            .expect("active workspace")
            .to_vec();
        let origin = self
            .space
            .output_geometry(output)
            .map_or((0, 0).into(), |geometry| geometry.loc);
        let global = global_usable_area(*area, origin);
        let usable = Rect {
            x: global.loc.x,
            y: global.loc.y,
            width: global.size.w,
            height: global.size.h,
        };
        let settings = LayoutSettings {
            master_ratio: self
                .wm
                .master_ratio(monitor, active)
                .expect("active workspace"),
            ..self.layout_settings
        };
        let mode = self
            .wm
            .tiling_mode(monitor, active)
            .expect("active workspace");
        let weights = self
            .wm
            .column_weights(monitor, active)
            .expect("active workspace");
        let placements = match mode {
            TilingMode::MasterStack => layout::master_stack(usable, &tiled, settings),
            TilingMode::SideBySide => {
                layout::side_by_side_weighted(usable, &tiled, &weights, settings)
            }
            TilingMode::Floating => {
                self.wm.clamp_workspace_floats(monitor, active, usable);
                let first_arrangement = tiled.iter().all(|&id| self.wm.floating_rect(id).is_none());
                let count = tiled.len();
                for (index, &id) in tiled.iter().enumerate() {
                    if self.wm.floating_rect(id).is_none() {
                        let rect =
                            layout::floating_placement(usable, index, count, first_arrangement);
                        self.wm.ensure_workspace_float_rect(id, rect);
                    }
                }
                Ok(tiled
                    .iter()
                    .filter_map(|&id| {
                        self.wm.floating_rect(id).map(|frame| layout::Placement {
                            window: id,
                            frame,
                            content: frame,
                        })
                    })
                    .collect())
            }
        };
        let placements = match placements {
            Ok(placements) => placements,
            Err(error) => {
                tracing::warn!(?error, "Unable to lay out output");
                return;
            }
        };
        tracing::debug!(
            ?monitor,
            ?mode,
            windows = placements.len(),
            "arranging tiles"
        );
        for placement in placements {
            // place_window reserves decoration insets *inside* this frame.
            // Using placement.content here applied an extra two-pixel inset
            // around both decorated and undecorated tiled windows.
            let selected = self
                .wm
                .selected_tile(placement.window)
                .expect("layout slot");
            self.tile_geometry.insert(placement.window, placement.frame);
            self.place_window(selected, placement.frame);
        }
        let floating = self
            .wm
            .floating_windows(monitor, active)
            .expect("active workspace")
            .to_vec();
        for id in floating {
            if let Some(rect) = self.wm.floating_rect(id) {
                self.place_window(id, rect);
            }
        }
        for (&id, window) in &self.id_windows {
            if self.wm.is_window_visible(id)
                && self
                    .wm
                    .owner(id)
                    .is_some_and(|owner| owner.monitor == monitor)
                && let Some(surface) = window.toplevel().map(|top| top.wl_surface())
            {
                self.refresh_reactive_popups(surface);
            }
        }
    }

    fn place_window(&mut self, id: WindowId, frame: Rect) {
        // Focus and decoration updates can arrive after a workspace transfer.
        // Never let those paths put a hidden window back into the rendered Space.
        if !self.wm.is_window_visible(id) {
            return;
        }
        let Some(window) = self.id_windows.get(&id) else {
            return;
        };
        let scene = &self.scene_windows[&id];
        if let Some(output) = self
            .wm
            .owner(id)
            .and_then(|owner| {
                self.output_monitors
                    .iter()
                    .find(|(_, monitor)| **monitor == owner.monitor)
            })
            .map(|(output, _)| output)
            && let Some(geometry) = self.space.output_geometry(output)
        {
            scene.set_owner_clip(geometry, (frame.x, frame.y).into());
        }
        let stacking_changed = scene.set_floating(self.wm.floating_rect(id).is_some());
        let tabs = self.wm.group_tabs(id).unwrap_or_default();
        let grouped = tabs.len() > 1;
        let decorated = grouped || server_decorated(window);
        let title = window_title(window);
        let tabs = if grouped {
            tabs.into_iter()
                .filter_map(|tab| {
                    self.id_windows
                        .get(&tab)
                        .map(|window| (window_title(window), tab == id))
                })
                .collect()
        } else {
            Vec::new()
        };
        let tabs_changed = scene.set_tabs(tabs);
        let mode_changed = scene.decorated() != decorated;
        if mode_changed {
            tracing::debug!(?id, decorated, "window decoration mode changed");
        }
        let changed = scene.update(decorated, title, self.wm.keyboard_focus() == Some(id));
        self.needs_repaint |= changed || tabs_changed;
        let (side, title_height) = insets(decorated);
        let position = (frame.x, frame.y);
        let previous = self.requested_geometry.insert(id, frame);
        let mapped = self.mapped_windows.insert(id)
            || stacking_changed
            || mode_changed
            || previous.is_some_and(|rect| (rect.x, rect.y) != position);
        if mapped {
            self.space.map_element(scene.clone(), position, false);
            self.needs_repaint = true;
        }
        if (mode_changed
            || previous.is_none_or(|rect| (rect.width, rect.height) != (frame.width, frame.height)))
            && let Some(top) = window.toplevel()
        {
            top.with_pending_state(|pending| {
                pending.size = Some(
                    (
                        (frame.width - side * 2).max(1),
                        (frame.height - title_height - side).max(1),
                    )
                        .into(),
                );
            });
            if top.is_initial_configure_sent() {
                // Count size updates in both nested and native modes. Native
                // interactive drags may defer/coalesce their configures;
                // nested mode sends each one immediately.
                if let Some(perf) = self.perf.as_mut() {
                    perf.resize_update();
                }
                #[cfg(feature = "native")]
                let defer = self.coalesce_resize_configures
                    && (self
                        .floating_resize
                        .as_ref()
                        .is_some_and(|resize| resize.window == id)
                        || self
                            .column_resize
                            .as_ref()
                            .is_some_and(|resize| match resize.kind {
                                DividerResize::Columns { left, right } => left == id || right == id,
                                DividerResize::MasterStack { .. } => {
                                    self.wm.owner(id).is_some_and(|owner| {
                                        owner.monitor == resize.monitor
                                            && owner.workspace == resize.workspace
                                    })
                                }
                            }));
                #[cfg(not(feature = "native"))]
                let defer = false;
                if defer {
                    #[cfg(feature = "native")]
                    {
                        self.deferred_resize_configures.insert(id);
                    }
                } else {
                    if top.send_pending_configure().is_some()
                        && let Some(perf) = self.perf.as_mut()
                    {
                        perf.resize_configure();
                    }
                }
            }
            self.needs_repaint = true;
        }
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
            forward_release: false,
        });
        true
    }

    pub(crate) fn begin_client_move_drag(
        &mut self,
        window: &Window,
        start: Point<f64, Logical>,
    ) -> bool {
        if !self.begin_floating_drag(window, start) {
            return false;
        }
        self.floating_drag
            .as_mut()
            .expect("started drag")
            .forward_release = true;
        true
    }

    pub fn is_floating_drag_active(&self) -> bool {
        self.floating_drag.is_some()
    }

    pub fn start_title_drag(&mut self, window: &Window, start: Point<f64, Logical>) {
        self.title_drag = Some(TitleDrag {
            window: window.clone(),
            start,
            started: false,
            target: None,
        });
    }

    pub(crate) fn title_tab_at(
        &self,
        window: &Window,
        position: Point<f64, Logical>,
    ) -> Option<Window> {
        let id = *self.window_ids.get(window)?;
        let tabs = self.wm.group_tabs(id)?;
        if tabs.len() < 2 {
            return Some(window.clone());
        }
        let frame = self.requested_geometry.get(&id)?;
        if frame.width <= 0 {
            return None;
        }
        let x = (position.x - f64::from(frame.x)).floor() as i64;
        let index = (x.max(0) as usize * tabs.len() / frame.width as usize).min(tabs.len() - 1);
        self.id_windows.get(&tabs[index]).cloned()
    }

    pub fn is_title_drag_pending(&self) -> bool {
        self.title_drag.is_some()
    }

    /// Keep tiles tiled during an unmodified titlebar drag. Floating windows
    /// retain the usual titlebar move behavior after the movement threshold.
    pub fn move_title_drag(&mut self, position: Point<f64, Logical>) -> bool {
        let Some(pending) = &self.title_drag else {
            return false;
        };
        if !pending.started {
            if !title_drag_exceeded(pending.start, position) {
                return true;
            }
            let Some(id) = self.window_ids.get(&pending.window).copied() else {
                self.title_drag = None;
                return true;
            };
            if self.wm.floating_rect(id).is_some() {
                let pending = self.title_drag.take().expect("pending title drag");
                if self.begin_floating_drag(&pending.window, pending.start) {
                    self.move_floating_drag(position);
                }
                return true;
            }
            self.title_drag
                .as_mut()
                .expect("pending title drag")
                .started = true;
        }
        let id = self.window_ids[&self.title_drag.as_ref().expect("title drag").window];
        let target = self
            .tile_drop_target(id, position)
            .map(|(root, after, edge)| {
                let frame = self.tile_geometry[&root];
                let selected = self.wm.selected_tile(root).expect("visible target slot");
                let decorated = self.wm.group_tabs(root).is_some_and(|tabs| tabs.len() > 1)
                    || self.id_windows.get(&selected).is_some_and(server_decorated);
                if decorated && position.y < f64::from(frame.y + decorations::TITLE_HEIGHT) {
                    TitleDrop::Group(root)
                } else {
                    TitleDrop::Insert(root, after, edge)
                }
            });
        let previous = self.title_drag.as_ref().expect("title drag").target;
        if previous != target {
            self.needs_repaint = true;
            self.title_drag.as_mut().expect("title drag").target = target;
        }
        true
    }

    fn tile_drop_target(
        &self,
        source: WindowId,
        position: Point<f64, Logical>,
    ) -> Option<(WindowId, bool, DropEdge)> {
        let owner = self.wm.owner(source)?;
        if self.monitor_at(position)? != owner.monitor
            || self.wm.active_workspace(owner.monitor)? != owner.workspace
        {
            return None;
        }
        let tiled = self.wm.tiled_windows(owner.monitor, owner.workspace)?;
        let source_root = self.wm.tile_root(source)?;
        if !tiled.contains(&source_root) {
            return None;
        }
        let mode = self.wm.tiling_mode(owner.monitor, owner.workspace)?;
        if mode == TilingMode::Floating {
            return None;
        }
        tiled.iter().enumerate().find_map(|(index, &id)| {
            let rect = self.tile_geometry.get(&id)?;
            if position.x < f64::from(rect.x)
                || position.x >= f64::from(rect.x + rect.width)
                || position.y < f64::from(rect.y)
                || position.y >= f64::from(rect.y + rect.height)
            {
                return None;
            }
            if id == source_root {
                let group = self.wm.group_tabs(source)?;
                if group.len() < 2 || position.y < f64::from(rect.y + decorations::TITLE_HEIGHT) {
                    return None;
                }
            }
            let horizontal = mode == TilingMode::SideBySide || index == 0;
            let after = if horizontal {
                position.x >= f64::from(rect.x) + f64::from(rect.width) / 2.0
            } else {
                position.y >= f64::from(rect.y) + f64::from(rect.height) / 2.0
            };
            let edge = match (horizontal, after) {
                (true, false) => DropEdge::Left,
                (true, true) => DropEdge::Right,
                (false, false) => DropEdge::Top,
                (false, true) => DropEdge::Bottom,
            };
            Some((id, after, edge))
        })
    }

    pub fn end_title_drag(&mut self) -> bool {
        let Some(drag) = self.clear_title_drag() else {
            return false;
        };
        if let Some(target) = drag.target
            && let Some(&window) = self.window_ids.get(&drag.window)
            && let Some(owner) = self.wm.owner(window)
        {
            let command = match target {
                TitleDrop::Insert(target, after, _) => WmCommand::InsertTile {
                    monitor: owner.monitor,
                    window,
                    target,
                    after,
                },
                TitleDrop::Group(target) => WmCommand::GroupTile {
                    monitor: owner.monitor,
                    window,
                    target,
                },
            };
            let _ = self.apply_command(command);
        }
        true
    }

    fn clear_title_drag(&mut self) -> Option<TitleDrag> {
        let drag = self.title_drag.take()?;
        if drag.target.is_some() {
            self.needs_repaint = true;
        }
        Some(drag)
    }

    pub(crate) fn drop_indicator_element(
        &mut self,
        output: &smithay::output::Output,
    ) -> Option<SolidColorRenderElement> {
        let drag = self.title_drag.as_ref()?;
        let target = drag.target?;
        let source = *self.window_ids.get(&drag.window)?;
        let owner = self.wm.owner(source)?;
        if self.output_monitors.get(output) != Some(&owner.monitor)
            || self.wm.active_workspace(owner.monitor) != Some(owner.workspace)
        {
            return None;
        }
        let geometry = self.space.output_geometry(output)?;
        let area = global_usable_area(*self.usable_areas.get(output)?, geometry.loc);
        let area = Rect {
            x: area.loc.x,
            y: area.loc.y,
            width: area.size.w,
            height: area.size.h,
        };
        let root = match target {
            TitleDrop::Insert(root, ..) | TitleDrop::Group(root) => root,
        };
        let frame = *self.tile_geometry.get(&root)?;
        let rect = match target {
            TitleDrop::Insert(_, _, edge) => {
                drop_indicator_rect(frame, area, edge, self.layout_settings.inner_gap)?
            }
            TitleDrop::Group(_) => Rect {
                x: frame.x,
                y: frame.y,
                width: frame.width,
                height: decorations::TITLE_HEIGHT.min(frame.height),
            },
        };
        self.drop_indicator.resize((rect.width, rect.height));
        let scale = output.current_scale().fractional_scale();
        Some(SolidColorRenderElement::from_buffer(
            &self.drop_indicator,
            Point::<i32, Logical>::from((rect.x - geometry.loc.x, rect.y - geometry.loc.y))
                .to_physical_precise_round(Scale::from(scale)),
            Scale::from(scale),
            1.0,
            Kind::Unspecified,
        ))
    }

    pub fn move_floating_drag(&mut self, position: Point<f64, Logical>) {
        let Some(drag) = &self.floating_drag else {
            return;
        };
        let (window, start, origin) = (drag.window, drag.start, drag.origin);
        let Some(owner) = self.wm.owner(window) else {
            return;
        };
        if !self.wm.is_window_visible(window) {
            return;
        }
        // Pointer ownership, not the edge of a transient client buffer,
        // determines when the window changes monitor. The destination's
        // active workspace remains visible throughout the grab.
        let destination = self.monitor_at(position).unwrap_or(owner.monitor);
        let Some((output, _)) = self
            .output_monitors
            .iter()
            .find(|(_, id)| **id == destination)
        else {
            return;
        };
        let Some(area) = self.usable_areas.get(output) else {
            return;
        };
        let Some(geometry) = self.space.output_geometry(output) else {
            return;
        };
        let (x, y) = drag_position(
            origin,
            start,
            position,
            global_usable_area(*area, geometry.loc),
        );
        let command = if destination == owner.monitor {
            WmCommand::MoveFloating {
                monitor: owner.monitor,
                window,
                x,
                y,
            }
        } else {
            WmCommand::TransferFloating {
                window,
                destination,
                x,
                y,
            }
        };
        let _ = self.apply_command(command);
    }

    pub fn end_floating_drag(&mut self) -> bool {
        self.floating_drag
            .take()
            .is_some_and(|drag| drag.forward_release)
    }

    fn resize_column_pair(
        &mut self,
        monitor: MonitorId,
        left: WindowId,
        right: WindowId,
        delta: i32,
    ) -> Result<CommandEffect, CommandError> {
        let left_width = self
            .tile_geometry
            .get(&left)
            .ok_or(CommandError::InvalidGeometry)?
            .width;
        let right_width = self
            .tile_geometry
            .get(&right)
            .ok_or(CommandError::InvalidGeometry)?
            .width;
        self.apply_command(WmCommand::ResizeColumnPair {
            monitor,
            left,
            right,
            left_width,
            right_width,
            delta,
        })
    }

    pub(crate) fn resize_focused_column(
        &mut self,
        monitor: MonitorId,
        direction: Direction,
    ) -> Result<CommandEffect, CommandError> {
        let workspace = self
            .wm
            .active_workspace(monitor)
            .ok_or(CommandError::UnknownMonitor)?;
        let tiles = self
            .wm
            .tiled_windows(monitor, workspace)
            .ok_or(CommandError::UnknownMonitor)?;
        let focused = self
            .wm
            .focused_window(monitor, workspace)
            .ok_or(CommandError::NoFocusedWindow)?;
        let root = self.wm.tile_root(focused).unwrap_or(focused);
        let Some(index) = tiles.iter().position(|&id| id == root) else {
            return Ok(CommandEffect::Unchanged);
        };
        if tiles.len() < 2 {
            return Ok(CommandEffect::Unchanged);
        }
        let (left, right, focused_is_left) = if index + 1 < tiles.len() {
            (tiles[index], tiles[index + 1], true)
        } else {
            (tiles[index - 1], tiles[index], false)
        };
        let total = i64::from(
            self.tile_geometry
                .get(&left)
                .ok_or(CommandError::InvalidGeometry)?
                .width,
        ) + i64::from(
            self.tile_geometry
                .get(&right)
                .ok_or(CommandError::InvalidGeometry)?
                .width,
        );
        let step = (total / 20).max(1) as i32;
        let grow = matches!(direction, Direction::Next);
        let delta = if grow == focused_is_left { step } else { -step };
        self.resize_column_pair(monitor, left, right, delta)
    }

    fn column_divider_at(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(MonitorId, crate::wm::WorkspaceId, DividerResize)> {
        let monitor = self.monitor_at(position)?;
        let workspace = self.wm.active_workspace(monitor)?;
        let tiles = self.wm.tiled_windows(monitor, workspace)?;
        match self.wm.tiling_mode(monitor, workspace)? {
            TilingMode::Floating => None,
            TilingMode::SideBySide => tiles.windows(2).find_map(|pair| {
                let left = self.tile_geometry.get(&pair[0])?;
                let right = self.tile_geometry.get(&pair[1])?;
                let edge = (f64::from(left.x + left.width) + f64::from(right.x)) / 2.0;
                (position.x >= edge - 4.0
                    && position.x <= edge + 4.0
                    && position.y >= f64::from(left.y.max(right.y) + decorations::TITLE_HEIGHT)
                    && position.y < f64::from((left.y + left.height).min(right.y + right.height)))
                .then_some((
                    monitor,
                    workspace,
                    DividerResize::Columns {
                        left: pair[0],
                        right: pair[1],
                    },
                ))
            }),
            TilingMode::MasterStack => {
                let master_count = self.layout_settings.master_count.min(tiles.len());
                if master_count == 0 || master_count == tiles.len() {
                    return None;
                }
                let master = self.tile_geometry.get(&tiles[0])?;
                let stack = self.tile_geometry.get(&tiles[master_count])?;
                let edge = (f64::from(master.x + master.width) + f64::from(stack.x)) / 2.0;
                let output = self
                    .output_monitors
                    .iter()
                    .find(|(_, id)| **id == monitor)?
                    .0;
                let area = global_usable_area(
                    *self.usable_areas.get(output)?,
                    self.space.output_geometry(output)?.loc,
                );
                let shared_width = master.width + stack.width;
                let start_ratio = self.wm.master_ratio(monitor, workspace)?.thousandths();
                let over_titlebar = tiles.iter().any(|id| {
                    self.tile_geometry.get(id).is_some_and(|frame| {
                        position.y >= f64::from(frame.y)
                            && position.y < f64::from(frame.y + decorations::TITLE_HEIGHT)
                    })
                });
                (shared_width > 0
                    && position.x >= edge - 4.0
                    && position.x <= edge + 4.0
                    && position.y >= f64::from(area.loc.y)
                    && position.y < f64::from(area.loc.y + area.size.h)
                    && !over_titlebar)
                    .then_some({
                        (
                            monitor,
                            workspace,
                            DividerResize::MasterStack {
                                start_ratio,
                                start_x: position.x,
                                shared_width,
                            },
                        )
                    })
            }
        }
    }

    pub(crate) fn column_resize_cursor_at(&self, position: Point<f64, Logical>) -> bool {
        if self.column_resize.is_some() {
            return true;
        }
        if self
            .popup_grab
            .as_ref()
            .is_some_and(|grab| !grab.has_ended())
            || self
                .seat
                .get_pointer()
                .is_some_and(|pointer| pointer.is_grabbed())
            || self
                .seat
                .get_keyboard()
                .is_some_and(|keyboard| keyboard.modifier_state().alt)
        {
            return false;
        }
        self.column_divider_at(position).is_some()
    }

    /// Grab the shared vertical edge below titlebars without changing either
    /// window's floating/tiled status or forwarding the press to a client.
    pub(crate) fn begin_column_resize(&mut self, position: Point<f64, Logical>) -> bool {
        let Some((monitor, workspace, kind)) = self.column_divider_at(position) else {
            return false;
        };
        self.column_resize = Some(ColumnResize {
            monitor,
            workspace,
            kind,
            last_x: position.x,
        });
        true
    }

    pub(crate) fn is_column_resize_active(&self) -> bool {
        self.column_resize.is_some()
    }

    pub(crate) fn move_column_resize(&mut self, position: Point<f64, Logical>) {
        let Some(resize) = &mut self.column_resize else {
            return;
        };
        if self.wm.active_workspace(resize.monitor) != Some(resize.workspace) {
            self.column_resize = None;
            return;
        }
        let delta = (position.x - resize.last_x).round() as i32;
        if delta == 0 {
            return;
        }
        resize.last_x += f64::from(delta);
        let (monitor, workspace, kind, last_x) =
            (resize.monitor, resize.workspace, resize.kind, resize.last_x);
        let command = match kind {
            DividerResize::Columns { left, right } => {
                self.resize_column_pair(monitor, left, right, delta)
            }
            DividerResize::MasterStack {
                start_ratio,
                start_x,
                shared_width,
            } => {
                let offset = ((last_x - start_x) * 1000.0 / f64::from(shared_width)).round() as i32;
                let ratio = (i32::from(start_ratio) + offset).clamp(1, 999) as u16;
                self.apply_command(WmCommand::SetMasterRatio {
                    monitor,
                    workspace,
                    ratio,
                })
            }
        };
        if command.is_err() {
            self.column_resize = None;
        }
    }

    pub(crate) fn end_column_resize(&mut self) {
        self.column_resize = None;
        #[cfg(feature = "native")]
        self.flush_resize_configure();
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
        #[cfg(feature = "native")]
        self.flush_resize_configure();
    }

    #[cfg(feature = "native")]
    pub(crate) fn flush_resize_configure(&mut self) {
        for id in self.deferred_resize_configures.drain() {
            if let Some(top) = self.id_windows.get(&id).and_then(Window::toplevel)
                && top.send_pending_configure().is_some()
                && let Some(perf) = self.perf.as_mut()
            {
                perf.resize_configure();
            }
        }
    }

    fn owner_for_new_window(&mut self) -> WindowOwner {
        // Hovering a bar or empty desktop does not change keyboard focus, but
        // new windows should still open on the output under the pointer.
        let monitor = self
            .cursor_position
            .and_then(|position| self.monitor_at(position))
            .or_else(|| self.wm.active_monitor())
            .expect("mapped output");
        self.wm.set_active_monitor(monitor);
        let workspace = self
            .wm
            .active_workspace(monitor)
            .expect("registered monitor");
        WindowOwner { monitor, workspace }
    }

    pub fn register_window(&mut self, window: &Window) {
        let owner = self.owner_for_new_window();
        let id = self.wm.add_window(owner).expect("active workspace exists");
        self.window_ids.insert(window.clone(), id);
        self.id_windows.insert(id, window.clone());
        self.scene_windows.insert(
            id,
            DecoratedWindow::new(
                window.clone(),
                self.keybindings.decoration_colors(),
                self.title_font.clone(),
            ),
        );
        if let Some(top) = window.toplevel() {
            self.surface_windows
                .insert(top.wl_surface().clone(), window.clone());
        }
        self.ipc_dirty = true;
        self.layout_monitor(owner.monitor);
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
            CommandEffect::FocusChanged { monitor } => {
                if matches!(
                    command,
                    WmCommand::CycleGroupTab { .. } | WmCommand::ActivateGroupTab { .. }
                ) {
                    self.reconcile_monitor(monitor);
                } else {
                    self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
                }
            }
            CommandEffect::TileOrderChanged { monitor } => {
                self.reconcile_monitor(monitor);
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
                    self.place_window(window, rect);
                    if let Some(surface) = self
                        .id_windows
                        .get(&window)
                        .and_then(|window| window.toplevel().map(|top| top.wl_surface()))
                    {
                        self.refresh_reactive_popups(surface);
                    }
                }
            }
            CommandEffect::WorkspaceSwitched { monitor }
            | CommandEffect::WindowMoved { monitor, .. } => self.reconcile_monitor(monitor),
            CommandEffect::WindowTransferred { from, to, window } => {
                tracing::info!(
                    ?window,
                    ?from,
                    ?to,
                    "floating window transferred between monitors"
                );
                self.layout_monitor(from);
                self.layout_monitor(to);
                self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
                self.needs_repaint = true;
            }
        }
        Ok(effect)
    }

    /// A same-user control request changes only the named output's view. It
    /// does not transfer keyboard focus to that monitor, and lock state never
    /// accepts control commands.
    pub fn switch_workspace_from_control(&mut self, output: &str, workspace: u32) {
        if self.is_locked() {
            tracing::debug!("ignored workspace control request while locked");
            return;
        }
        let Some(workspace) = crate::wm::WorkspaceId::new(workspace) else {
            return;
        };
        let Some(monitor) = self
            .output_monitors
            .iter()
            .find_map(|(display, &monitor)| (display.name() == output).then_some(monitor))
        else {
            tracing::debug!(output, "ignored control request for unknown output");
            return;
        };
        if let Err(error) = self.apply_command(WmCommand::SwitchWorkspace { monitor, workspace }) {
            tracing::warn!(?error, output, "workspace control request rejected");
        }
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
        let under = if self.is_title_drag_pending()
            || self.is_floating_drag_active()
            || self.is_floating_resize_active()
            || self.is_column_resize_active()
        {
            None
        } else {
            self.surface_under(location)
        };
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
        debug_assert!(
            self.mapped_windows
                .iter()
                .all(|&id| self.wm.is_window_visible(id))
        );
    }

    pub fn focus_window(&mut self, window: Option<&Window>, serial: smithay::utils::Serial) {
        let previous = self.wm.keyboard_focus();
        let tab_switched = window
            .and_then(|window| self.window_ids.get(window))
            .is_some_and(|&id| !self.wm.is_window_visible(id));
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
            if tab_switched {
                self.reconcile_monitor(monitor);
            } else {
                self.sync_keyboard_focus(serial);
            }
        }
    }

    pub(crate) fn raise_floating_on_click(&mut self, window: &Window) {
        if let Some(&id) = self.window_ids.get(window)
            && self.wm.floating_rect(id).is_some()
            && let Some(scene) = self.scene_windows.get(&id)
        {
            self.space.raise_element(scene, false);
            self.needs_repaint = true;
        }
    }

    pub(super) fn sync_keyboard_focus(&mut self, serial: smithay::utils::Serial) {
        if self.is_locked() {
            let focus = self.lock_screen.as_ref().and_then(|session| {
                self.cursor_position
                    .and_then(|pos| self.monitor_at(pos))
                    .and_then(|monitor| {
                        self.output_monitors
                            .iter()
                            .find_map(|(output, &id)| (id == monitor).then_some(output))
                    })
                    .and_then(|output| session.surfaces.get(output))
                    .filter(|surface| surface.alive())
                    .or_else(|| session.surfaces.values().find(|surface| surface.alive()))
                    .map(|surface| surface.wl_surface().clone())
            });
            self.seat
                .get_keyboard()
                .expect("keyboard")
                .set_focus(self, focus, serial);
            return;
        }
        // Interactive top/overlay layer surfaces (such as a launcher) own
        // keyboard input until they close. Keep the WM's remembered toplevel
        // focus so it can be restored immediately afterwards.
        let exclusive = self
            .layers
            .values()
            .filter_map(|entry| {
                use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer};
                let layer = entry.layer.layer();
                (entry.layer.cached_state().keyboard_interactivity
                    == KeyboardInteractivity::Exclusive)
                    .then(|| match layer {
                        Layer::Overlay => Some((2, entry.layer.wl_surface().clone())),
                        Layer::Top => Some((1, entry.layer.wl_surface().clone())),
                        _ => None,
                    })
                    .flatten()
            })
            .max_by_key(|(priority, _)| *priority)
            .map(|(_, surface)| surface);
        if let Some(surface) = exclusive {
            let keyboard = self.seat.get_keyboard().expect("keyboard");
            if keyboard.current_focus() != Some(surface.clone()) {
                keyboard.set_focus(self, Some(surface), serial);
            }
            return;
        }
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
        if self.hovered_window.as_ref() == Some(window) {
            self.hovered_window = None;
        }
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
            self.clear_title_drag();
        } else if self
            .title_drag
            .as_ref()
            .and_then(|drag| drag.target)
            .is_some_and(|target| match target {
                TitleDrop::Insert(root, ..) | TitleDrop::Group(root) => root == id,
            })
        {
            self.title_drag.as_mut().expect("title drag").target = None;
            self.needs_repaint = true;
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
        self.tile_geometry.remove(&id);
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
        let selected = self
            .wm
            .tile_root(id)
            .and_then(|root| self.wm.selected_tile(root))
            .unwrap_or(id);
        if let Some(rect) = self.requested_geometry.get(&selected).copied() {
            self.place_window(selected, rect);
        }
    }
}

struct CompositorClient {
    compositor_state: CompositorClientState,
    disconnect_sender: mpsc::Sender<ClientId>,
}

impl ClientData for CompositorClient {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, client_id: ClientId, _reason: DisconnectReason) {
        let _ = self.disconnect_sender.send(client_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_focused_window_is_safe_with_no_focus() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        assert!(compositor.wm.keyboard_focus().is_none());
        compositor.close_focused_window();
    }

    #[test]
    fn locked_session_hides_ipc_and_never_hit_tests_desktop_surfaces() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        compositor.wm.add_monitor();
        assert!(!compositor.ipc_snapshot().monitors.is_empty());
        compositor.lock_screen = Some(LockSession {
            confirmation: None,
            surfaces: HashMap::new(),
            covered: HashSet::new(),
        });
        assert_eq!(compositor.ipc_snapshot().monitors.len(), 0);
        assert_eq!(compositor.ipc_snapshot().active_monitor, None);
        assert!(compositor.surface_under((20.0, 20.0).into()).is_none());
        compositor.lock_screen = None;
        assert!(!compositor.ipc_snapshot().monitors.is_empty());
    }

    #[test]
    fn configured_tile_gaps_reach_the_compositor_layout() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let keys = Keybindings::parse("[layout]\ninner_gap = 3\nouter_gap = 19\n").unwrap();
        let compositor = Compositor::new(&mut loop_, display, keys, "seat0").unwrap();
        assert_eq!(compositor.layout_settings.inner_gap, 3);
        assert_eq!(compositor.layout_settings.outer_gap, 19);
        assert_eq!(
            compositor.layout_settings.border,
            LayoutSettings::default().border
        );
    }

    #[test]
    fn tile_drop_target_uses_column_halves_and_stack_halves() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let output = smithay::output::Output::new(
            "test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (300, 200).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((0, 0).into()),
        );
        compositor.space.map_output(&output, (0, 0));
        compositor.register_output(&output);
        let monitor = compositor.monitor_for_output(&output).unwrap();
        let ids: Vec<_> = (0..3)
            .map(|index| {
                let id = compositor
                    .wm
                    .add_window(WindowOwner {
                        monitor,
                        workspace: crate::wm::WorkspaceId::FIRST,
                    })
                    .unwrap();
                compositor.tile_geometry.insert(
                    id,
                    Rect {
                        x: index * 100,
                        y: 0,
                        width: 100,
                        height: 200,
                    },
                );
                id
            })
            .collect();
        compositor
            .apply_command(WmCommand::ToggleTilingMode { monitor })
            .unwrap();
        for (index, &id) in ids.iter().enumerate() {
            compositor.tile_geometry.insert(
                id,
                Rect {
                    x: index as i32 * 100,
                    y: 0,
                    width: 100,
                    height: 200,
                },
            );
        }
        assert_eq!(
            compositor.tile_drop_target(ids[0], (110.0, 80.0).into()),
            Some((ids[1], false, DropEdge::Left))
        );
        assert_eq!(
            compositor.tile_drop_target(ids[0], (190.0, 80.0).into()),
            Some((ids[1], true, DropEdge::Right))
        );
        assert_eq!(
            compositor.tile_drop_target(ids[0], (30.0, 80.0).into()),
            None
        );
        assert_eq!(
            compositor.tile_drop_target(ids[0], (320.0, 80.0).into()),
            None
        );
        compositor
            .apply_command(WmCommand::ToggleTilingMode { monitor })
            .unwrap();
        compositor.tile_geometry.insert(
            ids[1],
            Rect {
                x: 100,
                y: 0,
                width: 200,
                height: 100,
            },
        );
        compositor.tile_geometry.insert(
            ids[2],
            Rect {
                x: 100,
                y: 100,
                width: 200,
                height: 100,
            },
        );
        assert_eq!(
            compositor.tile_drop_target(ids[0], (150.0, 25.0).into()),
            Some((ids[1], false, DropEdge::Top))
        );
        assert_eq!(
            compositor.tile_drop_target(ids[0], (150.0, 75.0).into()),
            Some((ids[1], true, DropEdge::Bottom))
        );
    }

    #[test]
    fn grouping_does_not_mark_the_hidden_tabs_full_tile_size_as_configured() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let output = smithay::output::Output::new(
            "test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (400, 200).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((0, 0).into()),
        );
        compositor.space.map_output(&output, (0, 0));
        compositor.register_output(&output);
        let monitor = compositor.monitor_for_output(&output).unwrap();
        let owner = WindowOwner {
            monitor,
            workspace: crate::wm::WorkspaceId::FIRST,
        };
        let a = compositor.wm.add_window(owner).unwrap();
        let b = compositor.wm.add_window(owner).unwrap();
        let old_frame = Rect {
            x: 200,
            y: 0,
            width: 200,
            height: 200,
        };
        compositor.requested_geometry.insert(b, old_frame);

        compositor
            .apply_command(WmCommand::GroupTile {
                monitor,
                window: a,
                target: b,
            })
            .unwrap();
        let slot = compositor.tile_geometry[&b];
        assert!(slot.width > old_frame.width);
        assert_eq!(compositor.requested_geometry[&b], old_frame);
        compositor
            .apply_command(WmCommand::CycleGroupTab { monitor })
            .unwrap();
        assert_eq!(compositor.wm.keyboard_focus(), Some(b));
        assert_eq!(compositor.requested_geometry[&b], old_frame);
        // On a registered client, place_window now sees the width change and
        // sends the missing configure before drawing that tab.
    }

    #[test]
    fn workspace_control_targets_output_without_moving_keyboard_monitor() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let mut monitors = Vec::new();
        for (index, name) in ["HDMI-A-2", "DP-4"].into_iter().enumerate() {
            let output = smithay::output::Output::new(
                name.into(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "virtual".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (400, 200).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                Some(((index as i32) * 400, 0).into()),
            );
            compositor
                .space
                .map_output(&output, ((index as i32) * 400, 0));
            compositor.register_output(&output);
            monitors.push(compositor.monitor_for_output(&output).unwrap());
        }
        assert_eq!(compositor.wm.active_monitor(), Some(monitors[0]));
        compositor.switch_workspace_from_control("DP-4", 7);
        assert_eq!(
            compositor
                .wm
                .active_workspace(monitors[1])
                .unwrap()
                .number(),
            7
        );
        assert_eq!(compositor.wm.active_monitor(), Some(monitors[0]));
        compositor.switch_workspace_from_control("missing", 2);
        compositor.switch_workspace_from_control("DP-4", 0);
        compositor.lock_screen = Some(LockSession {
            confirmation: None,
            surfaces: HashMap::new(),
            covered: HashSet::new(),
        });
        compositor.switch_workspace_from_control("DP-4", 8);
        assert_eq!(
            compositor
                .wm
                .active_workspace(monitors[1])
                .unwrap()
                .number(),
            7
        );
    }

    #[test]
    fn insertion_line_spans_the_slot_in_purple_without_covering_a_window() {
        let area = Rect {
            x: 0,
            y: 24,
            width: 300,
            height: 176,
        };
        let column = Rect {
            x: 108,
            y: 32,
            width: 84,
            height: 160,
        };
        assert_eq!(
            drop_indicator_rect(column, area, DropEdge::Left, 8),
            Some(Rect {
                x: 100,
                y: 24,
                width: 8,
                height: 176
            })
        );
        assert_eq!(
            drop_indicator_rect(column, area, DropEdge::Right, 8),
            Some(Rect {
                x: 192,
                y: 24,
                width: 8,
                height: 176
            })
        );
        let stack = Rect {
            x: 108,
            y: 32,
            width: 184,
            height: 76,
        };
        assert_eq!(
            drop_indicator_rect(stack, area, DropEdge::Bottom, 8),
            Some(Rect {
                x: 108,
                y: 108,
                width: 184,
                height: 8
            })
        );
        assert_eq!(
            drop_indicator_rect(Rect { x: 0, ..column }, area, DropEdge::Left, 0)
                .unwrap()
                .x,
            0
        );
        assert_eq!(
            drop_indicator_rect(column, Rect { height: 0, ..area }, DropEdge::Top, 8),
            None
        );
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let keys = Keybindings::parse("[colors]\nfocused_border = '#1234ab'\n").unwrap();
        let compositor = Compositor::new(&mut loop_, display, keys, "seat0").unwrap();
        assert_eq!(
            compositor.drop_indicator.color(),
            [
                0x12 as f32 / 255.0,
                0x34 as f32 / 255.0,
                0xab as f32 / 255.0,
                1.0
            ]
            .into()
        );
    }

    #[test]
    fn divider_drag_resizes_master_stack_and_side_by_side_but_not_titlebar() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let output = smithay::output::Output::new(
            "test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (200, 200).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((0, 0).into()),
        );
        compositor.space.map_output(&output, (0, 0));
        compositor.register_output(&output);
        let monitor = compositor.monitor_for_output(&output).unwrap();
        let left = compositor
            .wm
            .add_window(WindowOwner {
                monitor,
                workspace: crate::wm::WorkspaceId::FIRST,
            })
            .unwrap();
        let right = compositor
            .wm
            .add_window(WindowOwner {
                monitor,
                workspace: crate::wm::WorkspaceId::FIRST,
            })
            .unwrap();
        compositor.tile_geometry.insert(
            left,
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 200,
            },
        );
        compositor.tile_geometry.insert(
            right,
            Rect {
                x: 100,
                y: 0,
                width: 100,
                height: 200,
            },
        );
        assert!(!compositor.begin_column_resize((100.0, 10.0).into()));
        assert!(compositor.column_resize_cursor_at((100.0, 50.0).into()));
        assert!(compositor.begin_column_resize((100.0, 50.0).into()));
        compositor.move_column_resize((110.0, 50.0).into());
        assert_eq!(
            compositor
                .wm
                .master_ratio(monitor, crate::wm::WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            650
        );
        compositor.move_column_resize((200.0, 50.0).into());
        assert_eq!(
            compositor
                .wm
                .master_ratio(monitor, crate::wm::WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            800
        );
        compositor.end_column_resize();
        compositor
            .apply_command(WmCommand::ToggleTilingMode { monitor })
            .unwrap();
        compositor.tile_geometry.insert(
            left,
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 200,
            },
        );
        compositor.tile_geometry.insert(
            right,
            Rect {
                x: 100,
                y: 0,
                width: 100,
                height: 200,
            },
        );
        assert!(!compositor.begin_column_resize((100.0, 10.0).into()));
        assert!(!compositor.begin_column_resize((80.0, 50.0).into()));
        assert!(!compositor.column_resize_cursor_at((100.0, 10.0).into()));
        assert!(compositor.column_resize_cursor_at((100.0, 50.0).into()));
        assert!(compositor.begin_column_resize((100.0, 50.0).into()));
        assert!(compositor.column_resize_cursor_at((80.0, 50.0).into()));
        compositor.move_column_resize((110.0, 50.0).into());
        assert!(
            compositor
                .wm
                .column_weights(monitor, crate::wm::WorkspaceId::FIRST)
                .unwrap()[0]
                > 1000
        );
        compositor.end_column_resize();
        assert!(!compositor.is_column_resize_active());
        assert!(!compositor.column_resize_cursor_at((80.0, 50.0).into()));
    }

    #[test]
    fn column_keys_shrink_and_grow_focused_tile() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let output = smithay::output::Output::new(
            "test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (300, 200).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((0, 0).into()),
        );
        compositor.space.map_output(&output, (0, 0));
        compositor.register_output(&output);
        let monitor = compositor.monitor_for_output(&output).unwrap();
        let windows: Vec<_> = (0..3)
            .map(|index| {
                let id = compositor
                    .wm
                    .add_window(WindowOwner {
                        monitor,
                        workspace: crate::wm::WorkspaceId::FIRST,
                    })
                    .unwrap();
                compositor.tile_geometry.insert(
                    id,
                    Rect {
                        x: index * 100,
                        y: 0,
                        width: 100,
                        height: 200,
                    },
                );
                id
            })
            .collect();
        compositor.wm.set_focus(monitor, Some(windows[2]));
        compositor
            .apply_command(WmCommand::ToggleTilingMode { monitor })
            .unwrap();
        for (index, &id) in windows.iter().enumerate() {
            compositor.tile_geometry.insert(
                id,
                Rect {
                    x: index as i32 * 100,
                    y: 0,
                    width: 100,
                    height: 200,
                },
            );
        }
        compositor
            .resize_focused_column(monitor, Direction::Previous)
            .unwrap();
        let after_h = compositor
            .wm
            .column_weights(monitor, crate::wm::WorkspaceId::FIRST)
            .unwrap();
        assert_eq!(after_h[0], 1000);
        assert!(after_h[1] > 1000 && after_h[2] < 1000);
        compositor
            .resize_focused_column(monitor, Direction::Next)
            .unwrap();
        let after_l = compositor
            .wm
            .column_weights(monitor, crate::wm::WorkspaceId::FIRST)
            .unwrap();
        assert!(after_l[1] < after_h[1] && after_l[2] > after_h[2]);
    }

    #[test]
    fn zero_gaps_fill_usable_edges_without_extra_border_insets() {
        let keys = Keybindings::parse("[layout]\ninner_gap = 0\nouter_gap = 0\n").unwrap();
        let (inner_gap, outer_gap) = keys.layout_gaps();
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let ids: Vec<_> = (0..3)
            .map(|_| {
                wm.add_window(WindowOwner {
                    monitor,
                    workspace: crate::wm::WorkspaceId::FIRST,
                })
                .unwrap()
            })
            .collect();
        let area = Rect {
            x: 10,
            y: 32, // Bar-exclusive area begins below the bar.
            width: 100,
            height: 80,
        };
        let settings = LayoutSettings {
            inner_gap,
            outer_gap,
            ..LayoutSettings::default()
        };
        let placements = layout::master_stack(area, &ids, settings).unwrap();
        let master = placements[0].frame;
        let first_stack = placements[1].frame;
        let second_stack = placements[2].frame;
        assert_eq!((master.x, master.y), (area.x, area.y));
        assert_eq!(master.x + master.width, first_stack.x);
        assert_eq!(first_stack.y + first_stack.height, second_stack.y);
        assert_eq!(first_stack.x + first_stack.width, area.x + area.width);
        assert_eq!(second_stack.y + second_stack.height, area.y + area.height);
        // The layout's content inset is *not* another outer margin; the
        // decoration reserves its border inside the full tile frame.
        assert_eq!(placements[0].content.x - master.x, settings.border);
    }

    #[test]
    fn newly_discovered_output_recovers_windows_after_last_output_was_lost() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let create = |name: &str| {
            let output = smithay::output::Output::new(
                name.into(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "virtual".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (1920, 1080).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                Some((0, 0).into()),
            );
            output
        };
        let first = create("HDMI-A-2");
        compositor.space.map_output(&first, (0, 0));
        compositor.register_output(&first);
        let old = compositor.monitor_for_output(&first).unwrap();
        let window = compositor
            .wm
            .add_window(WindowOwner {
                monitor: old,
                workspace: crate::wm::WorkspaceId::FIRST,
            })
            .unwrap();
        compositor.detach_output(&first);
        let discovered = create("DP-5");
        compositor.space.map_output(&discovered, (0, 0));
        compositor.register_output(&discovered);
        let new = compositor.monitor_for_output(&discovered).unwrap();
        assert_ne!(old, new);
        assert_eq!(compositor.wm.owner(window).unwrap().monitor, new);
        assert_eq!(compositor.wm.active_monitor(), Some(new));
        assert_eq!(compositor.space.outputs().count(), 1);
    }

    #[test]
    fn simulated_outputs_detach_and_reattach_without_reallocating_monitor_identity() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let create = |name: &str, width, x| {
            let output = smithay::output::Output::new(
                name.into(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "virtual".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (width, 1080).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                Some((x, 0).into()),
            );
            output
        };
        let hdmi = create("HDMI-A-2", 3840, 0);
        let dp = create("DP-4", 1920, 3840);
        for (output, x) in [(&hdmi, 0), (&dp, 3840)] {
            compositor.space.map_output(output, (x, 0));
            compositor.register_output(output);
        }
        let hdmi_id = compositor.monitor_for_output(&hdmi).unwrap();
        let dp_id = compositor.monitor_for_output(&dp).unwrap();
        compositor.detach_output(&hdmi);
        assert_eq!(compositor.wm.active_monitor(), Some(dp_id));
        assert_eq!(compositor.space.outputs().count(), 1);
        compositor.move_output(&dp, 0);
        assert_eq!(
            compositor.space.output_geometry(&dp).unwrap().loc,
            (0, 0).into()
        );
        compositor.space.map_output(&hdmi, (0, 0));
        compositor.reattach_output(&hdmi, hdmi_id);
        compositor.move_output(&dp, 3840);
        assert_eq!(compositor.monitor_for_output(&hdmi), Some(hdmi_id));
        assert_eq!(compositor.space.outputs().count(), 2);
        compositor.detach_output(&hdmi);
        compositor.detach_output(&dp);
        assert_eq!(compositor.space.outputs().count(), 0);
        assert_eq!(compositor.wm.active_monitor(), Some(dp_id));
        compositor.space.map_output(&hdmi, (0, 0));
        compositor.reattach_output(&hdmi, hdmi_id);
        assert_eq!(compositor.wm.active_monitor(), Some(hdmi_id));
    }

    #[test]
    fn new_windows_use_the_output_under_the_pointer_without_a_prior_click() {
        let mut loop_ = EventLoop::<crate::LoopData>::try_new().unwrap();
        let display = Display::<Compositor>::new().unwrap();
        let mut compositor =
            Compositor::new(&mut loop_, display, Keybindings::default(), "seat0").unwrap();
        let create = |name: &str, x| {
            let output = smithay::output::Output::new(
                name.into(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "virtual".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (800, 600).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                Some((x, 0).into()),
            );
            output
        };
        let hdmi = create("HDMI-A-2", 0);
        let dp = create("DP-4", 800);
        for (output, x) in [(&hdmi, 0), (&dp, 800)] {
            compositor.space.map_output(output, (x, 0));
            compositor.register_output(output);
        }
        let hdmi_id = compositor.monitor_for_output(&hdmi).unwrap();
        let dp_id = compositor.monitor_for_output(&dp).unwrap();
        let dp_workspace = crate::wm::WorkspaceId::new(2).unwrap();
        compositor
            .wm
            .execute(WmCommand::SwitchWorkspace {
                monitor: dp_id,
                workspace: dp_workspace,
            })
            .unwrap();
        compositor.wm.set_active_monitor(hdmi_id);
        compositor.cursor_position = Some((900.0, 100.0).into());
        assert_eq!(compositor.wm.active_monitor(), Some(hdmi_id));
        assert_eq!(
            compositor.owner_for_new_window(),
            WindowOwner {
                monitor: dp_id,
                workspace: dp_workspace
            }
        );
        assert_eq!(compositor.wm.active_monitor(), Some(dp_id));

        compositor.wm.set_active_monitor(hdmi_id);
        compositor.cursor_position = None;
        assert_eq!(compositor.owner_for_new_window().monitor, hdmi_id);
        compositor.cursor_position = Some((1700.0, 100.0).into());
        assert_eq!(compositor.owner_for_new_window().monitor, hdmi_id);
    }

    #[test]
    fn each_outputs_local_usable_area_maps_to_its_own_global_origin() {
        let local = Rectangle::new((0, 24).into(), (1920, 1056).into());
        assert_eq!(global_usable_area(local, (0, 0).into()).loc, (0, 24).into());
        let second = global_usable_area(local, (1920, 0).into());
        assert_eq!(second.loc, (1920, 24).into());
        let window = Rect {
            x: 1980,
            y: 40,
            width: 600,
            height: 400,
        };
        assert_eq!(
            drag_position(window, (2000.0, 50.0).into(), (2010.0, 50.0).into(), second),
            (1990, 40)
        );
    }

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
