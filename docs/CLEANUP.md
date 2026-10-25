# Alpha/beta cleanup inventory

This is a **future removal list**, not an instruction to delete these files now.
Revisit it after a usable native session—including VT pause/resume, normal
shutdown, and the intended client workflow—has passed hardware testing. Nested
mode remains the default development path until we explicitly decide otherwise.
Uncommitted or untracked files may be user work; review each before removal.

## Bring-up paths to retire or fold into the real session

| Candidate | Where | Remove when / retain instead |
| --- | --- | --- |
| Discovery-only CLI probe | `--backend native` without a display option; `probe()` and query-only output in `src/native.rs` | Retire the **probe command** once device discovery is handled and diagnosed by normal startup. Keep the udev/libseat discovery needed to start the compositor. |
| One-shot presentation scene | `--native-present`, the blue/green scene, five-second hold, first-flip timeout, and its separate loop in `src/native/presentation.rs` | Remove after the normal native renderer and recovery paths replace this safety baseline. First move the connector/CRTC helpers and shared DRM/VT cleanup used by `src/native/runtime.rs` into production modules. |
| Controlled switch-test mode | `--native-switch-vt` parsing in `src/main.rs` and timed switch trigger in `src/native/presentation.rs` | Retire the **presentation-probe trigger** after production VT switching and resume are supported. Keep session pause, device relinquishment, and reactivation handling. |
| Long-running bring-up selector | `--native-session [CONNECTOR[,CONNECTOR]]` in `src/main.rs`; `run_session`, `run_session_pair`, `run_session_auto` in `src/native/runtime.rs` | Revisit the test-era CLI name once production native startup can choose outputs normally. Keep automatic connector selection, optional explicit connector choice for diagnosis, unlimited lifetime, deliberate shutdown and session reactivation. |
| Offline connector retries for early hotplug | `reprobe_connectors` and the 750-ms offline retry in `src/native/runtime.rs` | Review whether udev notifications alone suffice after hardware unplug/replug and pointer responsiveness are verified. Retain per-connector snapshot invalidation, conservative preflight, window migration and output reflow regardless of the trigger. |
| Repeated backend initialization | Overlapping DRM/GBM/EGL setup in `src/native/presentation.rs` and `src/native/runtime.rs` | Extract what the final path actually shares, then remove obsolete copies. Do not weaken connector isolation or scoped restore preflight. |

## Optional diagnostics and test-only programs

- `src/perf/mod.rs` and `DEVEL_WM_PERF`: remove the bring-up counters and
  detailed per-second output **if** production profiling uses a simpler
  mechanism. Keep any measurement needed to catch actual performance regressions.
- `scripts/bench-nested.py`: remove if its nested workload is no longer used.
  The ignored `snapshot_scaling_benchmark` in `src/ipc/mod.rs` can also go if
  it no longer answers a distinct performance question.
- `bar-test/` and `scripts/run-with-bar.sh`: remove the prototype client and
  launcher once a production bar or equivalent layer-shell/IPC coverage exists.
  Do not delete the compositor's layer-shell or IPC functionality merely
  because the prototype client is retired.
- `scripts/run-native-with-bar.sh`: review its temporary startup-log handling
  and one-prototype-bar startup. Fold it into production startup or retire
  it once a normal native session and production bar have passed real-VT checks;
  keep any useful separate smoke-test launcher deliberately rather than silently
  retaining a timeout. Do not remove the VT safety check or graceful cleanup.
- `popup-test/`: remove the standalone popup-protocol client once popup/grab/
  reposition behavior is covered elsewhere and no longer needs manual testing.
  This directory is currently untracked; decide its fate explicitly.
- Test-only logging and messages in `src/main.rs`, `src/native.rs`, and
  `src/native/`: remove obsolete scene, timeout, and smoke-test narration while
  retaining actionable production errors and cleanup diagnostics.
- Temporary hotplug timing lines in `src/native/runtime.rs` (`Native udev
  change callback`, `Native hotplug timing`, background connector queries and
  new-output takeover): review after validating asynchronous discovery against
  the residual libinput lag.
  Remove noisy per-event measurements if the diagnostic is no longer needed;
  retain a lightweight slow-operation warning if it helps detect regressions.
  Keep the connector-state transition tests if that policy remains in use.

## Automated tests, docs, and captured results

- If the final policy is literally **only code needed to run the compositor**,
  review the `#[cfg(test)]` modules in `src/main.rs`, `src/native.rs`,
  `src/native/{presentation,restore,runtime}.rs`, `src/compositor/{mod,cursor,decorations,handlers}.rs`,
  `src/{config,input,ipc,layout,wm}/` (where present). They do **not** ship in
  normal binaries, so deleting useful unit tests reduces regression protection,
  not runtime size. Prefer removing tests of retired commands and duplicate
  scenarios while keeping concise tests for surviving policy and safety code.
- Specifically revisit `native_resume_requires_client_and_switch_target`
  and the resume cases in `vt_switch_requires_explicit_native_display_test_and_valid_number`
  (`src/main.rs`) when their test-only CLI options are removed. Add any
  pause/resume regression tests we create during bring-up to this review;
  preserve tests for safety behavior that remains in the production session.
- Revisit the `--native-session` parser in `src/main.rs` when the test-era CLI
  name is consolidated; preserve tests for default nested mode, explicit native
  opt-in and connector-selection safety. The `--native-client` and
  `--native-resume` branches have already been removed.
- Keep the `launch-terminal` binding, terminal-command validation and their
  tests in `src/config/mod.rs` and `src/input/mod.rs` while on-demand launching
  is part of the real compositor; they are not the retired auto-client harness.
- Consolidate `docs/NATIVE.md` and duplicated manual workflows in `README.md`
  into current run, recovery, and troubleshooting instructions. Archive or
  remove superseded 5-, 30-, and 45-second bring-up recipes and obsolete claims
  after preserving the relevant hardware findings.
- The 17 root bring-up logs (`native-*.log`, extensionless
  `native-client-vt-switch`, and extensionless `2`) were inventoried and
  removed at the user's explicit request. The original restoration failure,
  later successes (including the VT-resume round trip), performance findings,
  and hotplug results remain summarized in `docs/NATIVE.md` and
  `docs/ROADMAP.md`. New logs, including logs captured from
  `scripts/run-native-with-bar.sh`, are separate user work; review each before
  any future cleanup.
- Build outputs under `target/`, `bar-test/target/`, and `popup-test/target/`
  are generated artifacts, not source; they do not need to ship.

## Architectural decision to make later

`src/compositor/nested.rs`, winit dependencies, and nested-only launch guidance
are unnecessary to a **native-only** binary, but nested mode is currently the
default and the safe development environment. Decide whether to retain it as a
supported backend, move it behind a development feature, or remove it **after**
native resume and everyday usability are established.

## Do not mistake these for disposable test scaffolding

Keep or refactor scoped atomic restoration (`src/native/restore.rs`), restore
preflight, connector/CRTC isolation, libseat ownership and pause/resume,
console-mode restoration, exit/error handling, required Wayland protocols,
input routing, and native mouse-motion/resize coalescing. These are functional
or safety paths even if they were first exercised through a smoke test.

Before deleting any item: identify its remaining callers, confirm an
equivalent production path exists, run the default and native builds/tests,
and check the changed hardware behavior from a separate text VT. Review
individual file and log deletions rather than using a broad cleanup command.
When adding another temporary CLI test, helper program, log, or hardware
recipe, record its name and removal condition here at the same time.
