# Configuration

devel-wm reads a TOML file at `$XDG_CONFIG_HOME/devel-wm/config.toml` (or `~/.config/devel-wm/config.toml` when `XDG_CONFIG_HOME` is unset). The file is optional; omitted settings use built-in defaults. Supply `--config /path/to/config.toml` to use a different file (a missing explicit file is an error). Changes take effect on restart, not live reload.

For example:

```toml
[keybindings]
focus-next = "Super+j"
switch-workspace-1 = "Super+1"
launch-terminal = "Super+Return"

[terminal]
command = ["foot", "--title", "dev"]

[layout]
inner_gap = 12
outer_gap = 8

[colors]
focused_border = "#bd93f9"

[fonts]
family = "Iosevka"
size = 14

[outputs.DP-4]
mode = "1920x1080@144"
scale = 1
```

`[keybindings]` replaces individual named shortcuts while keeping the other defaults; conflicting or invalid bindings stop startup. `[terminal].command` is a program-and-arguments array, not a shell string. Gaps are nonnegative logical pixels. Color values are six-digit RGB hex strings; the font family is optional and size must be 8–20. `[outputs.CONNECTOR]` applies to native sessions only: choose an advertised `WIDTHxHEIGHT@HZ` mode and an integer scale from 1 to 4. Unspecified outputs use their preferred mode and scale 1.

`Alt+Shift+t` (`toggle-floating-mode`) enters or leaves workspace-local floating mode, restoring the previous tiled layout when leaving. `Alt+t` (`toggle-tiling-mode`) still switches master/stack and side-by-side. Entering floating mode arranges existing tiles in the usable area without changing their tile order; their adjusted floating positions are remembered when returning. Individually floating windows remain floating in every mode. Drag a floating-mode window by its titlebar or Alt+left-button, resize with Alt+right-button. Edge snapping is not implemented yet.
