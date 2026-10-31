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
