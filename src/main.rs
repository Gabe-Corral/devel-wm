use std::{
    ffi::{OsStr, OsString},
    io,
    path::PathBuf,
    time::Instant,
};

use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

use devel_wm::{
    LoopData,
    compositor::{Compositor, run_nested},
    config::Keybindings,
    ipc::IpcServer,
};

const USAGE: &str = "Usage: devel-wm [--backend nested|native] [--config PATH] [--native-present CONNECTOR | --native-session [CONNECTOR[,CONNECTOR]]] [--native-switch-vt N]\nNative without a display option is discovery only; --native-session runs until intentional exit. Omit its connector list to discover outputs on one DRM card automatically. Named connectors remain available for recovery. --native-switch-vt requires --native-present CONNECTOR.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Nested,
    Native,
}

#[derive(Debug, PartialEq, Eq)]
enum StartupCommand {
    Help,
    Run {
        backend: Backend,
        explicit_config: Option<PathBuf>,
        native_present: Option<String>,
        native_session: Option<String>,
        native_switch_vt: Option<i32>,
    },
}

fn connector_pair(value: &str) -> io::Result<Option<(&str, &str)>> {
    let Some((first, second)) = value.split_once(',') else {
        return Ok(None);
    };
    if first.is_empty()
        || second.is_empty()
        || second.contains(',')
        || first.eq_ignore_ascii_case(second)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output pair requires two distinct comma-separated connectors",
        ));
    }
    Ok(Some((first, second)))
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<StartupCommand> {
    let mut args = args.into_iter().peekable();
    let mut backend = None;
    let mut explicit_config = None;
    let mut native_present = None;
    let mut native_session = None;
    let mut native_switch_vt = None;
    while let Some(flag) = args.next() {
        if flag == OsStr::new("--help") {
            return Ok(StartupCommand::Help);
        } else if flag == OsStr::new("--backend") {
            if backend.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --backend",
                ));
            }
            let value = args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--backend requires nested or native",
                )
            })?;
            backend = Some(match value.to_str() {
                Some("nested") => Backend::Nested,
                Some("native") => Backend::Native,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "invalid backend: {} (expected nested or native)",
                            value.to_string_lossy()
                        ),
                    ));
                }
            });
        } else if flag == OsStr::new("--native-present") {
            if native_present.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --native-present",
                ));
            }
            native_present = Some(
                args.next()
                    .and_then(|value| value.into_string().ok())
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--native-present requires a connector name, e.g. HDMI-A-1",
                        )
                    })?,
            );
        } else if flag == OsStr::new("--native-session") {
            if native_session.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --native-session",
                ));
            }
            native_session = Some(
                if args
                    .peek()
                    .is_some_and(|value| !value.to_string_lossy().starts_with('-'))
                {
                    args.next()
                        .expect("peeked connector")
                        .into_string()
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidInput, "invalid connector name")
                        })?
                } else {
                    String::new()
                },
            );
        } else if flag == OsStr::new("--native-switch-vt") {
            if native_switch_vt.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --native-switch-vt",
                ));
            }
            native_switch_vt = Some(
                args.next()
                    .and_then(|value| value.to_str().and_then(|value| value.parse::<i32>().ok()))
                    .filter(|vt| (1..=63).contains(vt))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "--native-switch-vt requires a VT number from 1 to 63",
                        )
                    })?,
            );
        } else if flag == OsStr::new("--config") {
            if explicit_config.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "duplicate --config",
                ));
            }
            explicit_config = Some(PathBuf::from(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--config requires a TOML file path",
                )
            })?));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown option: {}", flag.to_string_lossy()),
            ));
        }
    }
    let backend = backend.unwrap_or(Backend::Nested);
    if (native_present.is_some() || native_session.is_some()) && backend != Backend::Native {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native display options require --backend native",
        ));
    }
    if native_present.is_some() && native_session.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "choose one of --native-present or --native-session",
        ));
    }
    if native_session.is_some() && native_switch_vt.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--native-switch-vt is only for bounded display tests",
        ));
    }
    if let Some(connectors) = &native_session {
        connector_pair(connectors)?;
    }
    if native_switch_vt.is_some() && native_present.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--native-switch-vt requires --native-present CONNECTOR",
        ));
    }
    Ok(StartupCommand::Run {
        backend,
        explicit_config,
        native_present,
        native_session,
        native_switch_vt,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let StartupCommand::Run {
        backend,
        explicit_config,
        native_present,
        native_session,
        native_switch_vt,
    } = parse_args(std::env::args_os().skip(1))?
    else {
        println!("{USAGE}");
        return Ok(());
    };
    // Native discovery/presentation do not start clients. The explicit client
    // path creates its own Wayland socket without opening a winit window.
    if backend == Backend::Native {
        #[cfg(feature = "native")]
        {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();
            return match native_present {
                Some(connector) => match native_switch_vt {
                    Some(vt) => devel_wm::native::present_switch_test(&connector, vt),
                    None => devel_wm::native::present(&connector),
                },
                None => match native_session {
                    Some(connector) => {
                        let keys = Keybindings::load(explicit_config.as_deref())?;
                        if connector.is_empty() {
                            devel_wm::native::run_session_auto(keys)
                        } else if let Some((first, second)) = connector_pair(&connector)? {
                            devel_wm::native::run_session_pair(first, second, keys)
                        } else {
                            devel_wm::native::run_session(&connector, keys)
                        }
                    }
                    None => devel_wm::native::probe(),
                },
            };
        }
        #[cfg(not(feature = "native"))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native support is disabled; rebuild with --features native (nested remains the default)",
        )
        .into());
    }
    let _ = native_present;
    let _ = native_session;
    let _ = native_switch_vt;
    let keybindings = Keybindings::load(explicit_config.as_deref())?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut event_loop: EventLoop<LoopData> = EventLoop::try_new()?;
    let display = Display::<Compositor>::new()?;
    let display_handle = display.handle();
    let compositor = Compositor::new(&mut event_loop, display, keybindings, "winit")?;
    let mut data = LoopData {
        compositor,
        display_handle,
        nested: None,
        #[cfg(feature = "native")]
        native: None,
        ipc: None,
    };

    run_nested(&mut event_loop, &mut data)?;
    let ipc = IpcServer::start(&data.compositor.ipc_snapshot())?;
    println!("DEVEL_WM_IPC={}", ipc.path().display());
    data.ipc = Some(ipc);
    println!(
        "WAYLAND_DISPLAY={}",
        data.compositor.socket_name.to_string_lossy()
    );
    println!(
        "To test: env -u DISPLAY WAYLAND_DISPLAY={} alacritty",
        data.compositor.socket_name.to_string_lossy()
    );

    let mut last_fallback_cleanup = Instant::now();
    event_loop.run(None, &mut data, move |data| {
        let measured = data.compositor.perf.is_some();
        let start = measured.then(Instant::now);
        let window_count = data.compositor.space.elements().len();
        data.compositor.space.refresh();
        let after_refresh = measured.then(Instant::now);
        // xdg-shell removal is event-driven. Keep an infrequent fallback for
        // unexpected lifetimes (including surfaces already evicted by Space).
        if data.compositor.space.elements().len() != window_count
            || last_fallback_cleanup.elapsed() >= std::time::Duration::from_secs(5)
        {
            data.compositor.cleanup_windows();
            data.compositor.reap_terminal_children();
            last_fallback_cleanup = Instant::now();
        }
        let after_cleanup = measured.then(Instant::now);
        if let Some(snapshot) = data.compositor.take_ipc_update()
            && let Some(ipc) = &data.ipc
        {
            ipc.publish(&snapshot);
        }
        let after_ipc = measured.then(Instant::now);
        if data.compositor.space.elements().len() != window_count {
            data.compositor.needs_repaint = true;
        }
        data.compositor.popups.cleanup();
        if let Err(err) = data.display_handle.flush_clients() {
            eprintln!("Failed to flush Wayland clients: {err}");
            data.compositor.loop_signal.stop();
        }
        let redraw = data.compositor.needs_repaint;
        if redraw {
            data.compositor.needs_repaint = false;
            if let Some(nested) = &data.nested {
                nested.request_redraw();
            }
        }
        if let Some(perf) = data.compositor.perf.as_mut() {
            let start = start.expect("performance timer");
            let after_refresh = after_refresh.expect("performance timer");
            let after_cleanup = after_cleanup.expect("performance timer");
            let after_ipc = after_ipc.expect("performance timer");
            perf.event(
                after_refresh.duration_since(start),
                after_cleanup.duration_since(after_refresh),
                after_ipc.duration_since(after_cleanup),
                redraw,
            );
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> io::Result<StartupCommand> {
        parse_args(values.iter().map(OsString::from))
    }

    #[test]
    fn defaults_to_nested_and_accepts_either_option_order() {
        assert_eq!(
            parse(&[]).unwrap(),
            StartupCommand::Run {
                backend: Backend::Nested,
                explicit_config: None,
                native_present: None,
                native_session: None,
                native_switch_vt: None,
            }
        );
        for args in [
            ["--backend", "native", "--config", "custom.toml"],
            ["--config", "custom.toml", "--backend", "native"],
        ] {
            assert_eq!(
                parse(&args).unwrap(),
                StartupCommand::Run {
                    backend: Backend::Native,
                    explicit_config: Some(PathBuf::from("custom.toml")),
                    native_present: None,
                    native_session: None,
                    native_switch_vt: None,
                }
            );
        }
        assert_eq!(parse(&["--help"]).unwrap(), StartupCommand::Help);
    }

    #[test]
    fn presentation_requires_explicit_native_backend() {
        assert_eq!(
            parse(&["--native-present", "HDMI-A-1", "--backend", "native"]).unwrap(),
            StartupCommand::Run {
                backend: Backend::Native,
                explicit_config: None,
                native_present: Some("HDMI-A-1".into()),
                native_session: None,
                native_switch_vt: None,
            }
        );
        for args in [
            vec!["--native-present", "HDMI-A-1"],
            vec!["--backend", "nested", "--native-present", "HDMI-A-1"],
            vec!["--backend", "native", "--native-present"],
            vec!["--backend", "native", "--native-present", "--config"],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-1",
                "--native-present",
                "DP-4",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn rejects_missing_invalid_and_repeated_options() {
        for args in [
            vec!["--backend"],
            vec!["--backend", "unknown"],
            vec!["--backend", "nested", "--backend", "native"],
            vec!["--config"],
            vec!["--config", "a", "--config", "b"],
            vec!["--other"],
        ] {
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn vt_switch_requires_explicit_native_display_test_and_valid_number() {
        assert_eq!(
            parse(&[
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
                "2"
            ])
            .unwrap(),
            StartupCommand::Run {
                backend: Backend::Native,
                explicit_config: None,
                native_present: Some("HDMI-A-2".into()),
                native_session: None,
                native_switch_vt: Some(2),
            }
        );
        for args in [
            vec!["--backend", "native", "--native-switch-vt", "2"],
            vec!["--backend", "native", "--native-resume"],
            vec![
                "--backend",
                "native",
                "--native-client",
                "HDMI-A-2",
                "--native-resume",
            ],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
                "7",
                "--native-resume",
            ],
            vec![
                "--backend",
                "native",
                "--native-client",
                "HDMI-A-2",
                "--native-switch-vt",
                "7",
                "--native-resume",
                "--native-resume",
            ],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
                "0",
            ],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
                "64",
            ],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
            ],
            vec![
                "--backend",
                "native",
                "--native-present",
                "HDMI-A-2",
                "--native-switch-vt",
                "2",
                "--native-switch-vt",
                "3",
            ],
        ] {
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn native_session_is_explicit_and_excludes_bounded_test_flags() {
        assert_eq!(
            parse(&["--backend", "native", "--native-session"]).unwrap(),
            StartupCommand::Run {
                backend: Backend::Native,
                explicit_config: None,
                native_present: None,
                native_session: Some(String::new()),
                native_switch_vt: None,
            }
        );
        assert_eq!(
            parse(&[
                "--config",
                "custom.toml",
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2"
            ])
            .unwrap(),
            StartupCommand::Run {
                backend: Backend::Native,
                explicit_config: Some(PathBuf::from("custom.toml")),
                native_present: None,
                native_session: Some("HDMI-A-2".into()),
                native_switch_vt: None,
            }
        );
        assert_eq!(
            parse(&["--backend", "native", "--native-session", "HDMI-A-2,DP-4"]).unwrap(),
            StartupCommand::Run {
                backend: Backend::Native,
                explicit_config: None,
                native_present: None,
                native_session: Some("HDMI-A-2,DP-4".into()),
                native_switch_vt: None,
            }
        );
        for args in [
            vec!["--backend", "native", "--native-session", "HDMI-A-2,"],
            vec!["--backend", "native", "--native-session", ",DP-4"],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2,HDMI-A-2",
            ],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2,DP-4,DP-5",
            ],
            vec!["--native-session", "HDMI-A-2"],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2",
                "--native-session",
                "DP-4",
            ],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2",
                "--native-client",
                "HDMI-A-2",
            ],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2",
                "--native-present",
                "HDMI-A-2",
            ],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2",
                "--native-switch-vt",
                "7",
            ],
            vec![
                "--backend",
                "native",
                "--native-session",
                "HDMI-A-2",
                "--native-resume",
            ],
        ] {
            assert_eq!(
                parse(&args).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{args:?}"
            );
        }
    }
}
