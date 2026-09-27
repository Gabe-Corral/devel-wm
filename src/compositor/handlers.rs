//! Minimal Wayland protocol handlers for the nested prototype.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        LayerSurface as DesktopLayerSurface, PopupKeyboardGrab, PopupKind, PopupPointerGrab,
        Window, find_popup_root_surface, layer_map_for_output,
    },
    input::pointer::{CursorImageStatus, Focus},
    input::{Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        Client, Resource,
        protocol::{wl_buffer, wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        output::OutputHandler,
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
        },
        shell::wlr_layer::{
            Layer, LayerSurface, LayerSurfaceCachedState, WlrLayerShellHandler, WlrLayerShellState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData, decoration::XdgDecorationHandler,
        },
        shm::{ShmHandler, ShmState},
    },
};

use super::{Compositor, CompositorClient, LayerEntry};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;

fn same_layer_layout(a: LayerSurfaceCachedState, b: LayerSurfaceCachedState) -> bool {
    a.size == b.size
        && a.anchor == b.anchor
        && a.exclusive_zone == b.exclusive_zone
        && a.layer == b.layer
        && (a.margin.top, a.margin.right, a.margin.bottom, a.margin.left)
            == (b.margin.top, b.margin.right, b.margin.bottom, b.margin.left)
}

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<CompositorClient>()
            .expect("client data")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let visible = self.surface_affects_visible_scene(&root);
        if let Some(perf) = self.perf.as_mut() {
            perf.commit(visible);
        }
        self.needs_repaint |= visible;
        if matches!(&self.cursor.image, CursorImageStatus::Surface(cursor) if cursor == &root) {
            self.needs_repaint = true;
        }
        if !is_sync_subsurface(surface)
            && let Some(window) = self.surface_windows.get(&root)
        {
            window.on_commit();
        }
        if let Some(window) = self.surface_windows.get(surface) {
            let configured = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .expect("xdg toplevel")
                    .lock()
                    .expect("xdg state")
                    .initial_configure_sent
            });
            if !configured {
                window.toplevel().expect("toplevel").send_configure();
            }
            self.refresh_toplevel_decoration(surface);
        }
        self.popups.commit(surface);
        if let Some(entry) = self.layers.get_mut(surface) {
            let current = entry.layer.cached_state();
            let changed = !same_layer_layout(entry.layout_state, current);
            entry.layout_state = current;
            let output = entry.output.clone();
            let layer = entry.layer.clone();
            if changed {
                layer_map_for_output(&output).arrange();
                self.update_usable_area(&output);
            }
            layer.layer_surface().send_pending_configure();
        }
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            let _ = popup.send_configure();
        }
    }
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        println!("xdg-shell toplevel connected");
        let window = Window::new_wayland_window(surface);
        self.register_window(&window);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.unregister_toplevel(surface.wl_surface());
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_toplevel_decoration(surface.wl_surface());
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat_resource: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::from_resource(&seat_resource).filter(|seat| seat == &self.seat)
        else {
            return;
        };
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let Some(keyboard) = seat.get_keyboard() else {
            return;
        };
        // Do not let an unfocused client grab input. Nested popups may already
        // have keyboard focus, so compare their root rather than the surface.
        let focused_root = keyboard.current_focus().map(|focused| {
            self.popups
                .find_popup(&focused)
                .and_then(|popup| find_popup_root_surface(&popup).ok())
                .unwrap_or(focused)
        });
        if focused_root.as_ref() != Some(&root) {
            tracing::debug!("denying popup grab without focus on its root");
            return;
        }
        if !seat
            .get_pointer()
            .is_some_and(|pointer| pointer.has_grab(serial))
            && self.last_forwarded_button_serial != Some(serial)
            && self.last_forwarded_key_serial != Some(serial)
        {
            tracing::debug!("denying popup grab without a matching input serial");
            return;
        }
        match self.popups.grab_popup(root, popup, &seat, serial) {
            Ok(grab) => {
                keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                if let Some(pointer) = seat.get_pointer() {
                    pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
                }
                self.popup_grab = Some(grab);
            }
            Err(error) => tracing::warn!(?error, "popup grab rejected"),
        }
    }
}

impl XdgDecorationHandler for Compositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(toplevel, DecorationMode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        self.set_decoration_mode(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.set_decoration_mode(toplevel, DecorationMode::ServerSide);
    }
}

impl Compositor {
    fn set_decoration_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

impl WlrLayerShellHandler for Compositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        requested_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = requested_output
            .as_ref()
            .and_then(smithay::output::Output::from_resource)
            .or_else(|| self.space.outputs().next().cloned());
        let Some(output) = output else {
            return;
        };
        let layer = DesktopLayerSurface::new(surface, namespace);
        if let Err(err) = layer_map_for_output(&output).map_layer(&layer) {
            tracing::warn!("Could not map layer surface: {err}");
            return;
        }
        self.layers.insert(
            layer.wl_surface().clone(),
            LayerEntry {
                output: output.clone(),
                layout_state: layer.cached_state(),
                layer,
            },
        );
        self.update_usable_area(&output);
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        let _ = self.popups.track_popup(PopupKind::Xdg(popup));
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        if let Some(entry) = self.layers.remove(surface.wl_surface()) {
            layer_map_for_output(&entry.output).unmap_layer(&entry.layer);
            self.update_usable_area(&entry.output);
        }
    }
}

impl SeatHandler for Compositor {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // A client may change the hotspot without changing the surface handle.
        tracing::debug!(?image, "cursor image requested");
        self.cursor.image = image;
        self.needs_repaint = true;
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
    }
}

impl SelectionHandler for Compositor {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        seat: Seat<Self>,
    ) {
        if ty == SelectionTarget::Clipboard {
            self.clipboard_owner = source.and_then(|_| {
                seat.get_keyboard()
                    .and_then(|keyboard| keyboard.current_focus())
                    .and_then(|surface| self.display_handle.get_client(surface.id()).ok())
                    .map(|client| client.id())
            });
        }
    }
}
impl DataDeviceHandler for Compositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for Compositor {}
impl ServerDndGrabHandler for Compositor {}
impl OutputHandler for Compositor {}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);
delegate_layer_shell!(Compositor);
delegate_seat!(Compositor);
delegate_data_device!(Compositor);
delegate_output!(Compositor);
