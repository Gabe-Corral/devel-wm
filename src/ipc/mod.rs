//! Read-only, newline-delimited JSON snapshots for local bar clients.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde::Serialize;

use crate::wm::{MonitorId, WmState};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Snapshot {
    pub version: u32,
    pub active_monitor: Option<u64>,
    pub monitors: Vec<MonitorSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MonitorSnapshot {
    pub id: u64,
    pub name: String,
    pub active_workspace: u32,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkspaceSnapshot {
    pub id: u32,
    pub occupied: bool,
    pub focused_window: Option<u64>,
    pub windows: Vec<u64>,
}

impl Snapshot {
    pub fn from_wm(wm: &WmState, mut output_name: impl FnMut(MonitorId) -> String) -> Self {
        Self {
            version: 1,
            active_monitor: wm.active_monitor().map(MonitorId::number),
            monitors: wm
                .monitors()
                .map(|monitor| MonitorSnapshot {
                    id: monitor.number(),
                    name: output_name(monitor),
                    active_workspace: wm
                        .active_workspace(monitor)
                        .expect("monitor has active workspace")
                        .number(),
                    workspaces: wm
                        .workspaces(monitor)
                        .map(|workspace| {
                            let tiled = wm.tiled_windows(monitor, workspace).expect("workspace");
                            let floating =
                                wm.floating_windows(monitor, workspace).expect("workspace");
                            let windows: Vec<_> =
                                tiled.iter().chain(floating).map(|id| id.number()).collect();
                            WorkspaceSnapshot {
                                id: workspace.number(),
                                occupied: !windows.is_empty(),
                                focused_window: wm
                                    .focused_window(monitor, workspace)
                                    .map(|id| id.number()),
                                windows,
                            }
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

struct Shared {
    line: String,
    subscribers: Vec<SyncSender<()>>,
}

impl Shared {
    fn update(&mut self, line: String) {
        if self.line == line {
            return;
        }
        self.line = line;
        self.subscribers.retain(|subscriber| {
            !matches!(subscriber.try_send(()), Err(TrySendError::Disconnected(_)))
        });
    }
}

/// The listener never accesses the compositor; the event loop publishes snapshots.
pub struct IpcServer {
    path: PathBuf,
    shared: Arc<Mutex<Shared>>,
    stopping: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

impl IpcServer {
    pub fn start(initial: &Snapshot) -> io::Result<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is unset"))?;
        Self::start_in(&PathBuf::from(runtime), initial)
    }

    fn start_in(runtime: &Path, initial: &Snapshot) -> io::Result<Self> {
        let meta = fs::metadata(runtime)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "XDG_RUNTIME_DIR must be private and owned by this user",
            ));
        }
        let dir = runtime.join("devel-wm");
        match fs::create_dir(&dir) {
            Ok(()) => fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        let meta = fs::symlink_metadata(&dir)?;
        if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "IPC directory must be private and owned by this user",
            ));
        }
        let path = dir.join(format!("{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let shared = Arc::new(Mutex::new(Shared {
            line: encode(initial),
            subscribers: Vec::new(),
        }));
        let server_shared = shared.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let worker = thread::spawn(move || {
            for connection in listener.incoming() {
                if worker_stopping.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(connection) = connection else {
                    break;
                };
                let shared = server_shared.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(connection, shared) {
                        tracing::debug!("IPC client disconnected: {err}");
                    }
                });
            }
        });
        Ok(Self {
            path,
            shared,
            stopping,
            listener: Some(worker),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn publish(&self, snapshot: &Snapshot) {
        let line = encode(snapshot);
        self.shared.lock().expect("IPC state").update(line);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Wake the accept thread before removing only our own socket file.
        self.stopping.store(true, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.path);
        if let Some(worker) = self.listener.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
        self.shared.lock().expect("IPC state").subscribers.clear();
    }
}

fn encode(snapshot: &Snapshot) -> String {
    format!(
        "{}\n",
        serde_json::to_string(snapshot).expect("snapshot serialization")
    )
}

fn handle_client(mut stream: UnixStream, shared: Arc<Mutex<Shared>>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut command = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if command.len() == 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request too long",
            ));
        }
        command.push(byte[0]);
    }
    match command.as_slice() {
        b"get" => {
            let line = shared.lock().expect("IPC state").line.clone();
            stream.write_all(line.as_bytes())
        }
        b"subscribe" => {
            let (sender, receiver) = mpsc::sync_channel(1);
            let initial = {
                let mut state = shared.lock().expect("IPC state");
                if state.subscribers.len() >= 64 {
                    return Err(io::Error::other("too many IPC subscribers"));
                }
                state.subscribers.push(sender);
                state.line.clone()
            };
            stream.write_all(initial.as_bytes())?;
            loop {
                match receiver.recv_timeout(Duration::from_secs(30)) {
                    Ok(()) => {
                        let line = shared.lock().expect("IPC state").line.clone();
                        stream.write_all(line.as_bytes())?;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let mut byte = [0];
                        // SAFETY: the connected fd and one-byte buffer are valid for this call.
                        let count = unsafe {
                            libc::recv(
                                stream.as_raw_fd(),
                                byte.as_mut_ptr().cast(),
                                1,
                                libc::MSG_PEEK | libc::MSG_DONTWAIT,
                            )
                        };
                        if count == 0 {
                            break;
                        }
                        if count < 0 {
                            let err = io::Error::last_os_error();
                            if err.kind() != io::ErrorKind::WouldBlock {
                                return Err(err);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        _ => stream.write_all(b"{\"error\":\"unknown request\"}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wm::{WindowOwner, WmCommand, WorkspaceId};
    use std::io::{BufRead, BufReader};

    #[test]
    fn snapshots_track_each_monitors_workspaces_and_focus() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        wm.set_focus(first, Some(window));
        wm.execute(WmCommand::MoveFocusedToWorkspace {
            monitor: first,
            workspace: WorkspaceId::new(2).unwrap(),
        })
        .unwrap();
        let snapshot = Snapshot::from_wm(&wm, |id| format!("output-{}", id.number()));
        assert_eq!(snapshot.active_monitor, Some(first.number()));
        assert!(!snapshot.monitors[0].workspaces[0].occupied);
        assert_eq!(
            snapshot.monitors[0].workspaces[1].windows,
            vec![window.number()]
        );
        assert_eq!(
            snapshot.monitors[0].workspaces[1].focused_window,
            Some(window.number())
        );
        assert_eq!(snapshot.monitors[1].id, second.number());
        assert!(!snapshot.monitors[1].workspaces[0].occupied);
        assert!(encode(&snapshot).ends_with('\n'));
    }

    /// Run explicitly to compare the old per-workspace full scan with the
    /// workspace-owned order. This isolates IPC construction from rendering.
    #[test]
    #[ignore = "performance comparison; run with --ignored --nocapture"]
    fn snapshot_scaling_benchmark() {
        use std::{hint::black_box, time::Instant};

        let mut wm = WmState::new();
        let monitors: Vec<_> = (0..4).map(|_| wm.add_monitor()).collect();
        for &monitor in &monitors {
            for number in 2..=64 {
                wm.add_workspace(monitor, WorkspaceId::new(number).unwrap());
            }
        }
        for number in 1..=64 {
            for &monitor in &monitors {
                for _ in 0..8 {
                    wm.add_window(WindowOwner {
                        monitor,
                        workspace: WorkspaceId::new(number).unwrap(),
                    })
                    .unwrap();
                }
            }
        }
        let legacy = || Snapshot {
            version: 1,
            active_monitor: wm.active_monitor().map(MonitorId::number),
            monitors: wm
                .monitors()
                .map(|monitor| MonitorSnapshot {
                    id: monitor.number(),
                    name: format!("output-{}", monitor.number()),
                    active_workspace: wm.active_workspace(monitor).unwrap().number(),
                    workspaces: wm
                        .workspaces(monitor)
                        .map(|workspace| {
                            let windows: Vec<_> = wm
                                .windows()
                                .filter(|(_, owner)| {
                                    owner.monitor == monitor && owner.workspace == workspace
                                })
                                .map(|(id, _)| id.number())
                                .collect();
                            WorkspaceSnapshot {
                                id: workspace.number(),
                                occupied: !windows.is_empty(),
                                focused_window: wm
                                    .focused_window(monitor, workspace)
                                    .map(|id| id.number()),
                                windows,
                            }
                        })
                        .collect(),
                })
                .collect(),
        };
        let current = || Snapshot::from_wm(&wm, |id| format!("output-{}", id.number()));
        assert_eq!(legacy(), current());
        const ITERATIONS: usize = 100;
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(legacy());
        }
        let old = start.elapsed();
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(current());
        }
        let new = start.elapsed();
        eprintln!(
            "IPC_BENCH monitors=4 workspaces=256 windows=2048 samples={ITERATIONS} old_us={} new_us={} ratio={:.2}",
            old.as_micros(),
            new.as_micros(),
            old.as_secs_f64() / new.as_secs_f64()
        );
    }

    #[test]
    fn get_returns_one_snapshot_and_subscription_receives_changes() {
        let shared = Arc::new(Mutex::new(Shared {
            line: "{\"version\":1}\n".into(),
            subscribers: Vec::new(),
        }));
        let (mut client, server) = UnixStream::pair().unwrap();
        let state = shared.clone();
        let worker = thread::spawn(move || handle_client(server, state).unwrap());
        client.write_all(b"get\n").unwrap();
        let mut result = String::new();
        client.read_to_string(&mut result).unwrap();
        assert_eq!(result, "{\"version\":1}\n");
        worker.join().unwrap();

        let (mut client, server) = UnixStream::pair().unwrap();
        let state = shared.clone();
        let worker = thread::spawn(move || handle_client(server, state).unwrap());
        client.write_all(b"subscribe\n").unwrap();
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"version\":1}\n");
        shared.lock().unwrap().update("{\"version\":2}\n".into());
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"version\":2}\n");
        shared.lock().unwrap().subscribers.clear();
        worker.join().unwrap();
    }

    #[test]
    fn server_creates_private_socket_and_cleans_it_up() {
        let runtime = PathBuf::from("/tmp/opencode").join(format!(
            "devel-wm-ipc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot = Snapshot {
            version: 1,
            active_monitor: None,
            monitors: Vec::new(),
        };
        let server = IpcServer::start_in(&runtime, &snapshot).unwrap();
        let path = server.path().to_path_buf();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let mut client = UnixStream::connect(&path).unwrap();
        client.write_all(b"get\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert_eq!(response, encode(&snapshot));
        drop(server);
        assert!(!path.exists());
        fs::remove_dir(runtime.join("devel-wm")).unwrap();
        fs::remove_dir(runtime).unwrap();
    }
}
