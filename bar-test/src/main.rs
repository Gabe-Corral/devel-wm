//! Minimal layer-shell client for testing devel-wm, not a production status bar.

use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
};

use serde::Deserialize;
use smithay_client_toolkit::reexports::{
    calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
    calloop_wayland_source::WaylandSource,
};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};

const HEIGHT: u32 = 32;

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct BarSnapshot {
    active_monitor: Option<u64>,
    monitors: Vec<BarMonitor>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct BarMonitor {
    id: u64,
    name: String,
    active_workspace: u32,
    workspaces: Vec<BarWorkspace>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct BarWorkspace {
    id: u32,
    occupied: bool,
    focused_window: Option<u64>,
}

fn digit_pixel(digit: u32, x: usize, y: usize) -> bool {
    const GLYPHS: [[u8; 5]; 9] = [
        [0b010, 0b110, 0b010, 0b010, 0b111], // 1
        [0b111, 0b001, 0b111, 0b100, 0b111], // 2
        [0b111, 0b001, 0b111, 0b001, 0b111], // 3
        [0b101, 0b101, 0b111, 0b001, 0b001], // 4
        [0b111, 0b100, 0b111, 0b001, 0b111], // 5
        [0b111, 0b100, 0b111, 0b101, 0b111], // 6
        [0b111, 0b001, 0b001, 0b001, 0b001], // 7
        [0b111, 0b101, 0b111, 0b101, 0b111], // 8
        [0b111, 0b101, 0b111, 0b001, 0b111], // 9
    ];
    (GLYPHS[(digit - 1) as usize][y] & (0b100 >> x)) != 0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<Bar> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle())?;
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;

    let surface = compositor.create_surface(&qh);
    let layer = shell.create_layer_surface(&qh, surface, Layer::Top, Some("prototype-bar"), None);
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_size(0, HEIGHT);
    layer.set_exclusive_zone(HEIGHT as i32);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.commit(); // Empty first commit requests the initial configure.

    let mut pool = SlotPool::new(4, &shm)?;
    // Opt-in client cursor for exercising wl_pointer.set_cursor, including hotspot.
    let cursor_test = if std::env::var("DEVEL_WM_CURSOR_TEST").as_deref() == Ok("1") {
        let surface = compositor.create_surface(&qh);
        let (buffer, pixels) = pool.create_buffer(12, 12, 48, wl_shm::Format::Argb8888)?;
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&0xff00ff00u32.to_le_bytes());
        }
        buffer.attach_to(&surface)?;
        surface.damage_buffer(0, 0, 12, 12);
        surface.commit();
        Some((surface, buffer))
    } else {
        None
    };
    let mut bar = Bar {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: SeatState::new(&globals, &qh),
        shm,
        pool,
        layer,
        pointer: None,
        cursor_test,
        width: 0,
        height: HEIGHT,
        highlighted: false,
        exit: false,
        snapshot: None,
        ipc_bytes: Vec::new(),
    };
    if let Some(path) = std::env::var_os("DEVEL_WM_IPC") {
        let mut stream = UnixStream::connect(&path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "cannot connect to IPC socket {}: {err} (copy DEVEL_WM_IPC from the running compositor's output)",
                    std::path::Path::new(&path).display()
                ),
            )
        })?;
        stream.write_all(b"subscribe\n")?;
        stream.set_nonblocking(true)?;
        event_loop.handle().insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            |_, source, bar| {
                let mut bytes = [0; 4096];
                loop {
                    // SAFETY: the stream remains in this event source for its lifetime.
                    match unsafe { source.get_mut().read(&mut bytes) } {
                        Ok(0) => return Ok(PostAction::Remove),
                        Ok(n) => bar.ipc_bytes.extend_from_slice(&bytes[..n]),
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                        Err(err) => return Err(err),
                    }
                    if bar.ipc_bytes.len() > 65536 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "IPC message too large",
                        ));
                    }
                }
                while let Some(end) = bar.ipc_bytes.iter().position(|&byte| byte == b'\n') {
                    let line: Vec<_> = bar.ipc_bytes.drain(..=end).collect();
                    let snapshot: BarSnapshot = serde_json::from_slice(&line)?;
                    if bar.snapshot.as_ref() != Some(&snapshot) {
                        bar.snapshot = Some(snapshot);
                        bar.draw();
                    }
                }
                Ok(PostAction::Continue)
            },
        )?;
    }
    println!(
        "prototype bar: workspace state via DEVEL_WM_IPC if set; click to toggle color; Ctrl-C to exit"
    );
    while !bar.exit {
        event_loop.dispatch(None, &mut bar)?;
    }
    Ok(())
}

struct Bar {
    registry: RegistryState,
    outputs: OutputState,
    seats: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,
    cursor_test: Option<(wl_surface::WlSurface, Buffer)>,
    width: u32,
    height: u32,
    highlighted: bool,
    exit: bool,
    snapshot: Option<BarSnapshot>,
    ipc_bytes: Vec<u8>,
}

impl Bar {
    fn draw(&mut self) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let width = self.width as i32;
        let height = self.height as i32;
        let (buffer, canvas) = self
            .pool
            .create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)
            .expect("allocate bar buffer");
        let monitor = self.snapshot.as_ref().and_then(|snapshot| {
            let name = std::env::var("DEVEL_WM_OUTPUT").ok();
            snapshot.monitors.iter().find(|monitor| {
                name.as_ref().is_some_and(|name| name == &monitor.name)
                    || name.is_none() && snapshot.active_monitor == Some(monitor.id)
            })
        });
        for (index, pixel) in canvas.chunks_exact_mut(4).enumerate() {
            let x = index % self.width as usize;
            let y = index / self.width as usize;
            // A colored segment makes the orientation and click response easy to see.
            let mut color: u32 = if self.highlighted {
                0xff303c50
            } else {
                0xff202830
            };
            if let Some(monitor) = monitor
                && (8..8 + 9 * 28).contains(&x)
                && (3..29).contains(&y)
            {
                let digit = 1 + (x - 8) / 28;
                let dx = (x - 8) % 28;
                if dx < 25 {
                    let workspace = monitor.workspaces.iter().find(|ws| ws.id == digit as u32);
                    color = if monitor.active_workspace == digit as u32 {
                        0xff5080c0
                    } else if workspace.is_some_and(|ws| ws.occupied) {
                        0xff465665
                    } else {
                        0xff303940
                    };
                    if (10..16).contains(&dx)
                        && (9..19).contains(&y)
                        && digit_pixel(digit as u32, (dx - 10) / 2, (y - 9) / 2)
                    {
                        color = 0xfff0f0f0;
                    }
                    if y >= 26 && workspace.is_some_and(|ws| ws.focused_window.is_some()) {
                        color = 0xffe0a050;
                    }
                }
            }
            pixel.copy_from_slice(&color.to_le_bytes());
        }
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("attach bar buffer");
        self.layer.wl_surface().damage_buffer(0, 0, width, height);
        self.layer.commit();
    }
}

impl CompositorHandler for Bar {
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

impl LayerShellHandler for Bar {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        if width != 0 && height != 0 {
            self.width = width;
            self.height = height;
            println!("bar configured: {width}x{height}");
            self.draw();
        }
    }
}

impl ShmHandler for Bar {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for Bar {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for Bar {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = Some(self.seats.get_pointer(qh, &seat).expect("get pointer"));
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
    }
}

impl PointerHandler for Bar {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface == self.layer.wl_surface()
                && let PointerEventKind::Enter { serial } = event.kind
                && let Some((surface, _buffer)) = &self.cursor_test
            {
                pointer.set_cursor(serial, Some(surface), 4, 5);
            }
            if &event.surface == self.layer.wl_surface()
                && let PointerEventKind::Press { button, .. } = event.kind
            {
                println!(
                    "bar clicked: button {button}, position {:?}",
                    event.position
                );
                self.highlighted = !self.highlighted;
                self.draw();
            }
        }
    }
}

delegate_compositor!(Bar);
delegate_layer!(Bar);
delegate_output!(Bar);
delegate_pointer!(Bar);
delegate_registry!(Bar);
delegate_seat!(Bar);
delegate_shm!(Bar);

impl ProvidesRegistryState for Bar {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_workspace_snapshot_and_digit_glyphs() {
        let state: BarSnapshot = serde_json::from_str(r#"{"version":1,"active_monitor":0,"monitors":[{"id":0,"name":"nested","active_workspace":2,"workspaces":[{"id":1,"occupied":false,"focused_window":null,"windows":[]},{"id":2,"occupied":true,"focused_window":4,"windows":[4]}]}]}"#).unwrap();
        assert_eq!(state.monitors[0].active_workspace, 2);
        assert!(state.monitors[0].workspaces[1].occupied);
        assert!(digit_pixel(2, 0, 0));
        assert!(!digit_pixel(2, 0, 1));
    }
}
