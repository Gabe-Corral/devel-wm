# Prototype layer-shell bar

This is a standalone test client for `devel-wm`, not the eventual status bar. It draws a 32-pixel top bar, requests a 32-pixel exclusive zone, and toggles color when clicked. With `DEVEL_WM_IPC` set, it subscribes to the [read-only state feed](../docs/IPC.md) and shows workspace numbers 1–9: blue for the active workspace, grey for occupied workspaces, and an amber underline for a workspace with remembered focus. It does not show a clock or issue commands when clicked.

For a one-command nested launch with the correct Wayland and IPC sockets, run `scripts/run-with-bar.sh` from the repository root (or use its absolute path from elsewhere). Add `--alacritty 2` to launch two terminal windows as well. Ctrl-C or closing the compositor stops all processes launched by the script. To run the bar separately, follow the steps below.

1. In one terminal at the repository root, run `cargo run --locked` and copy the `WAYLAND_DISPLAY` name it prints.
2. In another terminal, run (replace `wayland-1` with that name):

   ```sh
   cd bar-test
   env -u DISPLAY WAYLAND_DISPLAY=wayland-1 DEVEL_WM_IPC=/run/user/1000/devel-wm/12345.sock cargo run --locked
   ```

   **Do not copy the example `12345.sock` path literally.** Replace both `wayland-1` and the `DEVEL_WM_IPC` path with the exact values printed by the **currently running** compositor. Old `.sock` files may be left behind after an abnormal exit, so finding a file in the directory does not prove that it is live. Without `DEVEL_WM_IPC`, the bar still runs in its earlier static test mode.

3. Check that the bar appears at the top of the nested output, clicking it changes color and logs a click, and resizing the outer window reconfigures its width. Launch Alacritty against the same Wayland socket and check that its top edge begins **below** the bar. Use Alt+2 and Alt+1 to see the blue workspace indicator follow the active workspace; use Alt+Shift+2 on a focused window to see workspace 2 become occupied without switching. Stop the bar with Ctrl-C; the client area should return to the top of the output. Close the nested host window when finished.

For cursor testing, set `DEVEL_WM_CURSOR_TEST=1` when launching the script or the bar. Moving the pointer onto the bar requests a bright green 12×12 cursor with a (4, 5) hotspot; moving off the bar restores the compositor's arrow. This mode is only for testing `wl_pointer.set_cursor` and is not part of the normal bar.

The bar only redraws on configure and click, not continuously. The appearance, click response, resize, exclusive-zone reservation with Alacritty, and release of the zone on exit were manually confirmed on the nested output. The compositor currently has one nested output; this does not validate one bar per monitor.
