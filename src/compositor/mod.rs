//! Smithay integration: Wayland surfaces, outputs, and event loop.

mod handlers;
mod nested;

use std::{collections::HashMap, ffi::OsString, sync::Arc, time::Instant};

use smithay::{
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

pub use nested::{NestedOutput, run_nested};

use crate::wm::{MonitorId, WindowId, WindowOwner, WmState};

pub struct Compositor {
    pub needs_repaint: bool,
    pub socket_name: OsString,
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub space: Space<Window>,
    pub wm: WmState,
    output_monitors: HashMap<smithay::output::Output, MonitorId>,
    window_ids: HashMap<Window, WindowId>,
    pub popups: PopupManager,
    pub loop_signal: smithay::reexports::calloop::LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
}

impl Compositor {
    pub fn new(
        event_loop: &mut EventLoop<crate::LoopData>,
        display: Display<Self>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
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
            socket_name,
            start_time: Instant::now(),
            display_handle: dh,
            space: Space::default(),
            wm: WmState::new(),
            output_monitors: HashMap::new(),
            window_ids: HashMap::new(),
            popups: PopupManager::default(),
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
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
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + location).to_f64()))
            })
    }

    pub fn register_output(&mut self, output: &smithay::output::Output) {
        let monitor = self.wm.add_monitor();
        self.output_monitors.insert(output.clone(), monitor);
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
    }

    pub fn focus_window(&mut self, window: Option<&Window>, serial: smithay::utils::Serial) {
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
        self.sync_keyboard_focus(serial);
    }

    fn sync_keyboard_focus(&mut self, serial: smithay::utils::Serial) {
        let focused = self.wm.keyboard_focus().and_then(|id| {
            self.window_ids.iter().find_map(|(window, &window_id)| {
                (window_id == id)
                    .then(|| window.toplevel().map(|top| top.wl_surface().clone()))
                    .flatten()
            })
        });
        let keyboard = self.seat.get_keyboard().expect("keyboard");
        if keyboard.current_focus() != focused {
            keyboard.set_focus(self, focused, serial);
        }
    }

    /// Reconcile Smithay's dead-window cleanup with the pure WM registry.
    pub fn cleanup_windows(&mut self) {
        let removed: Vec<_> = self
            .window_ids
            .iter()
            .filter(|(window, _)| !self.space.elements().any(|live| live == *window))
            .map(|(window, &id)| (window.clone(), id))
            .collect();
        if removed.is_empty() {
            return;
        }
        for (window, id) in removed {
            self.window_ids.remove(&window);
            self.wm.remove_window(id);
        }
        self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
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
