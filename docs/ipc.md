# Local IPC

devel-wm prints two different Unix socket paths at session startup:

- `DEVEL_WM_IPC`: read-only `get\n` and `subscribe\n` newline-delimited JSON snapshots (version 2). Bar clients can display state without control access.
- `DEVEL_WM_CONTROL`: a separate same-user control socket. It accepts one line, `switch-workspace OUTPUT N\n`, where `OUTPUT` is a currently named output (for example `DP-4`) and `N` is 1–999. It replies `{"queued":true}` when the request is queued, not when the workspace is actually switched. Unknown, oversized or malformed commands are rejected.

Both sockets live inside the user's private `$XDG_RUNTIME_DIR/devel-wm/` directory and are mode `0600`. The control socket also checks the Unix peer UID. **This is same-user access control, not isolation among processes running as that user:** another same-UID process can discover and use the socket even if the bar was not given its path. Control requests are applied only by the compositor event loop; unknown outputs, invalid workspaces, locked sessions and paused native sessions do not switch. A click changes only that output's workspace, leaving the keyboard-active monitor unchanged.

The two launcher scripts pass both paths to devel-bar. For an independently started bar, set `DEVEL_WM_IPC` for display and optionally `DEVEL_WM_CONTROL` to enable workspace clicks. No other WM commands are available over this socket.
