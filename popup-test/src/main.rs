//! Small native Wayland client for explicit xdg_popup grabs and repositioning.

use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_popup, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgPositioner, XdgShell, XdgSurface,
            popup::{ConfigureKind, Popup, PopupConfigure, PopupHandler},
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};

const ROOT: (i32, i32) = (360, 240);
const POPUP: (i32, i32) = (160, 100);
const CHILD: (i32, i32) = (100, 70);
const MAX_BUFFERS: usize = 32;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = XdgShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;
    let window = shell.create_window(
        compositor.create_surface(&qh),
        WindowDecorations::RequestServer,
        &qh,
    );
    window.set_title("Popup protocol test");
    window.set_app_id("devel-wm.popup-test");
    window.commit();
    let seats = SeatState::new(&globals, &qh);
    // SeatState binds seats already advertised during registry initialization.
    // new_seat is only called for seats added later.
    let initial_seat = seats.seats().next();

    let mut app = App {
        registry: RegistryState::new(&globals),
        seats,
        outputs: OutputState::new(&globals, &qh),
        compositor,
        shell,
        pool: SlotPool::new(1024 * 1024, &shm)?,
        shm,
        window,
        root_drawn: false,
        popup_size: None,
        child_size: None,
        buffers_created: 0,
        seat: initial_seat,
        pointer: None,
        keyboard: None,
        popup: None,
        child: None,
        next_token: 1,
        pending_token: None,
        shifted: false,
        exit: false,
    };
    println!(
        "Click the blue content BELOW the titlebar (not the separate bar). Left-click: grabbed popup; left-click green: reposition; right-click green: nested popup; click outside: dismiss; Escape: close; Ctrl-C: exit"
    );
    while !app.exit {
        queue.blocking_dispatch(&mut app)?;
    }
    Ok(())
}

struct App {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    shell: XdgShell,
    pool: SlotPool,
    shm: Shm,
    window: Window,
    root_drawn: bool,
    popup_size: Option<(i32, i32)>,
    child_size: Option<(i32, i32)>,
    buffers_created: usize,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    popup: Option<Popup>,
    child: Option<Popup>,
    next_token: u32,
    pending_token: Option<u32>,
    shifted: bool,
    exit: bool,
}

impl App {
    fn surface_name(&self, surface: &wl_surface::WlSurface) -> &'static str {
        if surface == self.window.wl_surface() {
            "blue window"
        } else if self
            .popup
            .as_ref()
            .is_some_and(|p| surface == p.wl_surface())
        {
            "green popup"
        } else if self
            .child
            .as_ref()
            .is_some_and(|p| surface == p.wl_surface())
        {
            "red child popup"
        } else {
            "other surface"
        }
    }

    fn positioner(&self, size: (i32, i32), anchor: (i32, i32)) -> XdgPositioner {
        let positioner = XdgPositioner::new(&self.shell).expect("xdg positioner");
        positioner.set_size(size.0, size.1);
        positioner.set_anchor_rect(anchor.0, anchor.1, 1, 1);
        positioner.set_anchor(Anchor::BottomRight);
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipX
                | ConstraintAdjustment::FlipY
                | ConstraintAdjustment::SlideX
                | ConstraintAdjustment::SlideY,
        );
        positioner.set_reactive();
        positioner
    }

    fn open_popup(&mut self, qh: &QueueHandle<Self>, serial: u32, nested: bool, at: (i32, i32)) {
        let Some(seat) = &self.seat else {
            eprintln!("cannot request popup grab: no Wayland seat available");
            return;
        };
        let (parent, size) = if nested {
            let Some(parent) = self.popup.as_ref() else {
                return;
            };
            (parent.xdg_surface(), CHILD)
        } else {
            (self.window.xdg_surface(), POPUP)
        };
        let positioner = self.positioner(size, at);
        let surface = self.compositor.create_surface(qh);
        // Unlike Popup::new, from_surface does not commit. The grab MUST be
        // requested before the popup's first commit under xdg-shell.
        let popup = Popup::from_surface(Some(parent), &positioner, qh, surface, &self.shell)
            .expect("create popup");
        popup.xdg_popup().grab(seat, serial);
        popup.wl_surface().commit();
        println!(
            "requested {}grab with input serial {serial}",
            if nested { "nested " } else { "" }
        );
        if nested {
            self.child = Some(popup);
            self.child_size = None;
        } else {
            self.popup = Some(popup);
            self.popup_size = None;
        }
    }

    fn reposition(&mut self) {
        let Some(popup) = &self.popup else { return };
        self.shifted = !self.shifted;
        let anchor = if self.shifted { (210, 130) } else { (24, 24) };
        let positioner = self.positioner(POPUP, anchor);
        let token = self.next_token;
        self.next_token += 1;
        self.pending_token = Some(token);
        popup.reposition(&positioner, token);
        println!("requested reposition token {token} at {anchor:?}");
    }

    fn draw(&mut self, surface: &wl_surface::WlSurface, size: (i32, i32), color: u32) -> bool {
        // A test client should never be able to exhaust the desktop by
        // allocating a fresh SHM buffer for every configure or huge output.
        if size.0 <= 0
            || size.1 <= 0
            || size.0 > 512
            || size.1 > 384
            || self.buffers_created >= MAX_BUFFERS
        {
            eprintln!("skipping unsafe buffer allocation for {size:?}");
            surface.commit();
            return false;
        }
        self.buffers_created += 1;
        let (buffer, pixels) = self
            .pool
            .create_buffer(size.0, size.1, size.0 * 4, wl_shm::Format::Argb8888)
            .expect("create buffer");
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.to_le_bytes());
        }
        buffer.attach_to(surface).expect("attach buffer");
        surface.damage_buffer(0, 0, size.0, size.1);
        surface.commit();
        true
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        if !self.root_drawn {
            println!(
                "window configured: {:?}; drawing a fixed {ROOT:?} test surface",
                configure.new_size
            );
            self.root_drawn = self.draw(window.wl_surface(), ROOT, 0xff34465c);
        }
    }
}

impl PopupHandler for App {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        popup: &Popup,
        config: PopupConfigure,
    ) {
        println!(
            "popup configure: {:?} at {:?}, {}x{}",
            config.kind, config.position, config.width, config.height
        );
        match config.kind {
            ConfigureKind::Reposition { token } if self.pending_token == Some(token) => {
                self.pending_token = None;
                println!("PASS: reposition token {token} acknowledged with configure");
            }
            ConfigureKind::Reposition { token } => println!("unexpected reposition token {token}"),
            _ => {}
        }
        let child = self.child.as_ref().is_some_and(|p| p == popup);
        let size = (config.width, config.height);
        let previous = if child {
            self.child_size
        } else {
            self.popup_size
        };
        if previous == Some(size) {
            popup.wl_surface().commit();
        } else if self.draw(
            popup.wl_surface(),
            size,
            if child { 0xffa84c3c } else { 0xff4a9a68 },
        ) {
            if child {
                self.child_size = Some(size);
            } else {
                self.popup_size = Some(size);
            }
        }
    }
    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        println!(
            "popup_done: {}",
            if self.child.as_ref() == Some(popup) {
                "child"
            } else {
                "parent"
            }
        );
        if self.child.as_ref() == Some(popup) {
            self.child = None;
            self.child_size = None;
        } else if self.popup.as_ref() == Some(popup) {
            self.child = None;
            self.child_size = None;
            self.popup = None;
            self.popup_size = None;
            self.pending_token = None;
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if let PointerEventKind::Enter { .. } = event.kind {
                println!(
                    "pointer entered {} at {:?}",
                    self.surface_name(&event.surface),
                    event.position
                );
            }
            if let PointerEventKind::Press { serial, button, .. } = event.kind {
                println!(
                    "pointer press on {}: button {button:#x}, serial {serial}, position {:?}",
                    self.surface_name(&event.surface),
                    event.position
                );
                if event.surface == *self.window.wl_surface()
                    && button == 0x110
                    && self.popup.is_none()
                {
                    self.open_popup(
                        qh,
                        serial,
                        false,
                        (event.position.0 as i32, event.position.1 as i32),
                    );
                } else if self
                    .popup
                    .as_ref()
                    .is_some_and(|p| event.surface == *p.wl_surface())
                {
                    if button == 0x110 {
                        self.reposition();
                    } else if button == 0x111 && self.child.is_none() {
                        self.open_popup(qh, serial, true, (80, 24));
                    }
                }
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.child = None;
            self.child_size = None;
            self.popup = None;
            self.popup_size = None;
            self.pending_token = None;
            println!("dismissed popups with Escape");
        }
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat = Some(seat);
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.as_ref() == Some(&seat) {
            self.seat = None;
        }
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = Some(self.seats.get_pointer(qh, &seat).expect("pointer"));
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(self.seats.get_keyboard(qh, &seat, None).expect("keyboard"));
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

delegate_compositor!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_xdg_popup!(App);
delegate_pointer!(App);
delegate_keyboard!(App);
delegate_seat!(App);
delegate_shm!(App);
delegate_output!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
