# Read-only IPC (prototype)

The compositor prints `DEVEL_WM_IPC=/run/user/.../devel-wm/<pid>.sock` on startup. This is a **separate** Unix socket from `WAYLAND_DISPLAY`. Its directory is owned by the current user with mode 0700; the socket is mode 0600. The socket file is removed on normal shutdown. IPC currently supports queries and subscriptions only, not commands.

Open a connection, send one UTF-8 line, then read newline-delimited JSON:

- `get\n`: one snapshot, then EOF.
- `subscribe\n`: an initial snapshot, followed by a full snapshot whenever the state changes. Intermediate changes can be coalesced for a slow subscriber; always replace your displayed state with the latest snapshot. Disconnect to stop subscribing.

Other requests return `{"error":"unknown request"}` and close. Protocol version is `1`. Example snapshot (IDs are scoped as indicated; workspace numbers are local to each monitor):

```json
{"version":1,"active_monitor":0,"monitors":[{"id":0,"name":"nested","active_workspace":2,"workspaces":[{"id":1,"occupied":false,"focused_window":null,"windows":[]},{"id":2,"occupied":true,"focused_window":0,"windows":[0]}]}]}
```

`focused_window` remembers a workspace's last focused window even when that workspace is not active; `active_monitor` identifies the monitor receiving keyboard input. A window belongs to exactly one monitor/workspace. IDs are allocated for the lifetime of this compositor process and should not be persisted across restarts. Window titles, layout, commands, and incremental events are future additions.

For a one-shot query using the path printed by the compositor:

```sh
DEVEL_WM_IPC=/run/user/1000/devel-wm/12345.sock python3 -c 'import os,socket; s=socket.socket(socket.AF_UNIX); s.connect(os.environ["DEVEL_WM_IPC"]); s.sendall(b"get\n"); print(s.makefile().read(),end="")'
```
