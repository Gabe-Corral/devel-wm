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
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial},
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

/// Positioners use parent-window-geometry coordinates, not global coordinates.
fn popup_constraint_target(
    output: Rectangle<i32, Logical>,
    parent_origin: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    Rectangle::new(output.loc - parent_origin, output.size)
}

impl Compositor {
    fn popup_constraint(&self, popup: &PopupSurface) -> Option<Rectangle<i32, Logical>> {
        let parent = popup.get_parent_surface()?;
        let root = find_popup_root_surface(&PopupKind::Xdg(popup.clone())).ok()?;
        let (output, root_origin) = if let Some(window) = self.surface_windows.get(&root) {
            let id = self.window_ids.get(window)?;
            let scene = self.scene_windows.get(id)?;
            let output = self
                .output_monitors
                .iter()
                .find(|(_, monitor)| Some(**monitor) == self.wm.owner(*id).map(|o| o.monitor))?
                .0;
            (
                output,
                self.space.element_location(scene)? + window.geometry().loc,
            )
        } else {
            let entry = self.layers.get(&root)?;
            let layer_map = layer_map_for_output(&entry.output);
            let origin = self.space.output_geometry(&entry.output)?.loc
                + layer_map.layer_geometry(&entry.layer)?.loc
                - entry.layer.bbox().loc;
            (&entry.output, origin)
        };
        let parent_offset = if parent == root {
            (0, 0).into()
        } else {
            smithay::desktop::PopupManager::popups_for_surface(&root)
                .find(|(candidate, _)| candidate.wl_surface() == &parent)
                .map(|(_, location)| location)?
        };
        Some(popup_constraint_target(
            self.space.output_geometry(output)?,
            root_origin + parent_offset,
        ))
    }

    fn position_popup(&self, popup: &PopupSurface, positioner: PositionerState) {
        let geometry = self
            .popup_constraint(popup)
            .map(|target| positioner.get_unconstrained_geometry(target))
            .unwrap_or_else(|| positioner.get_geometry());
        popup.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
    }

    pub(super) fn refresh_reactive_popups(&self, root: &WlSurface) {
        for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(root) {
            let PopupKind::Xdg(popup) = popup else {
                continue;
            };
            if !popup.is_initial_configure_sent() {
                continue;
            }
            let positioner = popup.with_pending_state(|state| state.positioner);
            if positioner.reactive && self.popup_constraint(&popup).is_some() {
                self.position_popup(&popup, positioner);
                let _ = popup.send_pending_configure();
            }
        }
    }
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
        if self.is_locked()
            && self.lock_screen.as_ref().is_some_and(|session| {
                session
                    .surfaces
                    .values()
                    .any(|lock| lock.wl_surface() == &root)
            })
        {
            self.needs_repaint = true;
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
        if let Some(popup) = self.popups.find_popup(surface)
            && let Ok(root) = find_popup_root_surface(&popup)
        {
            // A parent popup may have just acknowledged a new position. Its
            // reactive children need a target in the updated coordinate space.
            self.refresh_reactive_popups(&root);
        }
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
            if current.keyboard_interactivity
                == smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
            {
                self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
            }
        }
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            // A layer parent can acquire its output geometry only at commit.
            let positioner = popup.with_pending_state(|state| state.positioner);
            self.position_popup(&popup, positioner);
            let _ = popup.send_configure();
        }
    }
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

#[cfg(feature = "native")]
impl smithay::wayland::dmabuf::DmabufHandler for Compositor {
    fn dmabuf_state(&mut self) -> &mut smithay::wayland::dmabuf::DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &smithay::wayland::dmabuf::DmabufGlobal,
        _dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: smithay::wayland::dmabuf::ImportNotifier,
    ) {
        // Advertised formats are imported on the single renderer during repaint.
        let _ = notifier.successful::<Self>();
    }
}

#[cfg(feature = "native")]
smithay::delegate_dmabuf!(Compositor);

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

    fn move_request(
        &mut self,
        surface: ToplevelSurface,
        seat_resource: wl_seat::WlSeat,
        serial: Serial,
    ) {
        if self.is_locked() {
            return;
        }
        let Some(seat) = Seat::from_resource(&seat_resource).filter(|seat| seat == &self.seat)
        else {
            return;
        };
        let Some(pointer) = seat
            .get_pointer()
            .filter(|pointer| pointer.has_grab(serial))
        else {
            return;
        };
        let Some(grab) = pointer.grab_start_data() else {
            return;
        };
        // BTN_LEFT from linux/input-event-codes.h; only that button ends the
        // floating drag in the shared input path.
        if grab.button != 0x110 {
            return;
        }
        // An xdg move must originate from a real press on this toplevel,
        // including its client-side decoration or subsurfaces. Reject stale
        // serials and presses on a different window or a layer surface.
        let Some((focus, _)) = grab.focus else { return };
        let Some(window) = self.surface_windows.get(surface.wl_surface()).cloned() else {
            return;
        };
        let Some((under, origin)) = self.space.element_under(grab.location) else {
            return;
        };
        if under.window != window
            || under
                .window
                .surface_under(
                    grab.location - origin.to_f64(),
                    smithay::desktop::WindowSurfaceType::ALL,
                )
                .is_none_or(|(pressed, _)| pressed != focus)
            || !self.begin_client_move_drag(&window, grab.location)
        {
            return;
        }
        tracing::info!("client xdg move request started floating drag");
        let time = self.start_time.elapsed().as_millis() as u32;
        pointer.motion(
            self,
            None,
            &smithay::input::pointer::MotionEvent {
                location: pointer.current_location(),
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let _ = self.popups.track_popup(PopupKind::Xdg(surface.clone()));
        self.position_popup(&surface, positioner);
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_toplevel_decoration(surface.wl_surface());
        self.ipc_dirty = true;
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        self.position_popup(&surface, positioner);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat_resource: wl_seat::WlSeat, serial: Serial) {
        if self.is_locked() {
            return;
        }
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
            .or_else(|| {
                self.space
                    .outputs()
                    .find(|output| self.monitor_for_output(output) == self.wm.active_monitor())
                    .cloned()
            })
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
            self.sync_keyboard_focus(SERIAL_COUNTER.next_serial());
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
        // Client-provided cursor surfaces can use a different theme/size than
        // the compositor's cursor (e.g. Firefox's volume control). Keep the
        // compositor theme instead of drawing that surface directly.
        self.cursor.image = match image {
            CursorImageStatus::Surface(_) => CursorImageStatus::default_named(),
            other => other,
        };
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
smithay::delegate_viewporter!(Compositor);
delegate_shm!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);
delegate_layer_shell!(Compositor);
delegate_seat!(Compositor);
delegate_data_device!(Compositor);
delegate_output!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner::{
        Anchor, ConstraintAdjustment, Gravity,
    };

    #[test]
    fn popup_target_uses_parent_geometry_coordinates() {
        let output = Rectangle::new((600, 100).into(), (200, 150).into());
        let root_origin: Point<i32, Logical> = (700, 120).into();
        assert_eq!(
            popup_constraint_target(output, root_origin),
            Rectangle::new((-100, -20).into(), (200, 150).into())
        );
        // A nested popup's positioner is relative to the parent popup, not
        // to the root toplevel or the output.
        assert_eq!(
            popup_constraint_target(output, root_origin + Point::from((35, 40))),
            Rectangle::new((-135, -60).into(), (200, 150).into())
        );
    }

    #[test]
    fn positioner_flips_an_edge_popup_inside_the_output() {
        let positioner = PositionerState {
            anchor_rect: Rectangle::new((90, 20).into(), (10, 10).into()),
            rect_size: (40, 20).into(),
            anchor_edges: Anchor::BottomRight,
            gravity: Gravity::BottomRight,
            constraint_adjustment: ConstraintAdjustment::FlipX,
            ..PositionerState::default()
        };
        let target = popup_constraint_target(
            Rectangle::new((0, 0).into(), (100, 100).into()),
            (0, 0).into(),
        );
        assert_eq!(positioner.get_geometry().loc, (100, 30).into());
        assert_eq!(
            positioner.get_unconstrained_geometry(target).loc,
            (50, 30).into()
        );
    }
}
