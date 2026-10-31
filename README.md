# devel-wm

devel-wm is a standalone Rust Wayland compositor with keyboard-driven tiling and floating windows. It is under development; nested mode is the default.

## Run

From the repository root, with Rust/Cargo and the Wayland, X11, EGL/GLES, and xkbcommon libraries installed:

```sh
cargo run --locked
```

This opens a nested compositor window in an existing graphical session. To run it with the standalone devel-bar, use `scripts/run-with-bar.sh` (requires `../devel-bar`).

For an **experimental native session**, save your work and log in on a separate Linux text VT; never start it from a graphical terminal or over SSH. With `../devel-bar` and `../devel-menu` checked out beside this repository, run:

```sh
scripts/run-native-with-bar.sh --release
```

Native mode requires explicit opt-in, can take over the displays, and is not a security-verified locked session. Exit gracefully with Ctrl+Alt+Backspace; avoid `kill -9`. The script builds with the `native` feature. To run without the bar/menu, build with `cargo build --locked --features native --release` and start `./target/release/devel-wm --backend native --native-session` from a separate text VT.

For configuration and an example, see [doc/config.md](doc/config.md). Pass `--config PATH` to either launcher to select a config file.
