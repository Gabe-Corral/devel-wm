# Nested clipboard smoke test

The compositor advertises Wayland `wl_data_device_manager`. Smithay forwards a
client-owned clipboard selection to the keyboard-focused client and relays
requests for a MIME type to the source client. The compositor does not read or
cache clipboard contents. It clears the selection when the owner disconnects;
copying from the nested compositor to the X11 host is not supported.

On an X11 desktop, run `cargo run --locked` and note its printed
`WAYLAND_DISPLAY` value. In two other terminals, start two clients using the
**same** value (replace `wayland-N` below):

```sh
env -u DISPLAY WAYLAND_DISPLAY=wayland-N alacritty
env -u DISPLAY WAYLAND_DISPLAY=wayland-N alacritty
```

1. In client A, print a unique string, e.g. `printf 'nested-clipboard-123\n'`.
   Select it with the mouse and press Alacritty's **Ctrl+Shift+C** (not Ctrl+C,
   which interrupts the shell). Focus client B and press **Ctrl+Shift+V**.
   Confirm the same text appears. Repeat with B as the source and A as the
   destination.
2. Copy in A, move B to workspace 2 with **Alt+Shift+2** while B is focused,
   switch to it with **Alt+2**, and paste in B. Copy a *new* string in B, switch
   back with **Alt+1**, and paste in A. The selection belongs to the seat, not
   a workspace. The host WM may intercept Alt shortcuts in nested mode.
3. While B remains open, copy another unique string in A, close A, focus B,
   and try to paste. The old string must not be inserted: without a clipboard
   manager, the exited client can no longer supply it. Copy a new string in B
   and verify that a subsequently opened nested client can paste it.

These are manual checks, not evidence that the test passed on every backend.
They require an X11 display and interactive input; `cargo test` alone does not
exercise them.
