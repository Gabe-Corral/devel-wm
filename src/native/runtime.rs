//! Explicit native client outputs and scoped connector recovery.

use std::{
    cell::RefCell,
    collections::HashSet,
    error::Error,
    ffi::CStr,
    io,
    os::fd::AsFd,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, GbmBufferedSurface},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::{Event as _, InputEvent, KeyState, KeyboardKeyEvent, PointerMotionEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Bind, ImportDma,
            damage::OutputDamageTracker,
            gles::{GlesRenderer, ffi},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::{EventLoop, LoopHandle, RegistrationToken},
        drm::control::{self, Device as _, connector, crtc, plane},
        input::Libinput,
        rustix::{fs::OFlags, io::dup},
        wayland_server::{Display, Resource, backend::GlobalId},
    },
    utils::{DeviceFd, Logical, Point},
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use super::{
    presentation::{self, CleanupReport, GraphicsVt, ManagedDrm, StopSignals},
    restore::AtomicRestore,
};
use crate::{
    LoopData,
    compositor::{Compositor, cursor::CursorElements},
    config::{Keybindings, OutputMode},
    ipc::IpcServer,
    wm::MonitorId,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type NativeSurface = GbmBufferedSurface<GbmAllocator<DrmDeviceFd>, ()>;

fn cursor_on_output(
    position: Point<f64, Logical>,
    geometry: smithay::utils::Rectangle<i32, Logical>,
) -> Option<Point<f64, Logical>> {
    geometry
        .to_f64()
        .contains(position)
        .then(|| position - geometry.loc.to_f64())
}

fn logical_width(physical_width: u16, scale: i32) -> io::Result<i32> {
    let width = i32::from(physical_width);
    width
        .checked_add(scale - 1)
        .map(|width| width / scale)
        .ok_or_else(|| io::Error::other("native output logical width overflow"))
}

fn selected_mode(
    modes: &[control::Mode],
    requested: Option<OutputMode>,
    name: &str,
) -> io::Result<control::Mode> {
    let Some(requested) = requested else {
        return presentation::preferred_mode(modes);
    };
    modes
        .iter()
        .filter(|mode| {
            mode.size() == (requested.width, requested.height)
                && mode.vrefresh() == requested.refresh_hz
        })
        .copied()
        .find(|mode| mode.mode_type().contains(control::ModeTypeFlags::PREFERRED))
        .or_else(|| {
            modes.iter().copied().find(|mode| {
                mode.size() == (requested.width, requested.height)
                    && mode.vrefresh() == requested.refresh_hz
            })
        })
        .ok_or_else(|| {
            io::Error::other(format!(
                "{name} does not advertise {}x{}@{} Hz; available modes: {}",
                requested.width,
                requested.height,
                requested.refresh_hz,
                modes
                    .iter()
                    .map(|mode| format!("{}x{}@{}", mode.size().0, mode.size().1, mode.vrefresh()))
                    .collect::<Vec<_>>()
                    .join(", "),
            ))
        })
}

// A udev notification can precede stable mode data. Disconnect immediately on
// a confirmed urgent change, but require two observations before reconnecting.
fn connector_transition(
    online: bool,
    state: connector::State,
    missing_checks: u8,
    urgent: bool,
) -> (u8, Option<bool>) {
    let connected = match state {
        connector::State::Connected => true,
        connector::State::Disconnected => false,
        connector::State::Unknown => return (0, None),
    };
    if connected == online {
        return (0, None);
    }
    let checks = missing_checks.saturating_add(1);
    if checks < 2 && !(urgent && !connected) {
        (checks, None)
    } else {
        (checks, Some(connected))
    }
}

fn needs_new_connector_scan(known: bool, pending: bool) -> bool {
    !known && !pending
}

#[derive(Clone, Copy)]
struct ProbeRequest {
    handle: connector::Handle,
    kind: ProbeKind,
    epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeKind {
    DiscoveryStatus,
    DiscoveryMode,
    VerifyHead { generation: u64, confirm: bool },
}

impl ProbeKind {
    fn force(self) -> bool {
        self != Self::DiscoveryStatus
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerifyDecision {
    RetryFresh,
    ConfirmDisconnect,
    Disconnect,
    ConfirmReconnect,
    Reconnect,
    KeepOnline,
}

fn verify_decision(
    generation: u64,
    current_generation: u64,
    online: bool,
    confirm: bool,
    state: connector::State,
) -> VerifyDecision {
    if generation != current_generation {
        VerifyDecision::RetryFresh
    } else {
        match (online, state, confirm) {
            (true, connector::State::Disconnected, true) => VerifyDecision::Disconnect,
            (true, connector::State::Disconnected, false) => VerifyDecision::ConfirmDisconnect,
            (false, connector::State::Connected, true) => VerifyDecision::Reconnect,
            (false, connector::State::Connected, false) => VerifyDecision::ConfirmReconnect,
            _ => VerifyDecision::KeepOnline,
        }
    }
}

fn verification_order<T: Copy>(heads: impl IntoIterator<Item = (T, bool)>) -> Vec<T> {
    let heads: Vec<_> = heads.into_iter().collect();
    // A known-offline connector is the likely source of a reconnect event.
    // Process it before slow forced probes of unchanged online outputs.
    heads
        .iter()
        .filter(|(_, online)| !online)
        .chain(heads.iter().filter(|(_, online)| *online))
        .map(|(handle, _)| *handle)
        .collect()
}

struct ProbeReply {
    request: ProbeRequest,
    result: io::Result<connector::Info>,
    elapsed: Duration,
}

/// Read-only DRM queries may block in the driver even with force_probe=false.
/// Keep those queries off the libinput/calloop thread; only the main thread
/// creates scanout surfaces and captures/restores the affected CRTC.
struct DiscoveryWorker {
    requests: Option<mpsc::Sender<ProbeRequest>>,
    replies: mpsc::Receiver<ProbeReply>,
    pending: HashSet<connector::Handle>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DiscoveryWorker {
    fn new(fd: &DeviceFd) -> io::Result<Self> {
        let card = super::ReadOnlyCard(dup(fd.as_fd())?);
        let (tx, rx) = mpsc::channel::<ProbeRequest>();
        let (replies, results) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = stopping.clone();
        let thread = thread::Builder::new()
            .name("native-connector-query".into())
            .spawn(move || {
                for request in rx {
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    let started = Instant::now();
                    let result = card.get_connector(request.handle, request.kind.force());
                    if replies
                        .send(ProbeReply {
                            request,
                            result,
                            elapsed: started.elapsed(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: Some(tx),
            replies: results,
            pending: HashSet::new(),
            stopping,
            thread: Some(thread),
        })
    }

    fn request(
        &mut self,
        handle: connector::Handle,
        kind: ProbeKind,
        epoch: u64,
    ) -> io::Result<()> {
        if !self.pending.insert(handle) {
            return Ok(());
        }
        if self.requests.as_ref().is_some_and(|tx| {
            tx.send(ProbeRequest {
                handle,
                kind,
                epoch,
            })
            .is_ok()
        }) {
            Ok(())
        } else {
            self.pending.remove(&handle);
            Err(io::Error::other("native connector query worker stopped"))
        }
    }

    fn stop(&mut self) -> io::Result<()> {
        self.stopping.store(true, Ordering::Relaxed);
        self.requests.take();
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            return Err(io::Error::other("native connector query worker panicked"));
        }
        Ok(())
    }
}

impl Drop for DiscoveryWorker {
    fn drop(&mut self) {
        if let Err(err) = self.stop() {
            eprintln!("Native connector query cleanup failed: {err}");
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingMotion {
    delta: Point<f64, Logical>,
    time: u32,
}

impl PendingMotion {
    fn add(&mut self, delta: Point<f64, Logical>, time: u32) {
        self.delta = Point::from((self.delta.x + delta.x, self.delta.y + delta.y));
        self.time = time;
    }
}

/// Owned by LoopData while the one-output native client session is running.
struct NativeHead {
    surface: Option<NativeSurface>,
    output: Output,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: control::Mode,
    plane: plane::Handle,
    monitor: MonitorId,
    global: Option<GlobalId>,
    online: bool,
    missing_checks: u8,
    tracker: OutputDamageTracker,
    queued: bool,
    queued_at: Option<Instant>,
    queued_lock_frame: bool,
    first_presented: bool,
    repaint_after_flip: bool,
}

struct OutputSelection<'a> {
    handles: &'a [connector::Handle],
    auto: bool,
}

pub struct NativeOutput {
    heads: Vec<NativeHead>,
    auto: bool,
    keys: Keybindings,
    pending_new: Vec<(connector::Handle, u8)>,
    discovery: Option<DiscoveryWorker>,
    discovery_epoch: u64,
    hotplug_generation: u64,
    discovery_rescan: bool,
    renderer: Option<GlesRenderer>,
    drm: Option<ManagedDrm>,
    vt: Option<GraphicsVt>,
    fd: Option<DeviceFd>,
    session: LibSeatSession,
    input: Option<Libinput>,
    tokens: Vec<RegistrationToken>,
    handle: LoopHandle<'static, LoopData>,
    report: Rc<RefCell<CleanupReport>>,
    pending_motion: Option<PendingMotion>,
    paused: bool,
    failure: Option<String>,
    last_connector_check: Instant,
    retired_globals: Vec<(GlobalId, Instant)>,
}

impl NativeOutput {
    fn has_queued(&self) -> bool {
        self.heads.iter().any(|head| head.queued)
    }

    fn verify_heads_after_change(&mut self) -> io::Result<()> {
        // The non-forced udev status query can retain a stale Connected value
        // for DP-4. Probe existing heads on the read-only worker rather than
        // blocking libinput with an online forced EDID query.
        self.hotplug_generation = self.hotplug_generation.wrapping_add(1);
        let Some(worker) = self.discovery.as_mut() else {
            return Ok(());
        };
        for handle in
            verification_order(self.heads.iter().map(|head| (head.connector, head.online)))
        {
            worker.request(
                handle,
                ProbeKind::VerifyHead {
                    generation: self.hotplug_generation,
                    confirm: false,
                },
                self.discovery_epoch,
            )?;
        }
        Ok(())
    }

    fn verify_offline_heads(&mut self) -> io::Result<()> {
        let Some(worker) = self.discovery.as_mut() else {
            return Ok(());
        };
        for head in self.heads.iter().filter(|head| !head.online) {
            worker.request(
                head.connector,
                ProbeKind::VerifyHead {
                    generation: self.hotplug_generation,
                    confirm: false,
                },
                self.discovery_epoch,
            )?;
        }
        Ok(())
    }

    fn receive_discovery(&mut self, compositor: &mut Compositor) -> Result<()> {
        let Some(_) = self.discovery.as_ref() else {
            return Ok(());
        };
        loop {
            let reply = match self
                .discovery
                .as_mut()
                .expect("discovery worker")
                .replies
                .try_recv()
            {
                Ok(reply) => reply,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other(
                        "native connector query worker exited unexpectedly",
                    )
                    .into());
                }
            };
            self.discovery
                .as_mut()
                .expect("discovery worker")
                .pending
                .remove(&reply.request.handle);
            let ProbeReply {
                request,
                result,
                elapsed,
            } = reply;
            if self.paused || !self.session.is_active() || request.epoch != self.discovery_epoch {
                self.discovery_rescan = true;
                continue;
            }
            let ProbeKind::VerifyHead {
                generation,
                confirm,
            } = request.kind
            else {
                let info = result?;
                let name = presentation::connector_name(&info);
                eprintln!(
                    "Native background {} query for {name}: {elapsed:?}",
                    if request.kind.force() {
                        "mode"
                    } else {
                        "status"
                    }
                );
                if request.kind == ProbeKind::DiscoveryMode {
                    let Some(index) = self
                        .pending_new
                        .iter()
                        .position(|(handle, _)| *handle == request.handle)
                    else {
                        continue;
                    };
                    let (_, attempts) = self.pending_new.remove(index);
                    if info.state() == connector::State::Connected {
                        if info.modes().is_empty() {
                            if attempts < 4 {
                                self.pending_new.push((request.handle, attempts + 1));
                            } else {
                                eprintln!(
                                    "Native output {name} still has no modes; waiting for another udev change"
                                );
                            }
                        } else {
                            let started = Instant::now();
                            self.add_discovered_head(&info, compositor)?;
                            if started.elapsed() >= Duration::from_millis(20) {
                                eprintln!(
                                    "Native newly connected {name} takeover blocked the input loop for {:?}",
                                    started.elapsed()
                                );
                            }
                        }
                    }
                } else if info.state() == connector::State::Connected
                    && !self
                        .heads
                        .iter()
                        .any(|head| head.connector == request.handle)
                    && !self
                        .pending_new
                        .iter()
                        .any(|(handle, _)| *handle == request.handle)
                {
                    println!(
                        "Native output {name} connected; waiting for a stable mode before takeover"
                    );
                    self.pending_new.push((request.handle, 0));
                    self.last_connector_check = Instant::now();
                }
                continue;
            };
            let Some(index) = self
                .heads
                .iter()
                .position(|head| head.connector == request.handle)
            else {
                continue;
            };
            // If another udev event arrived while this ioctl was in flight,
            // sample again rather than acting on a result from the old event.
            if generation != self.hotplug_generation {
                self.discovery.as_mut().expect("discovery worker").request(
                    request.handle,
                    ProbeKind::VerifyHead {
                        generation: self.hotplug_generation,
                        confirm: false,
                    },
                    self.discovery_epoch,
                )?;
                continue;
            }
            let info = match result {
                Ok(info) => info,
                Err(err) => {
                    eprintln!(
                        "Native background forced status query for {} failed: {err}",
                        self.heads[index].output.name()
                    );
                    continue;
                }
            };
            let name = presentation::connector_name(&info);
            eprintln!(
                "Native background forced status query for {name}: {:?} ({elapsed:?}, confirmation: {confirm})",
                info.state()
            );
            match verify_decision(
                generation,
                self.hotplug_generation,
                self.heads[index].online,
                confirm,
                info.state(),
            ) {
                VerifyDecision::ConfirmDisconnect | VerifyDecision::ConfirmReconnect => {
                    self.discovery.as_mut().expect("discovery worker").request(
                        request.handle,
                        ProbeKind::VerifyHead {
                            generation,
                            confirm: true,
                        },
                        self.discovery_epoch,
                    )?;
                }
                VerifyDecision::Disconnect => self.detach_head(index, compositor),
                VerifyDecision::Reconnect => {
                    if info.modes().contains(&self.heads[index].mode) {
                        let started = Instant::now();
                        self.reconnect_head(index, &info, compositor)?;
                        eprintln!(
                            "Native {name} reconnect main-thread setup: {:?}",
                            started.elapsed()
                        );
                    } else {
                        eprintln!(
                            "Native {name} has no configured startup mode yet; waiting for another status check"
                        );
                    }
                }
                VerifyDecision::RetryFresh | VerifyDecision::KeepOnline => {}
            }
        }
        Ok(())
    }

    fn add_discovered_head(
        &mut self,
        info: &connector::Info,
        compositor: &mut Compositor,
    ) -> Result<bool> {
        let name = presentation::connector_name(info);
        let settings = self.keys.output_settings(&name);
        let mode = match selected_mode(info.modes(), settings.mode, &name) {
            Ok(mode) => mode,
            Err(err) => {
                eprintln!("Native output {name} skipped: {err}");
                return Ok(false);
            }
        };
        let reserved: Vec<_> = self.heads.iter().map(|head| head.crtc).collect();
        let drm = self.drm.as_mut().expect("native DRM device");
        let crtc = match presentation::choose_crtc_excluding(drm, info, &reserved) {
            Ok(crtc) => crtc,
            Err(err) => {
                eprintln!("Native output {name} skipped: {err}");
                return Ok(false);
            }
        };
        let gbm = GbmDevice::new(DrmDeviceFd::new(
            self.fd.as_ref().expect("native fd").clone(),
        ))?;
        // Validate a specific candidate before constructing an active surface;
        // its destructor can otherwise clear the CRTC without a restore path.
        let saved = match drm.planes(&crtc)?.primary.iter().find_map(|plane| {
            AtomicRestore::capture(drm.device_fd(), info.handle(), crtc, plane.handle).ok()
        }) {
            Some(saved) => saved,
            None => {
                eprintln!("Native output {name} skipped: no restorable primary plane");
                return Ok(false);
            }
        };
        let plane = saved.plane();
        let surface = match drm.create_surface(crtc, mode, &[info.handle()]) {
            Ok(surface) => surface,
            Err(err) => {
                saved.restore().map_err(|restore| {
                    io::Error::other(format!(
                        "{name} surface creation failed: {err}; restore failed: {restore}"
                    ))
                })?;
                return Err(err.into());
            }
        };
        if surface.plane() != plane {
            drop(surface);
            saved.restore()?;
            return Err(io::Error::other(format!(
                "{name} chose a different primary plane; refusing takeover"
            ))
            .into());
        }
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let surface = match GbmBufferedSurface::<_, ()>::new(
            surface,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Abgr8888],
            self.renderer
                .as_ref()
                .expect("native renderer")
                .egl_context()
                .dmabuf_render_formats()
                .iter()
                .copied(),
        ) {
            Ok(surface) => surface,
            Err(err) => {
                saved.restore().map_err(|restore| {
                    io::Error::other(format!(
                        "{name} GBM setup failed: {err}; restore failed: {restore}"
                    ))
                })?;
                return Err(err.into());
            }
        };
        drm.save_connector(saved);
        let output = Output::new(
            name.clone(),
            PhysicalProperties {
                size: info
                    .size()
                    .map(|(w, h)| (w as i32, h as i32))
                    .unwrap_or((0, 0))
                    .into(),
                subpixel: Subpixel::Unknown,
                make: "devel-wm".into(),
                model: "native".into(),
            },
        );
        output.set_preferred(Mode::from(mode));
        output.change_current_state(
            Some(Mode::from(mode)),
            None,
            Some(OutputScale::Integer(settings.scale)),
            Some((0, 0).into()),
        );
        compositor.space.map_output(&output, (0, 0));
        compositor.register_output(&output);
        let monitor = compositor
            .monitor_for_output(&output)
            .expect("registered output");
        let global = output.create_global::<Compositor>(&compositor.display_handle);
        let tracker = OutputDamageTracker::from_output(&output);
        self.heads.push(NativeHead {
            surface: Some(surface),
            output,
            connector: info.handle(),
            crtc,
            mode,
            plane,
            monitor,
            global: Some(global),
            online: true,
            missing_checks: 0,
            tracker,
            queued: false,
            queued_at: None,
            queued_lock_frame: false,
            first_presented: false,
            repaint_after_flip: false,
        });
        println!(
            "Native output {name} discovered on CRTC {crtc:?}; scale {}",
            settings.scale
        );
        self.reflow_outputs(compositor);
        Ok(true)
    }

    fn reprobe_connectors(
        &mut self,
        compositor: &mut Compositor,
        urgent: bool,
        allow_forced: bool,
    ) -> Result<bool> {
        if self.paused
            || !self.session.is_active()
            || (!urgent
                && !self.heads.iter().any(|head| !head.online)
                && self.pending_new.is_empty()
                && !self.discovery_rescan)
            || (!urgent && self.last_connector_check.elapsed() < Duration::from_millis(750))
        {
            self.reap_retired_globals(compositor);
            return Ok(false);
        }
        let started = Instant::now();
        let settling = !self.pending_new.is_empty();
        self.last_connector_check = Instant::now();
        let mut changed = false;
        if !allow_forced && !urgent {
            self.verify_offline_heads()?;
        }
        // Even get_connector(force=false) can block libinput (238 ms on DP-4
        // reconnect). Normal udev and periodic paths only schedule the worker;
        // synchronous probes remain for exceptional DRM-error recovery.
        let heads_to_probe = if allow_forced { self.heads.len() } else { 0 };
        for index in 0..heads_to_probe {
            let handle = self.heads[index].connector;
            let mut force = !self.heads[index].online;
            let started = Instant::now();
            let mut probe = self
                .drm
                .as_ref()
                .expect("native DRM device")
                .get_connector(handle, force);
            if !force
                && probe
                    .as_ref()
                    .is_ok_and(|info| info.state() != connector::State::Connected)
            {
                // Confirm a possible unplug on the affected head before
                // dropping its surface and invalidating its restore snapshot.
                force = true;
                probe = self
                    .drm
                    .as_ref()
                    .expect("native DRM device")
                    .get_connector(handle, true);
            }
            if started.elapsed() >= Duration::from_millis(20) {
                eprintln!(
                    "Native connector {} {}query blocked the input loop for {:?}",
                    self.heads[index].output.name(),
                    if force { "forced " } else { "status " },
                    started.elapsed()
                );
            }
            let info = match probe {
                Ok(info) => info,
                Err(err) => {
                    tracing::warn!(?handle, %err, "native connector reprobe failed; retaining current state");
                    continue;
                }
            };
            if urgent {
                eprintln!(
                    "Native connector {} after udev/recovery: {:?} (forced: {}, online: {})",
                    self.heads[index].output.name(),
                    info.state(),
                    force,
                    self.heads[index].online,
                );
            }
            let (checks, transition) = connector_transition(
                self.heads[index].online,
                info.state(),
                self.heads[index].missing_checks,
                urgent,
            );
            self.heads[index].missing_checks = checks;
            let Some(connected) = transition else {
                continue;
            };
            if connected {
                // A failed reconnect might have touched this CRTC. End the
                // explicit pair session instead of retrying an unsafe state.
                self.reconnect_head(index, &info, compositor)?;
            } else {
                self.detach_head(index, compositor);
            }
            changed = true;
        }
        let existing_heads = started.elapsed();
        if self.auto && (urgent || self.discovery_rescan) {
            self.discovery_rescan = false;
            let handles = self
                .drm
                .as_ref()
                .expect("native DRM device")
                .resource_handles()?
                .connectors()
                .to_vec();
            for handle in handles {
                if !needs_new_connector_scan(
                    self.heads.iter().any(|head| head.connector == handle),
                    self.pending_new
                        .iter()
                        .any(|(pending, _)| *pending == handle)
                        || self
                            .discovery
                            .as_ref()
                            .is_some_and(|worker| worker.pending.contains(&handle)),
                ) {
                    continue;
                }
                self.discovery
                    .as_mut()
                    .expect("automatic discovery worker")
                    .request(handle, ProbeKind::DiscoveryStatus, self.discovery_epoch)?;
            }
        } else if self.auto {
            for &(handle, _) in &self.pending_new {
                self.discovery
                    .as_mut()
                    .expect("automatic discovery worker")
                    .request(handle, ProbeKind::DiscoveryMode, self.discovery_epoch)?;
            }
        }
        let new_outputs = started.elapsed().saturating_sub(existing_heads);
        self.reap_retired_globals(compositor);
        if urgent || settling {
            eprintln!(
                "Native hotplug timing ({}): existing_heads={existing_heads:?}, new_outputs={new_outputs:?}, total={:?}",
                if urgent { "udev/recovery" } else { "settling" },
                started.elapsed()
            );
        }
        Ok(changed)
    }

    fn reap_retired_globals(&mut self, compositor: &Compositor) {
        let now = Instant::now();
        self.retired_globals.retain(|(global, disabled)| {
            if now.duration_since(*disabled) >= Duration::from_secs(5) {
                compositor
                    .display_handle
                    .remove_global::<Compositor>(global.clone());
                false
            } else {
                true
            }
        });
    }

    fn detach_head(&mut self, index: usize, compositor: &mut Compositor) {
        let head = &mut self.heads[index];
        println!(
            "Native output {} disconnected; transferring windows to a surviving output",
            head.output.name()
        );
        compositor.detach_output(&head.output);
        if let Some(global) = head.global.take() {
            compositor
                .display_handle
                .disable_global::<Compositor>(global.clone());
            self.retired_globals.push((global, Instant::now()));
        }
        // Smithay clears only this disconnected CRTC on surface drop. Forget
        // its saved scanout: replaying it after a hot-unplug is unsafe. Other
        // connectors' snapshots remain owned by ManagedDrm.
        head.surface.take();
        head.queued = false;
        head.queued_at = None;
        head.queued_lock_frame = false;
        head.repaint_after_flip = false;
        head.online = false;
        head.missing_checks = 0;
        self.drm
            .as_mut()
            .expect("native DRM device")
            .forget_connector(head.connector);
        self.reflow_outputs(compositor);
    }

    fn reconnect_head(
        &mut self,
        index: usize,
        info: &connector::Info,
        compositor: &mut Compositor,
    ) -> Result<()> {
        let head = &self.heads[index];
        let drm = self.drm.as_mut().expect("native DRM device");
        if presentation::connector_name(info) != head.output.name()
            || presentation::choose_crtc(drm, info)? != head.crtc
        {
            return Err(
                io::Error::other("connector CRTC changed; refusing hotplug takeover").into(),
            );
        }
        let mode = info
            .modes()
            .iter()
            .copied()
            .find(|mode| *mode == head.mode)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "{} no longer advertises its configured startup mode; refusing reattach",
                    head.output.name()
                ))
            })?;
        let gbm = GbmDevice::new(DrmDeviceFd::new(
            self.fd.as_ref().expect("native fd").clone(),
        ))?;
        // Preflight before creating a Smithay surface: on failure its active
        // destructor would otherwise clear the CRTC without a restore path.
        let saved = AtomicRestore::capture(drm.device_fd(), head.connector, head.crtc, head.plane)?;
        let surface = match drm.create_surface(head.crtc, mode, &[head.connector]) {
            Ok(surface) => surface,
            Err(err) => {
                saved.restore().map_err(|restore| {
                    io::Error::other(format!(
                        "hotplug surface failed: {err}; cleanup failed: {restore}"
                    ))
                })?;
                return Err(err.into());
            }
        };
        if surface.plane() != head.plane {
            let found = surface.plane();
            drop(surface);
            saved.restore().map_err(|restore| {
                io::Error::other(format!(
                    "hotplug plane changed from {:?} to {found:?}; cleanup failed: {restore}",
                    head.plane,
                ))
            })?;
            return Err(
                io::Error::other("hotplug plane changed; refusing connector takeover").into(),
            );
        }
        let allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let surface = match GbmBufferedSurface::<_, ()>::new(
            surface,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Abgr8888],
            self.renderer
                .as_ref()
                .expect("native renderer")
                .egl_context()
                .dmabuf_render_formats()
                .iter()
                .copied(),
        ) {
            Ok(surface) => surface,
            Err(err) => {
                saved.restore().map_err(|restore| {
                    io::Error::other(format!(
                        "hotplug surface failed: {err}; cleanup failed: {restore}"
                    ))
                })?;
                return Err(err.into());
            }
        };
        drm.save_connector(saved);
        let head = &mut self.heads[index];
        head.output.set_preferred(Mode::from(mode));
        head.output
            .change_current_state(Some(Mode::from(mode)), None, None, Some((0, 0).into()));
        compositor.space.map_output(&head.output, (0, 0));
        compositor.reattach_output(&head.output, head.monitor);
        head.global = Some(
            head.output
                .create_global::<Compositor>(&compositor.display_handle),
        );
        head.surface = Some(surface);
        head.tracker = OutputDamageTracker::from_output(&head.output);
        head.online = true;
        head.missing_checks = 0;
        head.first_presented = false;
        println!(
            "Native output {} reconnected; restore preflight passed",
            head.output.name()
        );
        self.reflow_outputs(compositor);
        compositor.needs_repaint = true;
        Ok(())
    }

    fn reflow_outputs(&mut self, compositor: &mut Compositor) {
        let mut x: i32 = 0;
        for head in self.heads.iter_mut().filter(|head| head.online) {
            compositor.move_output(&head.output, x);
            head.tracker = OutputDamageTracker::from_output(&head.output);
            let geometry = compositor
                .space
                .output_geometry(&head.output)
                .expect("mapped native output");
            x = x.saturating_add(geometry.size.w);
        }
        compositor.needs_repaint = true;
    }

    fn queue_motion(&mut self, delta: Point<f64, Logical>, time: u32) {
        if let Some(pending) = self.pending_motion.as_mut() {
            pending.add(delta, time);
        } else {
            self.pending_motion = Some(PendingMotion { delta, time });
        }
    }

    fn flush_motion(&mut self, compositor: &mut Compositor) {
        if let Some(motion) = self.pending_motion.take() {
            let started = compositor.perf.as_ref().map(|_| Instant::now());
            compositor.process_relative_motion(motion.delta, motion.time);
            if let Some(started) = started {
                compositor
                    .perf
                    .as_mut()
                    .expect("performance enabled")
                    .motion_flush(started.elapsed());
            }
        }
    }

    fn repaint(&mut self, compositor: &mut Compositor) -> Result<()> {
        for head in &mut self.heads {
            if !head.online {
                continue;
            }
            if head.queued {
                head.repaint_after_flip = true;
                continue;
            }
            let started = compositor.perf.as_ref().map(|_| Instant::now());
            let renderer = self.renderer.as_mut().expect("native renderer");
            let surface = head.surface.as_mut().expect("native surface");
            let (mut buffer, age) = surface.next_buffer()?;
            let result = {
                let mut framebuffer = renderer.bind(&mut buffer)?;
                let mut cursor = compositor
                    .cursor_position
                    .and_then(|global| {
                        compositor
                            .space
                            .output_geometry(&head.output)
                            .and_then(|geometry| cursor_on_output(global, geometry))
                            .map(|local| (global, local))
                    })
                    .map(|(global, local)| {
                        let scale = head.output.current_scale().fractional_scale();
                        let icon = compositor
                            .column_resize_cursor_at(global)
                            .then_some(smithay::input::pointer::CursorIcon::EwResize);
                        compositor
                            .cursor
                            .elements(renderer, local.to_physical(scale), scale, icon)
                    })
                    .unwrap_or_default();
                if let Some(line) = compositor.drop_indicator_element(&head.output) {
                    cursor.push(CursorElements::Indicator(line));
                }
                if compositor.is_locked() {
                    use smithay::backend::renderer::element::{
                        Kind,
                        surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                    };
                    let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = compositor
                        .lock_surface_for_output(&head.output)
                        .map(|surface| {
                            render_elements_from_surface_tree(
                                renderer,
                                surface.wl_surface(),
                                (0, 0),
                                head.output.current_scale().fractional_scale(),
                                1.0,
                                Kind::Unspecified,
                            )
                        })
                        .unwrap_or_default();
                    head.tracker.render_output(
                        renderer,
                        &mut framebuffer,
                        0,
                        &elements,
                        [0.0, 0.0, 0.0, 1.0],
                    )?
                } else {
                    smithay::desktop::space::render_output::<_, CursorElements<GlesRenderer>, _, _>(
                        &head.output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        age as usize,
                        [&compositor.space],
                        &cursor,
                        &mut head.tracker,
                        [0.1, 0.1, 0.1, 1.0],
                    )?
                }
            };
            let rendered = started.map(|_| Instant::now());
            let damage = result.damage.cloned();
            if let Some(ref damage) = damage {
                let damage = damage.clone();
                surface.queue_buffer(Some(result.sync), Some(damage), ())?;
                head.queued = true;
                head.queued_at = compositor.perf.as_ref().map(|_| Instant::now());
                head.queued_lock_frame = compositor.is_locked();
            } else {
                if compositor.is_locked() {
                    compositor.lock_frame_presented(&head.output);
                }
                compositor.flush_resize_configure();
                Self::send_frames(&head.output, compositor);
            }
            if let Some(started) = started {
                let damaged_pixels = damage.as_ref().map_or(0, |rects| {
                    rects
                        .iter()
                        .map(|rect| (i64::from(rect.size.w) * i64::from(rect.size.h)).max(0) as u64)
                        .sum()
                });
                let output_pixels = head.output.current_mode().map_or(0, |mode| {
                    (i64::from(mode.size.w) * i64::from(mode.size.h)).max(0) as u64
                });
                compositor
                    .perf
                    .as_mut()
                    .expect("performance enabled")
                    .frame(
                        rendered.expect("render timer").duration_since(started),
                        Instant::now().duration_since(rendered.expect("render timer")),
                        damaged_pixels,
                        output_pixels,
                        damage.is_some(),
                    );
            }
        }
        Ok(())
    }

    fn frame_submitted(&mut self, compositor: &mut Compositor, crtc: crtc::Handle) -> Result<()> {
        if !self
            .heads
            .iter()
            .any(|head| head.crtc == crtc && head.queued)
        {
            return Ok(());
        }
        // Capture callback arrival before flushing coalesced input or calling
        // Smithay's completion handler, so those costs are measured separately.
        let callback_at = compositor.perf.as_ref().map(|_| Instant::now());
        self.flush_motion(compositor);
        let head = self
            .heads
            .iter_mut()
            .find(|head| head.crtc == crtc)
            .expect("queued head");
        head.surface
            .as_mut()
            .expect("native surface")
            .frame_submitted()?;
        head.queued = false;
        if let (Some(queued_at), Some(callback_at)) = (head.queued_at.take(), callback_at) {
            compositor
                .perf
                .as_mut()
                .expect("performance enabled")
                .frame_completed(&head.output.name(), callback_at.duration_since(queued_at));
        }
        if head.queued_lock_frame && compositor.is_locked() {
            compositor.lock_frame_presented(&head.output);
        }
        head.queued_lock_frame = false;
        if !head.first_presented {
            head.first_presented = true;
            println!(
                "Native output {} frame presented (CRTC {:?})",
                head.output.name(),
                crtc
            );
        }
        // Input devices can deliver hundreds of resize motions per second.
        // Send the newest requested size at the output's flip cadence rather
        // than enqueueing an obsolete xdg configure for every mouse event.
        compositor.flush_resize_configure();
        Self::send_frames(&head.output, compositor);
        if std::mem::take(&mut head.repaint_after_flip) {
            compositor.needs_repaint = true;
        }
        Ok(())
    }

    fn send_frames(output: &Output, compositor: &mut Compositor) {
        if compositor.is_locked() {
            if let Some(surface) = compositor.lock_surface_for_output(output) {
                smithay::desktop::utils::send_frames_surface_tree(
                    surface.wl_surface(),
                    output,
                    compositor.start_time.elapsed(),
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
            return;
        }
        for window in compositor.space.elements() {
            if !compositor
                .space
                .outputs_for_element(window)
                .contains(output)
            {
                continue;
            }
            window.window.send_frame(
                output,
                compositor.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(
                output,
                compositor.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) =
            &compositor.cursor.image
            && surface.is_alive()
        {
            smithay::desktop::utils::send_frames_surface_tree(
                surface,
                output,
                compositor.start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
    }

    fn pause(&mut self) {
        self.discovery_epoch = self.discovery_epoch.wrapping_add(1);
        self.discovery_rescan = true;
        if let Some(input) = self.input.as_mut() {
            input.suspend();
        }
        if let Some(drm) = self.drm.as_mut() {
            drm.pause();
        }
        // Drop the old surface only after marking the DRM device inactive:
        // Smithay must not clear another session's output on surface drop.
        for head in &mut self.heads {
            head.surface.take();
            head.queued = false;
            head.queued_at = None;
            head.queued_lock_frame = false;
            head.repaint_after_flip = false;
        }
        self.pending_motion = None;
        self.paused = true;
    }

    fn resume(&mut self, compositor: &mut Compositor) -> Result<()> {
        if !self.paused || !self.session.is_active() {
            return Err(io::Error::other("cannot resume an inactive native session").into());
        }
        let drm = self.drm.as_mut().expect("native DRM device");
        if let Err(err) = drm.activate(false) {
            drm.pause();
            return Err(err.into());
        }
        // The previous snapshot belongs to the *previous* takeover. Never
        // replay it after i3 has used the connector in between VT switches.
        drm.saved.take();
        drm.additional_saved.clear();
        let result = (|| -> Result<()> {
            let gbm = GbmDevice::new(DrmDeviceFd::new(
                self.fd.as_ref().expect("native device fd").clone(),
            ))?;
            for head in &mut self.heads {
                if !head.online {
                    continue;
                }
                let info = drm.get_connector(head.connector, false)?;
                if info.state() != connector::State::Connected
                    || presentation::connector_name(&info) != head.output.name()
                {
                    return Err(
                        io::Error::other("native connector changed while on another VT").into(),
                    );
                }
                let mode = info
                    .modes()
                    .iter()
                    .copied()
                    .find(|mode| *mode == head.mode)
                    .ok_or_else(|| {
                        io::Error::other("native configured output mode disappeared while paused")
                    })?;
                if presentation::choose_crtc(drm, &info)? != head.crtc {
                    return Err(io::Error::other(
                        "native output mode or CRTC changed while paused; refusing resume",
                    )
                    .into());
                }
                let surface = drm.create_surface(head.crtc, mode, &[head.connector])?;
                let saved = match AtomicRestore::capture(
                    drm.device_fd(),
                    head.connector,
                    head.crtc,
                    surface.plane(),
                ) {
                    Ok(saved) => saved,
                    Err(err) => {
                        drm.pause(); // before the new surface is dropped
                        return Err(io::Error::other(format!(
                            "native resume restore preflight refused takeover: {err}"
                        ))
                        .into());
                    }
                };
                drm.save_connector(saved);
                let allocator = GbmAllocator::new(
                    gbm.clone(),
                    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                );
                head.surface = Some(GbmBufferedSurface::<_, ()>::new(
                    surface,
                    allocator,
                    &[Fourcc::Argb8888, Fourcc::Abgr8888],
                    self.renderer
                        .as_ref()
                        .expect("native renderer")
                        .egl_context()
                        .dmabuf_render_formats()
                        .iter()
                        .copied(),
                )?);
            }
            Ok(())
        })();
        if let Err(err) = result {
            // If GBM construction failed after a surface was captured, its
            // active destructor may have cleared that CRTC. Restore every
            // captured target; a failed preflight paused DRM *before* drop.
            for head in &mut self.heads {
                head.surface.take();
            }
            let restore_errors = if drm.is_active() {
                drm.saved
                    .iter()
                    .chain(&drm.additional_saved)
                    .filter_map(|saved| saved.restore().err().map(|err| err.to_string()))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            drm.pause();
            drm.saved.take();
            drm.additional_saved.clear();
            if !restore_errors.is_empty() {
                return Err(io::Error::other(format!(
                    "{err}; native resume cleanup failed: {}",
                    restore_errors.join("; ")
                ))
                .into());
            }
            return Err(err);
        }
        self.input
            .as_mut()
            .expect("native input")
            .resume()
            .map_err(|()| io::Error::other("libinput could not resume native input devices"))?;
        for head in &mut self.heads {
            if !head.online {
                continue;
            }
            head.tracker = OutputDamageTracker::from_output(&head.output);
            head.first_presented = false;
        }
        self.paused = false;
        compositor.needs_repaint = true;
        println!("Native session reactivated; requesting fresh frame");
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(input) = self.input.take() {
            input.suspend();
        }
        for token in self.tokens.drain(..) {
            self.handle.remove(token);
        }
        // Finish any read-only ioctl before libseat closes its underlying fd.
        let query = self
            .discovery
            .take()
            .map(|mut worker| worker.stop())
            .transpose();
        for head in &mut self.heads {
            head.surface.take();
        }
        self.renderer.take();
        self.drm.take(); // scoped restore, then suppress Smithay's card-wide replay
        let display = match &*self.report.borrow() {
            CleanupReport::Failed(err) => Some(err.clone()),
            _ => None,
        };
        let console = self.vt.as_mut().map(GraphicsVt::restore).transpose();
        self.vt.take();
        let fd = self.fd.take().expect("native device fd");
        let owned = fd
            .try_into()
            .map_err(|_| io::Error::other("native session retained DRM fd"))?;
        let close = self.session.close(owned);
        if let Some(err) = display {
            return Err(io::Error::other(err).into());
        }
        console?;
        close?;
        query?;
        Ok(())
    }
}

impl Drop for NativeOutput {
    fn drop(&mut self) {
        if self.fd.is_some()
            && let Err(err) = self.shutdown()
        {
            eprintln!("Native session cleanup failed: {err}");
        }
    }
}

fn initialize(
    event_loop: &mut EventLoop<'static, LoopData>,
    data: &mut LoopData,
    mut session: LibSeatSession,
    connector_names: &[&str],
    keys: Keybindings,
) -> Result<()> {
    let auto = connector_names.is_empty();
    let (path, handles) = if auto {
        presentation::find_connected_card(&mut session)?
    } else {
        let mut path = None;
        let mut handles = Vec::new();
        for name in connector_names {
            let (card, handle) = presentation::find_target(&mut session, name)?;
            if path.as_ref().is_some_and(|path| path != &card) || handles.contains(&handle) {
                return Err(io::Error::other(
                    "native outputs must be distinct connectors on the same DRM card",
                )
                .into());
            }
            path = Some(card);
            handles.push(handle);
        }
        (
            path.ok_or_else(|| io::Error::other("native session needs at least one output"))?,
            handles,
        )
    };
    // Prepare DRM card change notifications before opening the device, so a
    // pair session cannot start without a hotplug trigger. Reprobing a live
    // connector continuously can stall libinput on this same event loop.
    let hotplug = if auto || handles.len() > 1 {
        let backend = UdevBackend::new(session.seat())?;
        let device_id = backend
            .device_list()
            .find_map(|(id, card)| (card == path).then_some(id))
            .ok_or_else(|| io::Error::other("native DRM card missing from seat udev monitor"))?;
        Some((backend, device_id))
    } else {
        None
    };
    let fd = session.open(&path, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NONBLOCK)?;
    let fd = DeviceFd::from(fd);
    let result = initialize_device(
        event_loop,
        data,
        session.clone(),
        OutputSelection {
            handles: &handles,
            auto,
        },
        keys,
        fd.clone(),
        hotplug,
    );
    if result.is_err() {
        // All references created during initialization must be gone before
        // returning the original descriptor to libseat.
        let owned = fd.try_into().map_err(|_| {
            io::Error::other("native client startup retained the DRM device on failure")
        })?;
        session.close(owned)?;
    }
    result
}

fn initialize_device(
    event_loop: &mut EventLoop<'static, LoopData>,
    data: &mut LoopData,
    session: LibSeatSession,
    selection: OutputSelection<'_>,
    keys: Keybindings,
    fd: DeviceFd,
    hotplug: Option<(UdevBackend, u64)>,
) -> Result<()> {
    let OutputSelection { handles, auto } = selection;
    let drm_fd = DrmDeviceFd::new(fd.clone());
    let (device, notifier) = DrmDevice::new(drm_fd.clone(), false)?;
    let report = Rc::new(RefCell::new(CleanupReport::NotModified));
    let mut drm = ManagedDrm {
        device,
        session: session.clone(),
        saved: None,
        additional_saved: Vec::new(),
        report: report.clone(),
    };
    if !drm.is_atomic() {
        return Err(io::Error::other("native client requires atomic DRM").into());
    }
    let mut targets = Vec::new();
    let mut logical_total: i32 = 0;
    for &handle in handles {
        let info = drm.get_connector(handle, false)?;
        if info.state() != connector::State::Connected {
            return Err(io::Error::other("native connector disconnected").into());
        }
        let name = presentation::connector_name(&info);
        let settings = keys.output_settings(&name);
        let mode = selected_mode(info.modes(), settings.mode, &name)?;
        let reserved: Vec<_> = targets.iter().map(|(_, _, used, _)| *used).collect();
        let crtc = match presentation::choose_crtc_excluding(&drm, &info, &reserved) {
            Ok(crtc) => crtc,
            Err(err) if auto && !targets.is_empty() => {
                eprintln!("Native output {name} skipped: {err}");
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        logical_total = logical_total
            .checked_add(logical_width(mode.size().0, settings.scale)?)
            .ok_or_else(|| io::Error::other("native output layout width overflow"))?;
        targets.push((info, mode, crtc, settings));
    }
    let gbm = GbmDevice::new(drm_fd)?;
    // SAFETY: the EGL display keeps the GBM device alive.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let egl_device = EGLDevice::device_for_display(&display)?;
    if egl_device.is_software() {
        return Err(io::Error::other("native EGL renderer is software").into());
    }
    let context = EGLContext::new(&display)?;
    // SAFETY: the context is owned by GlesRenderer on this thread.
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let name = renderer.with_context(|gl| unsafe {
        // SAFETY: with_context makes the context current during this query.
        CStr::from_ptr(gl.GetString(ffi::RENDERER).cast())
            .to_string_lossy()
            .into_owned()
    })?;
    println!("Native client GLES renderer: {name}");
    if ["llvmpipe", "softpipe"]
        .iter()
        .any(|software| name.to_ascii_lowercase().contains(software))
    {
        return Err(io::Error::other("native renderer is software").into());
    }
    let render_dev = egl_device
        .try_get_render_node()?
        .map(|node| node.dev_id())
        .unwrap_or(drm.device_fd().dev_id()?);
    let feedback = DmabufFeedbackBuilder::new(render_dev, renderer.dmabuf_formats()).build()?;
    data.compositor
        .dmabuf_state
        .create_global_with_default_feedback::<Compositor>(&data.display_handle, &feedback);
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&session.seat())
        .map_err(|()| io::Error::other("libinput could not attach to the native seat"))?;
    // The worker also verifies already-online heads after udev, even in
    // explicitly named sessions. Keep forced DRM queries off the input loop.
    let discovery = Some(DiscoveryWorker::new(&fd)?);
    let vt = GraphicsVt::enter()?;
    let mut heads = Vec::new();
    let mut origin_x: i32 = 0;
    for (info, mode, crtc, settings) in targets {
        let handle = info.handle();
        let next_x = origin_x
            .checked_add(logical_width(mode.size().0, settings.scale)?)
            .expect("layout width prevalidated before entering graphics mode");
        let surface = match drm.create_surface(crtc, mode, &[handle]) {
            Ok(surface) => surface,
            Err(err) => {
                drm.pause();
                return Err(err.into());
            }
        };
        let plane = surface.plane();
        match AtomicRestore::capture(drm.device_fd(), handle, crtc, plane) {
            Ok(saved) => {
                drm.save_connector(saved);
            }
            Err(err) => {
                drm.pause();
                return Err(io::Error::other(format!(
                    "native restore preflight refused client session: {err}"
                ))
                .into());
            }
        }
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let surface = GbmBufferedSurface::<_, ()>::new(
            surface,
            allocator,
            &[Fourcc::Argb8888, Fourcc::Abgr8888],
            renderer
                .egl_context()
                .dmabuf_render_formats()
                .iter()
                .copied(),
        )?;
        let wl_mode = Mode::from(mode);
        let output = Output::new(
            presentation::connector_name(&info),
            PhysicalProperties {
                size: info
                    .size()
                    .map(|(w, h)| (w as i32, h as i32))
                    .unwrap_or((0, 0))
                    .into(),
                subpixel: Subpixel::Unknown,
                make: "devel-wm".into(),
                model: "native".into(),
            },
        );
        let global = output.create_global::<Compositor>(&data.display_handle);
        output.set_preferred(wl_mode);
        output.change_current_state(
            Some(wl_mode),
            None,
            Some(OutputScale::Integer(settings.scale)),
            Some((origin_x, 0).into()),
        );
        data.compositor.space.map_output(&output, (origin_x, 0));
        println!(
            "Native output {} mapped at ({origin_x}, 0) with mode {}x{}@{} Hz, scale {} (logical width {})",
            output.name(),
            wl_mode.size.w,
            wl_mode.size.h,
            mode.vrefresh(),
            settings.scale,
            next_x - origin_x,
        );
        data.compositor.register_output(&output);
        let monitor = data
            .compositor
            .monitor_for_output(&output)
            .expect("registered output");
        origin_x = next_x;
        let tracker = OutputDamageTracker::from_output(&output);
        heads.push(NativeHead {
            surface: Some(surface),
            output,
            connector: handle,
            crtc,
            mode,
            plane,
            monitor,
            global: Some(global),
            online: true,
            missing_checks: 0,
            tracker,
            queued: false,
            queued_at: None,
            queued_lock_frame: false,
            first_presented: false,
            repaint_after_flip: false,
        });
    }
    let drm_token =
        event_loop
            .handle()
            .insert_source(notifier, move |event, _, data| match event {
                DrmEvent::VBlank(flipped) => {
                    if data.native.as_ref().is_some_and(|native| native.paused) {
                        return;
                    }
                    let started = data.compositor.perf.as_ref().map(|_| Instant::now());
                    if let Some(native) = data.native.as_mut()
                        && let Err(err) = native.frame_submitted(&mut data.compositor, flipped)
                    {
                        eprintln!("Native frame completion failed: {err}");
                        match native.reprobe_connectors(&mut data.compositor, true, true) {
                            Ok(true) => println!("Native frame error followed a connector disconnect; survivor remains active"),
                            Ok(false) => {
                                native.failure = Some(err.to_string());
                                data.compositor.loop_signal.stop();
                            }
                            Err(reprobe) => {
                                native.failure = Some(format!("{err}; hotplug recovery failed: {reprobe}"));
                                data.compositor.loop_signal.stop();
                            }
                        }
                    }
                    if let Some(started) = started {
                        data.compositor
                            .perf
                            .as_mut()
                            .expect("performance enabled")
                            .flip(started.elapsed());
                    }
                }
                DrmEvent::Error(err) => {
                    if data.native.as_ref().is_some_and(|native| native.paused) {
                        return;
                    }
                    eprintln!("Native DRM error: {err}");
                    if let Some(native) = data.native.as_mut() {
                        match native.reprobe_connectors(&mut data.compositor, true, true) {
                            Ok(true) => return,
                            Ok(false) => native.failure = Some(err.to_string()),
                            Err(reprobe) => native.failure = Some(format!("{err}; hotplug recovery failed: {reprobe}")),
                        }
                    }
                    data.compositor.loop_signal.stop();
                }
            });
    let drm_token = match drm_token {
        Ok(token) => token,
        Err(err) => {
            drm.pause();
            return Err(err.into());
        }
    };
    let backend = LibinputInputBackend::new(libinput.clone());
    let input_token = event_loop
        .handle()
        .insert_source(backend, |event, _, data| {
            let started = data.compositor.perf.as_ref().map(|_| Instant::now());
            if let InputEvent::PointerMotion { event, .. } = &event {
                if let Some(native) = data.native.as_mut() {
                    native.queue_motion(event.delta(), event.time_msec());
                }
                if let Some(started) = started {
                    data.compositor
                        .perf
                        .as_mut()
                        .expect("performance enabled")
                        .input(started.elapsed());
                }
                return;
            }
            if let Some(native) = data.native.as_mut() {
                // Preserve event ordering and pointer hit testing for clicks,
                // scroll, absolute motion and keyboard shortcuts.
                native.flush_motion(&mut data.compositor);
            }
            if let InputEvent::Keyboard { event, .. } = &event
                && !data.compositor.is_locked()
                && event.state() == KeyState::Pressed
                && event.key_code() == smithay::input::keyboard::xkb::Keycode::new(22)
                && data.compositor.seat.get_keyboard().is_some_and(|keyboard| {
                    let mods = keyboard.modifier_state();
                    mods.ctrl && mods.alt
                })
            {
                println!("Native exit shortcut Ctrl+Alt+Backspace");
                data.compositor.loop_signal.stop();
                return;
            }
            data.compositor.process_input_event(event);
            if let Some(started) = started {
                data.compositor
                    .perf
                    .as_mut()
                    .expect("performance enabled")
                    .input(started.elapsed());
            }
        });
    let input_token = match input_token {
        Ok(token) => token,
        Err(err) => {
            event_loop.handle().remove(drm_token);
            drm.pause();
            return Err(err.into());
        }
    };
    let hotplug_token = if let Some((backend, device_id)) = hotplug {
        match event_loop
            .handle()
            .insert_source(backend, move |event, _, data| match event {
                UdevEvent::Changed { device_id: changed } if changed == device_id => {
                    if let Some(native) = data.native.as_mut() {
                        let started = Instant::now();
                        let result = native
                            .reprobe_connectors(&mut data.compositor, true, false)
                            .and_then(|_| native.verify_heads_after_change().map_err(Into::into));
                        eprintln!("Native udev change callback: {:?}", started.elapsed());
                        if let Err(err) = result {
                            native.failure = Some(format!("native hotplug recovery failed: {err}"));
                            data.compositor.loop_signal.stop();
                        }
                    }
                }
                UdevEvent::Removed { device_id: removed } if removed == device_id => {
                    if let Some(native) = data.native.as_mut() {
                        native.failure = Some("native DRM card removed".into());
                    }
                    data.compositor.loop_signal.stop();
                }
                _ => {}
            }) {
            Ok(token) => Some(token),
            Err(err) => {
                event_loop.handle().remove(input_token);
                event_loop.handle().remove(drm_token);
                drm.pause();
                return Err(err.into());
            }
        }
    } else {
        None
    };
    let mut tokens = vec![drm_token, input_token];
    tokens.extend(hotplug_token);
    data.native = Some(NativeOutput {
        heads,
        auto,
        keys,
        pending_new: Vec::new(),
        discovery,
        discovery_epoch: 0,
        hotplug_generation: 0,
        discovery_rescan: false,
        renderer: Some(renderer),
        drm: Some(drm),
        vt: Some(vt),
        fd: Some(fd),
        session,
        input: Some(libinput),
        tokens,
        handle: event_loop.handle(),
        report,
        pending_motion: None,
        paused: false,
        failure: None,
        last_connector_check: Instant::now(),
        retired_globals: Vec::new(),
    });
    data.compositor.needs_repaint = true;
    Ok(())
}

/// Opt-in single-output session. Does not start clients or impose a timer.
pub fn run_session(connector: &str, keys: Keybindings) -> Result<()> {
    run_native_session(&[connector], keys)
}

/// Two explicit connectors on the same DRM card; never auto-select another output.
pub fn run_session_pair(first: &str, second: &str, keys: Keybindings) -> Result<()> {
    run_native_session(&[first, second], keys)
}

/// Opt-in unlimited session with automatic output selection on one seat GPU.
pub fn run_session_auto(keys: Keybindings) -> Result<()> {
    run_native_session(&[], keys)
}

fn run_native_session(connectors: &[&str], keys: Keybindings) -> Result<()> {
    super::require_text_vt()?;
    let signals = StopSignals::new()?;
    let (session, notifier) = LibSeatSession::new()?;
    if !session.is_active() {
        return Err(io::Error::other("libseat is not active on this VT").into());
    }
    let mut event_loop: EventLoop<LoopData> = EventLoop::try_new()?;
    let display = Display::<Compositor>::new()?;
    let display_handle = display.handle();
    let output_keys = keys.clone();
    let mut compositor = Compositor::new(&mut event_loop, display, keys, &session.seat())?;
    compositor.coalesce_resize_configures = true;
    let mut data = LoopData {
        compositor,
        display_handle,
        nested: None,
        native: None,
        ipc: None,
    };
    event_loop
        .handle()
        .insert_source(notifier, move |event, _, data| match event {
            SessionEvent::PauseSession => {
                if let Some(native) = data.native.as_mut() {
                    native.pause();
                }
                println!("Native session paused; waiting for VT reactivation without modesetting");
            }
            SessionEvent::ActivateSession => {
                if let Some(native) = data.native.as_mut()
                    && native.paused
                    && let Err(err) = native.resume(&mut data.compositor)
                {
                    eprintln!("Native VT resume failed: {err}");
                    native.failure = Some(format!("native VT resume failed: {err}"));
                    data.compositor.loop_signal.stop();
                }
            }
        })?;
    initialize(&mut event_loop, &mut data, session, connectors, output_keys)?;
    let ipc = IpcServer::start(&data.compositor.ipc_snapshot())?;
    println!("DEVEL_WM_IPC={}", ipc.path().display());
    data.ipc = Some(ipc);
    let socket = data.compositor.socket_name.clone();
    println!("WAYLAND_DISPLAY={}", socket.to_string_lossy());
    println!(
        "Native session on {}: WAYLAND_DISPLAY={}; use the configured terminal shortcut to open a client; Ctrl+Alt+Backspace exits",
        if connectors.is_empty() {
            "automatically discovered outputs".into()
        } else {
            connectors.join(", ")
        },
        socket.to_string_lossy()
    );
    let mut last_cleanup = Instant::now();
    let stop = signals.stop.clone();
    let run = event_loop.run(Some(Duration::from_millis(50)), &mut data, move |data| {
        let measured = data.compositor.perf.is_some();
        let refresh_start = measured.then(Instant::now);
        if stop.load(Ordering::Relaxed) {
            data.compositor.loop_signal.stop();
            return;
        }
        if !data
            .native
            .as_ref()
            .is_some_and(|native| native.session.is_active() || native.paused)
        {
            data.compositor.loop_signal.stop();
            return;
        }
        if data.native.as_ref().is_some_and(|native| native.paused) {
            return;
        }
        if let Some(native) = data.native.as_mut()
            && let Err(err) = native
                .receive_discovery(&mut data.compositor)
                .and_then(|()| {
                    native
                        .reprobe_connectors(&mut data.compositor, false, false)
                        .map(|_| ())
                })
        {
            native.failure = Some(format!("native hotplug recovery failed: {err}"));
            data.compositor.loop_signal.stop();
            return;
        }
        let old_count = data.compositor.space.elements().len();
        data.compositor.space.refresh();
        let refreshed = measured.then(Instant::now);
        if data.compositor.space.elements().len() != old_count
            || last_cleanup.elapsed() > Duration::from_secs(5)
        {
            data.compositor.cleanup_windows();
            data.compositor.reap_launched_children();
            last_cleanup = Instant::now();
        }
        if let Some(snapshot) = data.compositor.take_ipc_update()
            && let Some(ipc) = &data.ipc
        {
            ipc.publish(&snapshot);
        }
        data.compositor.popups.cleanup();
        let cleaned = measured.then(Instant::now);
        if let Err(err) = data.display_handle.flush_clients() {
            data.native.as_mut().expect("native output").failure = Some(err.to_string());
            data.compositor.loop_signal.stop();
        }
        if let Some(native) = data.native.as_mut()
            && !native.has_queued()
        {
            native.flush_motion(&mut data.compositor);
        }
        let flushed = measured.then(Instant::now);
        let redraw = data.compositor.needs_repaint;
        if data.compositor.needs_repaint {
            data.compositor.needs_repaint = false;
            if let Some(native) = data.native.as_mut()
                && let Err(err) = native.repaint(&mut data.compositor)
            {
                match native.reprobe_connectors(&mut data.compositor, true, true) {
                    Ok(true) => println!(
                        "Native repaint error followed a connector disconnect; repainting survivor"
                    ),
                    Ok(false) => {
                        native.failure = Some(err.to_string());
                        data.compositor.loop_signal.stop();
                    }
                    Err(reprobe) => {
                        native.failure = Some(format!("{err}; hotplug recovery failed: {reprobe}"));
                        data.compositor.loop_signal.stop();
                    }
                }
            }
        }
        if let Some(perf) = data.compositor.perf.as_mut() {
            perf.event(
                refreshed
                    .expect("refresh timer")
                    .duration_since(refresh_start.expect("event timer")),
                cleaned
                    .expect("cleanup timer")
                    .duration_since(refreshed.expect("refresh timer")),
                flushed
                    .expect("flush timer")
                    .duration_since(cleaned.expect("cleanup timer")),
                redraw,
            );
        }
    });
    println!("Native session stopping; disconnecting clients after output cleanup");
    // A queued first flip can still be outstanding when a stop signal
    // fires. Drain it before dropping scanout buffers and restoring the
    // previous state, as in the one-shot presentation probe.
    let flip_deadline = Instant::now() + Duration::from_secs(3);
    let mut drain_error = None;
    while data.native.as_ref().is_some_and(|native| {
        native.has_queued()
            && !native.paused
            && native.session.is_active()
            && Instant::now() < flip_deadline
    }) {
        if let Err(err) = event_loop.dispatch(Some(Duration::from_millis(50)), &mut data) {
            drain_error = Some(err.to_string());
            break;
        }
    }
    if data
        .native
        .as_ref()
        .is_some_and(|native| native.has_queued() && !native.paused && native.session.is_active())
        && drain_error.is_none()
    {
        drain_error = Some("native page-flip completion timed out during cleanup".into());
    }
    let failure = data
        .native
        .as_mut()
        .and_then(|native| native.failure.take());
    let shutdown = data.native.take().expect("native output").shutdown();
    let errors = [
        run.err().map(|err| err.to_string()),
        drain_error,
        failure,
        shutdown.err().map(|err| err.to_string()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(io::Error::other(errors.join("; ")).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_worker_returns_errors_without_retaining_the_session_fd() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = DeviceFd::from(std::os::fd::OwnedFd::from(file));
        let mut worker = DiscoveryWorker::new(&fd).unwrap();
        let handle = connector::Handle::from(std::num::NonZeroU32::new(1).unwrap());
        worker
            .request(handle, ProbeKind::DiscoveryStatus, 7)
            .unwrap();
        let reply = worker.replies.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(reply.request.handle, handle);
        assert_eq!(reply.request.epoch, 7);
        assert!(reply.result.is_err());
        worker.stop().unwrap();
        let owned: std::result::Result<std::os::fd::OwnedFd, _> = fd.try_into();
        assert!(owned.is_ok());
    }

    #[test]
    fn background_verification_requires_two_current_forced_transition_reports() {
        use connector::State::{Connected, Disconnected, Unknown};
        assert!(!ProbeKind::DiscoveryStatus.force());
        assert!(ProbeKind::DiscoveryMode.force());
        assert!(
            ProbeKind::VerifyHead {
                generation: 1,
                confirm: false
            }
            .force()
        );
        assert_eq!(
            verify_decision(1, 2, true, true, Disconnected),
            VerifyDecision::RetryFresh
        );
        assert_eq!(
            verify_decision(2, 2, true, false, Connected),
            VerifyDecision::KeepOnline
        );
        assert_eq!(
            verify_decision(2, 2, true, true, Unknown),
            VerifyDecision::KeepOnline
        );
        assert_eq!(
            verify_decision(2, 2, true, false, Disconnected),
            VerifyDecision::ConfirmDisconnect
        );
        assert_eq!(
            verify_decision(2, 2, true, true, Disconnected),
            VerifyDecision::Disconnect
        );
        assert_eq!(
            verify_decision(2, 2, false, false, Connected),
            VerifyDecision::ConfirmReconnect
        );
        assert_eq!(
            verify_decision(2, 2, false, true, Connected),
            VerifyDecision::Reconnect
        );
        assert_eq!(
            verify_decision(2, 2, false, true, Disconnected),
            VerifyDecision::KeepOnline
        );
    }

    #[test]
    fn reconnect_verification_checks_offline_heads_before_online_heads() {
        assert_eq!(
            verification_order([(1, true), (2, false), (3, true), (4, false)]),
            [2, 4, 1, 3]
        );
    }

    #[test]
    fn hotplug_status_decisions_preserve_unchanged_heads_and_debounce_reconnects() {
        use connector::State::{Connected, Disconnected, Unknown};

        assert_eq!(connector_transition(true, Connected, 1, true), (0, None));
        assert_eq!(connector_transition(true, Unknown, 1, true), (0, None));
        // A udev disconnect is confirmed by the caller's fresh probe and can
        // detach immediately so we never keep drawing to an absent output.
        assert_eq!(
            connector_transition(true, Disconnected, 0, true),
            (1, Some(false))
        );
        assert_eq!(
            connector_transition(true, Disconnected, 0, false),
            (1, None)
        );
        assert_eq!(
            connector_transition(true, Disconnected, 1, false),
            (2, Some(false))
        );
        assert_eq!(
            connector_transition(false, Disconnected, 2, true),
            (0, None)
        );
        assert_eq!(connector_transition(false, Connected, 0, true), (1, None));
        assert_eq!(
            connector_transition(false, Connected, 1, false),
            (2, Some(true))
        );
        assert_eq!(
            connector_transition(false, Connected, u8::MAX, false),
            (u8::MAX, Some(true))
        );
        assert!(needs_new_connector_scan(false, false));
        assert!(!needs_new_connector_scan(true, false));
        assert!(!needs_new_connector_scan(false, true));
    }

    #[test]
    fn integer_scale_sets_logical_layout_and_rejects_missing_mode() {
        assert_eq!(logical_width(3840, 2).unwrap(), 1920);
        assert_eq!(logical_width(1921, 2).unwrap(), 961);
        let hdmi = Output::new(
            "HDMI-A-2".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        hdmi.change_current_state(
            Some(Mode {
                size: (3840, 2160).into(),
                refresh: 60_000,
            }),
            None,
            Some(OutputScale::Integer(2)),
            Some((0, 0).into()),
        );
        let dp = Output::new(
            "DP-4".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "virtual".into(),
            },
        );
        dp.change_current_state(
            Some(Mode {
                size: (1920, 1080).into(),
                refresh: 144_000,
            }),
            None,
            Some(OutputScale::Integer(1)),
            Some((1920, 0).into()),
        );
        let mut space = smithay::desktop::Space::<smithay::desktop::Window>::default();
        space.map_output(&hdmi, (0, 0));
        space.map_output(&dp, (1920, 0));
        assert_eq!(
            space.output_geometry(&hdmi).unwrap().size,
            (1920, 1080).into()
        );
        assert_eq!(space.output_geometry(&dp).unwrap().loc.x, 1920);
        assert_eq!(
            cursor_on_output((1920.0, 100.0).into(), space.output_geometry(&dp).unwrap()),
            Some((0.0, 100.0).into())
        );
        assert!(
            selected_mode(
                &[],
                Some(OutputMode {
                    width: 1920,
                    height: 1080,
                    refresh_hz: 144
                }),
                "DP-4"
            )
            .is_err()
        );
    }

    #[test]
    fn cursor_uses_output_local_coordinates_only_on_its_own_output() {
        let hdmi = smithay::utils::Rectangle::new((0, 0).into(), (3840, 2160).into());
        let dp = smithay::utils::Rectangle::new((3840, 0).into(), (1920, 1080).into());
        assert_eq!(
            cursor_on_output((3839.0, 400.0).into(), hdmi),
            Some((3839.0, 400.0).into())
        );
        assert_eq!(cursor_on_output((3840.0, 400.0).into(), hdmi), None);
        assert_eq!(
            cursor_on_output((3840.0, 400.0).into(), dp),
            Some((0.0, 400.0).into())
        );
        assert_eq!(
            cursor_on_output((4000.0, 400.0).into(), dp),
            Some((160.0, 400.0).into())
        );
        assert_eq!(cursor_on_output((4000.0, 1500.0).into(), dp), None);

        let scaled_hdmi = smithay::utils::Rectangle::new((0, 0).into(), (1920, 1080).into());
        let adjacent_dp = smithay::utils::Rectangle::new((1920, 0).into(), (1920, 1080).into());
        assert_eq!(
            cursor_on_output((1600.0, 500.0).into(), scaled_hdmi)
                .unwrap()
                .to_physical(2.0),
            smithay::utils::Point::from((3200.0, 1000.0))
        );
        assert_eq!(cursor_on_output((1600.0, 500.0).into(), adjacent_dp), None);
        assert_eq!(
            cursor_on_output((2000.0, 500.0).into(), adjacent_dp)
                .unwrap()
                .to_physical(1.0),
            smithay::utils::Point::from((80.0, 500.0))
        );
    }

    #[test]
    fn accumulated_relative_motion_keeps_total_delta_and_latest_time() {
        let mut motion = PendingMotion {
            delta: (2.5, -1.0).into(),
            time: 10,
        };
        motion.add((-0.5, 4.0).into(), 12);
        assert_eq!(motion.delta, (2.0, 3.0).into());
        assert_eq!(motion.time, 12);
    }
}
