# Native bring-up smoke tests

The original root-level bring-up logs, including the extensionless `2` and
`native-client-vt-switch`, were removed at the user's request after their
findings were summarized below and in [ROADMAP.md](ROADMAP.md). Filenames in
the historical notes are **not** existing artifacts. Use a new, descriptive
filename for each future run instead of overwriting another test result.

## Native session with one prototype bar

From a **separate Linux text VT**, `scripts/run-native-with-bar.sh` builds the
native compositor and `bar-test`, discovers connected outputs on one DRM card, waits
for its Wayland and read-only IPC sockets, and starts one test bar. Running the
script explicitly opts into an **unlimited** `--native-session`; there is no
30-second shutdown. Nested mode remains the normal `devel-wm` default.
`--config PATH` passes a TOML file to the compositor. The bar currently uses
the compositor's default output, **not** one bar per monitor. Ctrl+Alt+Backspace
stops devel-wm; Ctrl-C to the launcher requests a graceful compositor exit and
waits for DRM/console restoration. The launcher prints live output and the
compositor's exit status on normal exit; its temporary startup log under
`XDG_RUNTIME_DIR` is removed. For an inspectable hardware log, capture the
whole launch to a **new** file outside the launcher, for example:

```sh
scripts/run-native-with-bar.sh > native-bar-session-check.log 2>&1
echo $?
```

The first native IPC/bar run showed `bar configured: 3840x32`, both output
frames, and two DRM restores plus VT console restoration after the old
30-second timeout. The trailing broken pipes were clients disconnecting after
shutdown; the launcher incorrectly reported an early bar exit when shutdown
and bar disconnect raced. The launcher now recognizes compositor-initiated
shutdown. Verify bar appearance, updates, focus, exclusive zone and cleanup in
an intentional session before counting full Milestone 4 coverage. Never run
this DRM session from an X11/i3 terminal or force-terminate it with `kill -9`.

The first launcher attempt from tty3 failed before DRM takeover: Bash gave the
backgrounded compositor `/dev/null` as stdin, so its VT safety check refused
startup. Reopening `/dev/tty3` as a workaround also failed with `Permission
denied` in the subsequently overwritten `native-bar-check.log`. The launcher
now duplicates its *already-open* stdin before backgrounding the compositor;
that avoids opening the device again. A harmless pseudo-terminal test confirmed
the child sees the original tty path. The fix still needs a bounded real-VT
recheck; preserve `native-bar-check.log` until it is reviewed.

## Historical bounded bring-up records (not runnable instructions)

The `--native-client` and `--native-resume` CLI options and their automatic
30-second client timeout have been removed. Commands using them below record
earlier hardware experiments and **will not work in the current build**. Use
`--backend native --native-session` (optionally with a named connector or pair)
from a separate text VT for new interactive runs; end them intentionally with
Ctrl+Alt+Backspace or Ctrl-C to the bar launcher. This section preserves the
results and recovery context, not a recommendation to repeat those commands.

## Automatic native output selection and new-output hotplug (historical bounded checks)

`--backend native` alone is still a read-only discovery probe, and the default
backend remains nested. From a separate Linux text VT, use `--native-client`
without a connector argument for a **30-second bounded** automatic session:

```sh
./target/debug/devel-wm --backend native --native-client > native-auto-followup.log 2>&1
echo $?
```

After building with `cargo build --locked --features native`, save i3 work and
check the log for each mapped output, frame presentation, scoped restores, and
console restoration. Keep this new log and the existing logs for comparison.
The first udev-triggered discovery of a *new* connector was tested in a
separate bounded run below; keep further checks bounded until the probe stalls
are assessed. `--native-session`
without connector names explicitly opts into unlimited runtime; named
`--native-client HDMI-A-2,DP-4` and single-output commands remain the recovery
baseline. Automatic discovery refuses outputs on multiple cards instead of
silently modesetting a second GPU. The initial layout follows DRM connector
handle order, not a configurable physical left-to-right order. New connectors
join on a later udev event/settling check; connector changes that cannot be
safely restored or assigned a free CRTC are refused. Existing connectors keep
their monitor identity across disconnect/reconnect. The newly added connector
path has now passed one bounded physical test; longer-term behavior remains
unverified.

The bounded automatic startup in `native-auto-first-log` passed on 2026-10-24
with shell exit status **0**: HDMI-A-2 ran at 3840×2160@60, DP-4 at
1920×1080@144, both at scale 1. Both presented frames, and the log shows two
atomic state restores plus VT-console restoration at the 30-second timeout.
Trailing broken-pipe and `ExitFailure(1)` messages appeared after restoration;
their source was not established by the log, but they did not change the
reported shell exit status. Preserve this log.

In `native-auto-hotplug-log`, the user started with HDMI-A-2 connected and
plugged in DP-4 during the 30-second session. A udev change detected DP-4,
its restore snapshot was validated, and it presented a frame at 1920×1080@144;
a floating window transferred in both directions afterward. At timeout the
log shows **two atomic restores and VT-console restoration**; the user confirmed
shell exit status **0**. Three
HDMI-A-2 connector probes blocked the input loop for approximately 302, 180,
and 143 ms around the plug event; the log does not establish whether the user
noticed a freeze. Smithay also warned that an old mode property blob could not
be destroyed (`ENOENT`) before DP-4's first frame; this did not prevent the
observed presentation and restoration. Preserve this log and monitor these
warnings rather than treating one bounded pass as long-term hotplug coverage.

The `native-auto-hotplug-recheck-log` run tested the candidate that avoids
forced probes of unchanged online outputs. With reported exit status **0**,
HDMI-A-2 again started alone, DP-4 joined after a udev event and presented a
frame, a floating window transferred both ways, and two atomic restores plus
VT-console restoration completed. The earlier 143–302 ms HDMI-A-2 probe
notices did **not** recur. DP-4's forced mode probe took about 59 ms, however,
and libinput reported **382 ms of event-processing lag** near the plug event.
The log does not attribute that entire delay to the DP-4 probe or establish
whether the user noticed it. The nonfatal Smithay mode-blob `ENOENT` warning
recurred, as did client-side broken-pipe messages after restoration. This is
a partial improvement, **not** proof that hotplug no longer stalls input;
preserve this log and investigate the remaining event-loop lag before closing
the responsiveness item.

The next diagnostic build times the complete udev callback, existing-head
checks, unknown-connector scan, and later settling/takeover separately. With
DP-4 disconnected before startup, run **one bounded** automatic session from
a separate text VT, plug DP-4 in once, and capture a fresh log (for example
`native-auto-hotplug-timing.log`) plus `echo $?`. Preserve it and compare the
`Native udev change callback` / `Native hotplug timing` lines against any
libinput event-processing lag. These measurements cannot time udev delivery
before the callback or prove that unrelated processes did not stall the CPU.

`native-auto-hotplug-recheck-timing-log` isolates the remaining synchronous
work: the first udev callback took about **222 ms**, almost entirely an
unforced status query for the newly connected DP-4 (**220 ms**); existing-head
checks took under 0.02 ms. A second udev callback took about 2.8 ms. The later
settling pass took about 83 ms, including a 66 ms forced DP-4 probe. DP-4
presented and both outputs plus the VT restored at the 30-second timeout.
This run did **not** log a libinput lag warning, but the 222 ms callback still
blocked the event loop. The user confirmed shell exit status **0**; preserve the
log. Moving the new-connector query off the input loop is the next targeted
change if hotplug responsiveness is to be considered complete.

The follow-up candidate runs only **unknown/new-connector** status and mode
queries on a dedicated read-only DRM query worker. The main loop receives
results, then performs restore preflight, CRTC selection and surface takeover
itself. The worker is joined before the libseat fd closes; responses from an
inactive/previous VT epoch are discarded. With DP-4 disconnected before
startup, run one more bounded automatic plug-in test from a separate text VT,
using a **new** log such as `native-auto-hotplug-async-first.log` and capturing
`echo $?`. Look for `Native background status query`, `Native background mode
query`, `Native udev change callback`, and the existing takeover/restore lines.
The earlier logs must be retained for comparison. If takeover itself blocks
input, its slow-operation warning remains in the log.

`native-auto-hotplug-async-log` passed the bounded plug-in run with reported
exit status **0**: DP-4 presented and two atomic restores plus VT-console
restoration completed. The first udev callback fell from roughly 222 ms in
the timed synchronous run to **0.12 ms**. A second card-wide udev event still
spent **66.7 ms** in an HDMI-A-2 status query; a background DP-4 status query
took **70.5 ms** at about the same time. This suggests possible driver/DRM
query contention, but timing alone does not prove the cause. DP-4's mode
query took **81.2 ms** on the worker; settling on the main loop was about
0.03 ms, and the DP-4 takeover succeeded. No libinput lag warning appeared.
The recurring nonfatal mode-blob `ENOENT` warning and client-side connection
reset messages after restoration remain separate. Do **not** call hotplug
entirely nonblocking yet; preserve this log alongside both earlier checks.

## Per-output startup mode and logical scale (scale 2 hardware-checked; non-default timing unverified)

Add these optional sections to a **copy of your existing** TOML config, not
over the original terminal/keybinding settings:

```toml
[outputs.HDMI-A-2]
mode = "3840x2160@60"
scale = 2

[outputs.DP-4]
mode = "1920x1080@144"
scale = 1
```

Without an entry an output uses its advertised preferred mode and scale 1.
The mode string uses the connector's advertised physical width, height, and
**integer Hz**; the request must match an advertised mode exactly or startup
fails **before** entering graphics VT mode or attempting a modeset. Scale is
restricted to integers 1–4; named output sections apply to any connector with
that name. This is a startup-only
setting: it is retained across the selected connector's disconnect/reconnect
and VT resume, but a changed/unavailable mode refuses reattachment or resume
rather than picking another timing. Fractional scale and live mode/scale
switching are not supported. Automatic sessions can select otherwise
unspecified connected outputs at their preferred mode and scale 1.

At scale 2, HDMI-A-2's 3840×2160 **physical** pixels become a 1920×1080
**logical** desktop; DP-4 begins at logical `(1920, 0)` and remains
1920×1080 logical at scale 1. Rendered windows, pointer motion, decorations,
and bars should follow these logical coordinates. This deliberately makes the
4K desktop look as spacious as 1080p, with larger UI elements; set `scale = 1`
if you want a full 3840×2160 logical workspace. A 2026-10-24 test log `2`
confirmed the 4K/60 Hz physical HDMI mode with scale 2, both outputs presenting,
two atomic restores and VT-console restoration; visual sharpness and shell
exit status are not established by the log. Preserve `2`.

A later bounded run in `native-scale-2.log` also logged HDMI-A-2 at 4K/60 Hz
and scale 2, DP-4 at 1080p/144 Hz and scale 1, both frames, both atomic
restores and VT-console restoration. The user reported that scale 2 generally
looked good but **compositor-drawn decoration text was blurry**. The title
bitmap is generated from an 8×8 pixel font at scale 1 and enlarged. An
integer-scale bitmap candidate failed its physical recheck: in
`native-scale-2-sharp.log`, titles still looked blurry, pointer motion reached
DP-4 while hovering the second tiled HDMI-A-2 window, and tiled Firefox was
clipped at HDMI-A-2's edge. The log shows several GLES `GL_INVALID_VALUE`
errors after clients connected, followed by the 30-second timeout, two atomic
restores and VT-console restoration. The user reported `echo $?` as **127**
after running the binary directly; this status is not explained by the log or
the compositor's normal `main` return path. Preserve both logs. The unhelpful
title-buffer candidate was removed pending a better font/rendering solution.
The next bounded run, `native-scale-2-follow.log`, reported exit status **0**
and logged both restores and VT restoration without GLES errors, but the user
still saw blurry titles, early cursor crossing, and clipped tiled Firefox.
This rules out GLES errors as a necessary condition for those symptoms. The
pointer-speed adjustment failed that physical recheck and was removed. Native
cursor drawing was found to use logical coordinates in a physical-coordinate
render element; it now converts the cursor position and hotspot using the
output scale. The user confirmed the cursor looks correct in the bounded
`native-scale-2-geo.log` run and reported shell status **0**. Both outputs
presented and restored, and the VT console restored; Firefox was still clipped
and compositor-drawn titles still looked blurry. The file was subsequently
overwritten by a new run with DEBUG geometry enabled. In that later capture,
the right-hand tiled `WindowId(2)` is requested at `(1155, 10)` with width
`755`, while its committed xdg window geometry has offset `(13, 11)` and width
`755`. Smithay's `Space` already subtracts the element geometry origin when
calculating its render location, so this offset does **not** establish a
three-pixel overrun or justify manually shifting the mapped window. The log
does not identify the window's app by name or include the actual render-element
bounds/crop intersection. That capture again shows both frames, two atomic
restores, and console restoration. The earlier report of status 0 and cursor
improvement was for the previous capture, not independently recorded for the
overwritten one. Preserve the current log.

`native-scale-2-geo-debug.log` (a separate, preserved run) shows why the
right-hand client is cut: its committed xdg geometry is 755×1060 logical, but
its surface-tree bbox is 1523×2131 logical. On HDMI scale 2 a surface element
of 3020×4240 **physical** pixels starts at `(2310, 20)`; owner-output crop
retains only 1530×2140 pixels through HDMI's physical edge `(3840, 2160)`.
The client appears to supply roughly 2×-sized buffer content without a matching
logical surface size. This log does not include the client's actual
`wl_surface.buffer_scale` or viewport state, and cannot prove why that mismatch
occurred. The compositor now advertises `wp_viewporter` so clients can explicitly
specify the logical destination of high-DPI buffers. In a subsequent bounded
`native-scale-2-geo-viewport.log` run the user **confirmed Firefox is fully
visible**, but compositor-drawn decoration titles are still blurry. The newer
Firefox surface bbox is 781×1086 logical (including its margins), with a
755×1060 logical window geometry; commits show `buffer_scale=2` and
`viewport_dst=None`. This does **not** establish that Firefox used the newly
advertised viewporter or why its buffer scale differed in the earlier run.
Both outputs presented and restored at the 30-second timeout and the VT console
restored; no GLES errors appear. The user's shell status for this run is not
recorded. Preserve both logs. The user requested removal of temporary crop,
geometry, and surface-scale diagnostics once the clipping is confirmed fixed;
they have been removed from source. Retain the standard `wp_viewporter` support.

To address the still-blurry compositor title text, a new candidate replaces
the enlarged 8×8 bitmap with anti-aliased DejaVu Sans glyphs rasterized at each
output's **physical** integer scale and cached per output scale. The bundled font
and license live in `assets/fonts/`. The titlebar's logical height and window
configure sizes are unchanged; nested scale 1 uses the same font at 1×. This
candidate is built, unit-tested, and visually confirmed on the scaled HDMI
output. The bounded-check command used (use a fresh name for any repeat) is:

```sh
./target/debug/devel-wm --backend native --native-client > native-scale-2-title-font.log 2>&1
status=$?
printf 'devel-wm exit status: %s\n' "$status"
```

Open two titled windows on HDMI-A-2 and one on DP-4. Compare title sharpness,
focus colors, and titlebar width on both outputs; also confirm Firefox remains
fully visible on HDMI. Expect two atomic restores plus VT-console restoration
at the 30-second timeout. The user's `native-font.log` run ended via the exit
shortcut after two xdg toplevels connected. Both outputs presented, both atomic
restores completed, and the VT console restored; no GLES errors were logged.
The user confirmed the title text now looks good. The log does not include a
shell exit status or separately verify title appearance on DP-4. Preserve the
log; no further title-specific hardware run is required unless it regresses.

First verify the existing
known physical timings with **only HDMI-A-2's scale changed**, using a fresh
bounded log from a separate Linux text VT:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2,DP-4 --config /path/to/your/test-config.toml > native-scale-only.log 2>&1
echo $?
```

Check that both displays present, the window sizes and cursor crossing look
correct at `(1920, 0)`, independent workspaces still work, and the test
logs two restores plus VT-console restoration. Do not unplug anything during
this first scale check. Check the bar and its exclusive zone in a **separate**
bounded launcher run with `scripts/run-native-with-bar.sh --config
/path/to/your/test-config.toml` and a different log name. Only after scale
passes, try a different mode **known to
be advertised** by that connector in a separate bounded run; share its new
log and exit status before an unlimited session. Keep new logs until reviewed.

## Explicit two-output test (first bounded display/restore run passed)

HDMI-A-2 and DP-4 are currently reported **connected on card2** by sysfs. The
first two-output candidate uses one DRM device with distinct CRTCs and scoped
restore snapshots for each connector. It places HDMI-A-2 on the left and
DP-4 directly to its right with their top edges aligned (default scale 1): their
default logical origins are `(0, 0)` and `(3840, 0)`. Moving right from the lower
portion of HDMI-A-2 projects the pointer to DP-4's bottom edge. Mode and scale
can now be configured at startup as described above, but general output layout
and dynamic new-output discovery is implemented but not hardware-verified. Cross-monitor floating
drag and DP-4 disconnect/reconnect were confirmed by the user. Keep the working one-output commands as the
recovery baseline. Save i3 work, build with `cargo build --locked --features
native` in i3, and run the **bounded** test from a separate Linux text login:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2,DP-4 > native-pair-cursor.log 2>&1
echo $?
cat native-pair-cursor.log
```

Both outputs should light up; the log must show `Native output HDMI-A-2 frame
presented` and `Native output DP-4 frame presented` (each from its own CRTC).
Alt+Shift+Return launches Alacritty. Move the pointer onto DP-4, click its
empty background, then launch another terminal: new windows should belong to
that monitor. Check that workspaces and focus remain independent. The test exits after **30 seconds**
even if there are no clients. Check `Native target state restored` **twice**,
then `Native VT console mode restored`, and inspect both i3 monitors and the
text VT after exit. If either output or restore fails, **do not** run the
long-running pair: share `native-pair-cursor.log` and status first. GPU calls can
block independently of the timer; do not use `kill -9`.

Only after the bounded test and recovery are confirmed, the opt-in unlimited
pair uses `--native-session HDMI-A-2,DP-4` with the same text-VT and specific-
PID SIGTERM recovery precautions as the one-output session below. The
bounded VT-switch test flags remain single-output only.

The first `native-pair.log` recorded HDMI-A-2 at 3840×2160/60 Hz on CRTC 63
and DP-4 at 1920×1080/144 Hz on CRTC 82. Each output reported a presented
frame. One xdg-shell toplevel connected. At the 30-second timeout both scoped
restore commits completed, followed by console-mode restoration; the user
reported that the run looked good. Two earlier `PromoteFocusedToMaster`
shortcuts were rejected because no window was focused then. Three `Broken
pipe` messages and `Error: ExitFailure(1)` appear **after** restoration;
they likely came from the surviving terminal losing its Wayland connection,
but the log does not identify their process or contain the compositor's shell
exit status. The log also does not establish independent monitor workspaces,
terminal placement on DP-4, or the state of both i3 outputs after exit.

The long-running pair log `2` (no extension) has been overwritten by later
runs; `native-session.log`, `native-session-layout.log`, and
`native-session-cursor.log` are empty. In the latest `2`, HDMI-A-2 is at
`(0, 0)` and DP-4 at `(3840, 0)`. Both outputs presented and restored, and
monitor 1 accepted its own workspace shortcuts. The user confirmed the cursor
can move onto DP-4 but Firefox still briefly flashes there when resizing two
windows on HDMI-A-2 with `alt+l`. The log confirms repeated monitor-0 layout
changes; it cannot show the visual flash or shell exit status. Window render
elements are now clipped to their owning output, including transient resize
buffers; **this candidate fix has not been hardware-tested**. If the flash
persists, note whether it also appears over other HDMI windows, and use a new
log name rather than overwriting `2` again.

After the owner-output clipping change, the user reported that the Firefox
flash during resize appears fixed. The latest run again overwrote `2` rather
than creating `native-pair-resize.log`; it shows both outputs presenting and
both atomic restores plus console restoration on deliberate exit. There is no
recorded shell exit status, and one successful visual run is not yet a broad
stability or popup-at-output-edge test. Use a new log name
for subsequent runs.

For any further resize regression check, rebuild the native binary and run the bounded
pair from a separate text VT, saving its status and a fresh log:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2,DP-4 > native-pair-resize.log 2>&1
echo $?
```

Launch Firefox and Alacritty on HDMI-A-2; press Alt+L repeatedly and watch
DP-4. Confirm both outputs and the VT restore as above.

For the **new cross-monitor drag** path, rebuild the native binary, save i3
work, and use a fresh bounded log from a separate text VT:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2,DP-4 > native-pair-drag.log 2>&1
echo $?
```

Open Alacritty on HDMI-A-2. Hold **Alt+left mouse button** over its content,
drag the pointer across the boundary to DP-4, then release. Alternatively
drag an existing server-drawn titlebar after it starts moving. A tiled window
becomes floating when this gesture starts; while dragging, the pointer's
output determines ownership, and the destination's active workspace receives
the window and keyboard focus. Try dragging it back and switching workspaces
on both monitors; it must not be duplicated or lost. Then try Firefox's own
client-side titlebar (xdg move request), as well as Alt+left-drag over Firefox:
the request is accepted only for a live left-button grab on that window, and
the button release must still reach the client. Both gestures need hardware
verification.
Confirm two display restores and console restoration after the 30-second
timeout before trying an unlimited session. Capture future runs in new logs.

The user confirmed a cross-monitor drag. An earlier `2` (since overwritten;
no `native-pair-drag.log` exists) recorded WindowId(0) transferring both ways.
The current `2` records additional transfers after DP-4 reconnected, but does
not distinguish which drag gesture was used, prove Firefox's own xdg titlebar
request works, or record the shell exit status. These are historical findings;
the original log was removed at the user's request.

## Output disconnect/reconnect (pair-session hardware candidate)

The explicit **pair** client/session monitors udev changes for its selected DRM
card and probes only HDMI-A-2 and DP-4 when notified. After a disconnect it
polls every 750 ms while an output is offline to retry reattachment (two
matching connected probes); a DRM/frame error can also trigger an immediate
probe. It no longer forces periodic DRM connector reprobes while both outputs
are online: those synchronous probes could stall the libinput event loop.
On unplug, the affected surface and wl_output advertisement are retired, the
other display remains mapped, and all windows from the lost monitor's
workspaces move to the survivor's active workspace; floating positions are
clamped. The survivor shifts to `(0, 0)`. On replug, the connector must still
have the original CRTC and plane, pass a new scoped atomic restore preflight,
and create a compatible GBM surface before it is advertised again. Existing
windows **stay on the survivor**. If both outputs disappear, WM state is kept
until one returns. This does not implement automatic discovery of new outputs,
card removal, EDID/mode changes while connected, or scale changes. The
one-output paths keep their previous behavior. A failed reattach stops the
session so cleanup of remaining outputs can run rather than retrying an
uncertain hardware state.

The first DP-4 unplug/replug worked, but pointer movement intermittently froze
after replug and the user saw a libinput lag/debounce warning on exit. After
switching to udev-triggered probes, the user confirmed the freezing is fixed.
The **current** `2` (overwritten again) logs udev card change events, DP-4
disconnect/reconnect, a new DP-4 frame, later window transfers, deliberate
shortcut exit, two scoped atomic restores, and VT console restoration. It has
no libinput lag or `Native connector ... probe blocked` line. The user saw a
slow-probe notice for HDMI-A-2 and one for DP-4 in the terminal; durations and
exact timing were not preserved. Such notices mean a single synchronous DRM
probe took at least 20 ms; they do not by themselves indicate continuous
probing or a restore failure. The current `2` ran for roughly 85 seconds and
does **not** establish a bounded 30-second test. Shell exit status remains
unknown. `native-monitor-unplugg.log` and
`native-monitor-unplugg-reheck.log` were empty before the requested log cleanup.

**If repeating a bounded check: only unplug DP-4, leaving HDMI-A-2 connected.** Save
i3 work, build with `cargo build --locked --features native` in i3, switch to
a separate Linux text VT, and run a bounded pair with a *new* log name:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2,DP-4 > native-pair-hotplug-recheck.log 2>&1
echo $?
```

Launch a terminal on DP-4, unplug it, and verify the terminal migrates to
HDMI-A-2, retains keyboard focus and remains responsive. If time permits,
reconnect DP-4 and check it lights up again while the terminal remains on
HDMI-A-2. Move the pointer continuously after replug to check for pauses, and
report whether any libinput lag/debounce warning recurs. The test ends at 30
seconds; if replug takes longer, **do not**
switch straight to the unlimited pair: first inspect the log and confirm
HDMI-A-2 and the i3/VT displays recover. After an unplug without a replug,
only the still-connected connector can log `Native target state restored`;
after a successful replug expect **two** restores. In either case check VT
console restoration and the shell exit status. Do not unplug both outputs or
test a VT switch during this first hotplug check, and do not use `kill -9`.
Report the log, exit status, and any pointer/display/console failure before
further hardware tests.

Nested mode remains the default. Discovery and presentation are one-shot
probes. The opt-in native client test accepts Wayland clients with input
after you launch a terminal with the configured shortcut,
but is not a daily-driver session: stable hotplug and sustained VT resume remain
untested. Controlled VT-switch exit/cleanup passed for both the bounded
presentation probe and the one-client session on HDMI-A-2; a bounded client
pause/resume round trip also presented a frame after returning to tty3.

## Prerequisites

- Save unsaved work in your graphical session and keep it running.
- On Manjaro/Arch, `seatd` provides libseat and its development files.
  Verify `pkg-config --modversion libseat gbm egl` succeeds.
- Build in the graphical session:

  ```sh
  cargo build --locked --features native
  ```

- Switch to a **separate Linux text VT**, e.g. Ctrl+Alt+F3, and log in as your
  normal user. `tty` must report `/dev/ttyN`, not `/dev/pts/N`. Do not use sudo,
  SSH, or a terminal emulator for hardware testing.
- Change to `/home/gabe/dev-mode/devel-wm`. Know which VT hosts i3 before
  starting; it is commonly F1 or F2, but depends on your setup.

## Milestone 2: discovery (no modeset)

```sh
./target/debug/devel-wm --backend native
```

Expect an active seat, card paths and drivers, connector states/mode counts,
and a completion message. This passed the user's initial hardware test:
HDMI-A-2 and DP-4 were discovered, with HDMI-A-2 confirmed connected (the initial
HDMI-A-1 name was corrected by the user).

## Milestone 3: explicit single-output presentation

```sh
./target/debug/devel-wm --backend native --native-present HDMI-A-2 > native-normal.log 2>&1
echo $?
cat native-normal.log
```

This command **does modeset**. It searches seat cards for exactly one connected
HDMI-A-2 (case-insensitive), selects its preferred mode or the first reported
mode, and refuses to borrow a CRTC assigned to another connector. There is no
automatic fallback to DP-4 or another output. Only the target surface is
created; the backend does not deliberately disable all connectors at startup.

Expected acceptance checks:

1. The log identifies the target card, CRTC, mode, EGL render node, and native
   GLES vendor/renderer/version. Software rendering is rejected. The nested
   renderer log is not evidence about this native path.
2. HDMI-A-2 shows a dark blue background and a green center rectangle.
3. The log reports `Native frame presented` after a matching DRM page-flip.
   Queuing a frame alone is not counted as presentation.
4. After five seconds from that completion, the process exits and the text
   console becomes usable again. No continuous repaint loop is needed for
   this static scene.
5. Switch back to i3; verify existing windows and both monitors still behave
   normally. Record any unexpected changes to DP-4.

The page-flip wait is bounded to three seconds. Graphics-driver initialization
and ioctls can still block independently of that deadline; this is not a
guaranteed watchdog against a hung driver.

### Cleanup and recovery

- An actual SIGINT or SIGTERM requests normal cleanup. Ctrl+C on a VT in
  KD_GRAPHICS mode may not generate SIGINT: the presentation test has no
  libinput/keyboard event handling. Do not rely on the key for that test. Do not use
  `kill -9`: it bypasses restoration and console cleanup.
- Switching to another VT pauses the DRM device and ends the test. It does not
  attempt restoration/modesetting after another session has taken control.
- The presentation test now requires atomic DRM. Before presenting it captures
  only the connector mapping, CRTC mode/active/VRR state, and primary-plane
  properties changed by the test. It owns a copy of the original mode blob,
  uses no saved fence fd/pointer or damage-blob ID, and validates the inverse
  commit with TEST_ONLY. Failed preflight refuses presentation.
- Normal/error exits drop the output, explicitly validate and restore the
  target state while the seat is active, then restore the console and close
  the original device through libseat. Other outputs are not included in the
  restore request. The original framebuffer must remain valid; an unplug or
  stale resource fails visibly instead of using a fallback output.
- Smithay 0.7's unchecked card-wide property replay is bypassed after the
  explicit cleanup. Display/console/device-close errors cause a nonzero exit
  status even when the frame was successfully presented. A VT-switch exit
  deliberately skips display restoration and identifies that in the log.
- If the test leaves a black display, try returning to i3's VT. If necessary,
  log in on another text VT, identify the test process with `pgrep -a devel-wm`,
  and send `kill -TERM PID` to that specific process. Do not terminate a separate
  nested instance by mistake. Save the log before retrying.

## Cleanup retest: HDMI-A-2

The initial `native-presentation.log` confirmed NVIDIA RTX 5070 GPU rendering
and page-flip completion at 3840×2160/60 Hz. Smithay's old restore attempt
logged EINVAL. Scoped restoration passed the later normal-exit and timed-SIGINT
tests (`native-normal.log`, `native-interrupt.log`), with intact i3 displays.
The VT-switch test remains deferred; the historical results are noted below.

Build again in i3, then perform these tests one at a time from the text VT:

1. **Normal exit (passed):** run the `native-normal.log` command above, let the five-second
   hold finish, and inspect the exit status immediately. Expect status 0 and
   `Native target state restored (atomic commit completed)` followed by
   `Native VT console mode restored`. Return to i3 and check both monitors.
2. **Early SIGINT (passed):** use a short-lived child shell to send a signal to its
   own PID two seconds after it `exec`s the compositor. The compositor stays
   in the foreground, with stdin on the text VT. Run:

   ```sh
   bash -c '(sleep 2; kill -INT "$$") & exec ./target/debug/devel-wm --backend native --native-present HDMI-A-2' > native-interrupt.log 2>&1
   echo $?
   cat native-interrupt.log
   ```

   Expect `Native frame presented`, `Native test exit requested by signal`,
   the same two restoration messages, status 0, and usable console/i3.
   If the program fails before the two-second timer fires, do not retry
   blindly: inspect the log first. A pending first flip is drained (up to its
   three-second deadline) before signal-requested teardown.
3. **VT switch (presentation probe passed):** keyboard VT switching is not implemented in the
   presentation probe once it enters graphics mode, so Ctrl+Alt+Fn may do
   nothing. A separate, explicitly requested libseat switch can now test the
   pause/cleanup path. First identify the number of i3's VT (for example,
   `/dev/tty2` means `2`); in an i3 terminal, `loginctl show-session
   "$XDG_SESSION_ID" -p VTNr --value` may show it. Verify the result matches
   your graphical session **before starting**. Do not guess or use the current
   text VT number. From the separate text VT, run (replace `2` with the
   confirmed i3 VT number):

   ```sh
    ./target/debug/devel-wm --backend native --native-present HDMI-A-2 --native-switch-vt 2 > native-vt-switch.log 2>&1
   ```

    The scene should appear for approximately one second, then the program
    asks libseat to activate the specified VT. Do **not** press Ctrl+Alt+Fn
    during the test. Check both i3 monitors/windows. Then return to the text
    VT and run `echo $?` and `cat native-vt-switch.log`. Expect `Requesting VT
    switch`, `Session paused`, and `Native restore skipped: session/device
    inactive`, with no cleanup modeset, console restoration, and status 0. It
    should not resume on return. The first HDMI-A-2 run switched tty3 → tty7,
    logged the pause and skipped restore while inactive, then restored console
    mode and completed without a logged cleanup error. If libseat refuses or no pause arrives within
    three seconds, the command reports a failure and performs its usual
    bounded cleanup while it still owns the session. This does not establish
    recovery for the interactive native client session; if cleanup fails, stop
    and share the log rather than repeatedly trying.

Stop and share the new log if any test reports a restore/preflight/console/close
failure or visible display regression. Automated tests do not prove hardware
cleanup. Controlled VT-switch exit passed for the presentation and one-client
tests; the separate bounded VT-resume test below has since presented a frame
after returning. One DP-4 unplug/replug also worked, but pointer responsiveness
after reconnect and everyday native-session stability remain unverified.

## Milestone 4: one interactive native client (experimental)

### Long-running single-output session (first hardware run passed)

This opt-in path is **not** the bounded `--native-client` test. Save i3 work,
build with `cargo build --locked --features native` from i3, then log in on a
**separate Linux text VT** as your normal user. Check `tty` reports `/dev/ttyN`.
From the repository root run:

```sh
./target/debug/devel-wm --backend native --native-session HDMI-A-2 > native-session.log 2>&1
echo $?
cat native-session.log
```

It opens only HDMI-A-2, creates a Wayland socket, and **does not start any
client or impose a time limit**. Press **Alt+Shift+Return** to launch Alacritty
(or the terminal/shortcut configured in `~/.config/devel-wm/config.toml`).
Test opening/closing two terminals, typing and pointer input, then press
**Ctrl+Alt+Backspace** to shut down. Check that the original text console and
both i3 monitors still work; record exit status and log. Do not use `kill -9`.
The native pause/resume path remains enabled, but this run does not automatically
switch VTs and no further VT-switch exercise is required for this first check.
Terminals are not forcibly killed; they lose their Wayland connection when the
compositor exits. Missing terminal executables are reported without shutting
down the compositor.

**No safety timer runs in this mode.** If the exit shortcut fails, return to
another VT, identify this process with `pgrep -a devel-wm`, and send `kill -TERM
PID` to that specific PID (not another nested instance). Give it time to
restore state; do not force-kill it. If graphics is stuck, use another text VT
and share `native-session.log` before another attempt. Driver calls can block
even signal-driven shutdown. This path does not yet handle hotplug or other
outputs and is not considered a daily-driver session after one successful run.

The first `native-session.log` run lasted about 49 seconds, connected multiple
xdg-shell toplevels, applied master-ratio/focus/floating shortcuts, then logged
`Native exit shortcut Ctrl+Alt+Backspace`, `Native target state restored
(atomic commit completed)`, and `Native VT console mode restored`. There is a
Smithay warning about not becoming DRM master (it continued in unprivileged
mode), but no recorded cleanup failure. The log does not contain the shell
exit status or prove post-exit display appearance; the user reported that the
run looked good. Treat this as one successful manual run, not hotplug or
long-term stability coverage.

Build with `--features native` as above. From the **separate text VT** (not an
X11/i3 terminal), with Alacritty installed, run:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2 > native-client.log 2>&1
echo $?
cat native-client.log
```

This explicitly modesets only HDMI-A-2 and starts a Wayland socket **without
auto-launching a client**. Press **Alt+Shift+Return** promptly to launch the
configured terminal (Alacritty by default); the shortcut and terminal command
can be changed in the TOML config (see `README.md`). It routes keyboard,
relative mouse, mouse buttons and scrolling
through the shared WM seat. The session ends after **30 seconds** even without
input. `Ctrl+Alt+Backspace` asks it to exit earlier. A SIGINT/SIGTERM sent from
another VT also requests normal cleanup; Ctrl+C on the graphics VT may not
work. Avoid `kill -9`. On exit the display and console are restored; terminals
are no longer forcibly killed, though they lose their Wayland connection when
the compositor exits.

Check that the terminal is visible, typing works, the cursor moves, and the
text VT and both i3 outputs are intact afterward. Record the exit status and
 log. The first hardware run displayed Alacritty and restored the target and
 console, but resize smoothness is unresolved. If it fails, stop and
share the log instead of repeatedly modesetting. The separate interactive
client VT-switch cleanup test also passed; the separate bounded resume test
below covers one VT return, not an everyday native session.

### Controlled VT-switch test with Alacritty (passed once on HDMI-A-2)

First save i3 work and identify its VT number. The presentation probe switched
tty3 → tty7 on the first test, but verify tty7 still hosts i3 rather than
assuming that number. Rebuild with `cargo build --locked --features native` in
your graphical session. From the **separate text VT**, run (replace `7` if
your graphical VT differs):

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2 --native-switch-vt 7 > native-client-vt-switch.log 2>&1
```

Press Alt+Shift+Return to launch Alacritty promptly after startup. Once a
client frame has been presented and at
least two seconds have elapsed, the compositor requests the switch via libseat.
Do not press Ctrl+Alt+Fn during the test. Check both i3 outputs and existing
windows. Return to the text VT and run `echo $?` and
`cat native-client-vt-switch.log`. Expect `Requesting native client VT switch`,
`Native session paused; exiting without restoration modeset`, `Native restore
skipped: session/device inactive`, and `Native VT console mode restored`. The
launched terminal loses its compositor connection. The usual 30-second safety exit remains; if
no client frame appears, the switch test fails rather than claiming success.
The first run saved as `native-client-vt-switch` (without `.log`) showed tty3 →
tty7, a connected xdg toplevel, switch request, session pause, client stop,
skipped inactive DRM restore and restored console mode without a logged error.
If libseat refuses the switch or does not pause within three seconds, the
command reports a failure and performs active-session cleanup when possible.
Do not repeat a failed modeset; share the log and status first. Avoid `kill -9`.

### Bounded one-client VT-resume test (one round trip observed)

This is **not** the earlier switch-and-exit test. Save i3 work, verify its VT
number (tty7 in the previous runs), build with `cargo build --locked --features
native` in i3, and from the separate text login (tty3 in the previous runs)
run:

```sh
./target/debug/devel-wm --backend native --native-client HDMI-A-2 --native-switch-vt 7 --native-resume > native-client-resume.log 2>&1
```

Press Alt+Shift+Return to launch Alacritty promptly after startup. The test
switches to i3 after a client frame and **keeps the client process alive while
paused**. Confirm i3 and both monitors work.
Within the **45-second** safety limit, switch back to the original text VT
(for tty3, Ctrl+Alt+F3 from i3). Alacritty should reappear on HDMI-A-2 and
respond to input; the log should show `Native client session reactivated` and
`Native client frame presented after VT resume`. Do not start another instance.
The test then exits at the safety limit (or Ctrl+Alt+Backspace) and attempts
scoped restoration of the display state captured *on reactivation*. Check i3
and the text console again; record `echo $?` and `cat native-client-resume.log`
from the text VT. A pause without a resumed frame is a **failed test**, not a
successful switch. If the return does not work, let the bounded test exit,
avoid `kill -9`, and share the log before retrying. GPU operations can still
block independently of the timer; this is not a driver-hang watchdog.

The first run, saved as `native-resume.log`, recorded tty3 → tty7, session
pause, reactivation on tty3, a new restore snapshot and a presented client
frame after resume. The user confirmed Alacritty accepted input after return
and both i3 monitors worked afterward. At the 45-second safety timeout the
log shows the client stopped, scoped atomic restoration completed, and text
console mode restored. The shell exit status was not recorded. The log also
contains a nonfatal Smithay warning about destroying an old mode property
blob (`ENOENT`). This verifies one bounded manual VT round trip, not a
durable native session: repeated VT switches, early shutdown while paused,
and resume failure recovery remain untested.

### Resize performance diagnostic

The first HDMI-A-2 client run displayed Alacritty and restored the target and
console successfully, but resizing felt stuttery. Its log contains libinput
22–62 ms event-processing lag warnings, without frame timings. A second run
(`native-client-perf.log`) showed render/submit p95 generally below 1 ms,
26–60 submitted frames/s while active, and libinput warnings of 27–63 ms.
Rendering CPU time alone does not explain the resize jitter; pointer events
can arrive far faster than output flips and client commits. The native client
path now coalesces floating-resize configures to the latest size per flip,
flushing the final configure on mouse release. This is an unverified candidate
improvement, not a claim that the input lag is resolved. To compare, an
**optional** repeat from the text VT collects one-second summaries (including
render/submit time, frame p95, and damaged pixels). The latest retest still
felt very jittery, with 31–86 ms libinput warnings and some active seconds at
25–46 frames/s despite measured frame work mostly below 1 ms. A further
diagnostic build (`native-client-perf-2.log`) recorded resize input-handler
p95 at 1.5–2.5 ms across hundreds of mouse events per second (e.g. 659
updates versus 29 configures). The event handler is doing resize work far more
often than the display can flip. Native relative mouse motion is now
accumulated between output flips, keeping its total delta and latest timestamp,
and flushed before button/other input events; this candidate fix has **not**
been hardware-tested. To compare, run:

```sh
DEVEL_WM_PERF=1 ./target/debug/devel-wm --backend native --native-client HDMI-A-2 > native-client-perf.log 2>&1
echo $?
```

Move the mouse without resizing for a few seconds, then resize the floating
Alacritty (Alt+right-button drag) for several seconds. Share the log and note
roughly when the resize began and whether only the window or also the cursor
jittered. The 30-second safety limit and the same VT precautions still apply.
