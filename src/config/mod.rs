//! Validated TOML shortcuts, layout gaps, terminal command and native startup output settings.

use std::{
    collections::HashMap,
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use smithay::input::keyboard::{Keysym, ModifiersState, keysyms, xkb};

use crate::wm::{Direction, WorkspaceId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    SwitchWorkspace(WorkspaceId),
    MoveFocusedToWorkspace(WorkspaceId),
    FocusTile(Direction),
    SwapTile(Direction),
    PromoteFocusedToMaster,
    AdjustMasterRatio(Direction),
    ToggleFloating,
    LaunchTerminal,
    CloseFocused,
    ToggleTilingMode,
}

/// Native output startup request; `None` uses the connector's preferred mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputMode {
    pub width: u16,
    pub height: u16,
    pub refresh_hz: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputConfig {
    mode: Option<String>,
    #[serde(default = "default_output_scale")]
    scale: i32,
}

fn default_output_scale() -> i32 {
    1
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputSettings {
    pub mode: Option<OutputMode>,
    pub scale: i32,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            mode: None,
            scale: 1,
        }
    }
}

fn valid_output_name(name: &str) -> bool {
    name.rsplit_once('-').is_some_and(|(kind, number)| {
        !kind.is_empty()
            && kind
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
            && !kind.starts_with('-')
            && !kind.ends_with('-')
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn parse_output_mode(text: &str) -> Result<OutputMode, ConfigError> {
    let error = || {
        ConfigError(format!(
            "invalid output mode '{text}'; expected WIDTHxHEIGHT@HZ (for example 1920x1080@144)"
        ))
    };
    let (size, rate) = text.split_once('@').ok_or_else(error)?;
    let (width, height) = size.split_once('x').ok_or_else(error)?;
    let width: u16 = width.parse().map_err(|_| error())?;
    let height: u16 = height.parse().map_err(|_| error())?;
    let refresh_hz: u32 = rate.parse().map_err(|_| error())?;
    if width == 0 || height == 0 || refresh_hz == 0 {
        return Err(error());
    }
    Ok(OutputMode {
        width,
        height,
        refresh_hz,
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Chord {
    modifiers: u8,
    key: u32,
}

const SHIFT: u8 = 1;
const CTRL: u8 = 2;
const ALT: u8 = 4;
const SUPER: u8 = 8;

fn normalized_key(key: u32) -> u32 {
    if (keysyms::KEY_A..=keysyms::KEY_Z).contains(&key) {
        key + (keysyms::KEY_a - keysyms::KEY_A)
    } else {
        key
    }
}

impl Chord {
    fn from_input(mods: &ModifiersState, key: Keysym) -> Self {
        Self {
            modifiers: (u8::from(mods.shift) * SHIFT)
                | (u8::from(mods.ctrl) * CTRL)
                | (u8::from(mods.alt) * ALT)
                | (u8::from(mods.logo) * SUPER),
            key: normalized_key(key.raw()),
        }
    }

    fn parse(text: &str) -> Result<Self, ConfigError> {
        let parts: Vec<_> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|part| part.is_empty()) {
            return Err(ConfigError(format!(
                "invalid binding '{text}': empty key or modifier"
            )));
        }
        let (key, modifiers) = parts.split_last().expect("nonempty binding");
        let mut bits = 0;
        for modifier in modifiers {
            let bit = match modifier.to_ascii_lowercase().as_str() {
                "shift" => SHIFT,
                "ctrl" | "control" => CTRL,
                "alt" => ALT,
                "super" | "logo" => SUPER,
                _ => {
                    return Err(ConfigError(format!(
                        "invalid modifier '{modifier}' in '{text}'"
                    )));
                }
            };
            if bits & bit != 0 {
                return Err(ConfigError(format!("duplicate modifier in '{text}'")));
            }
            bits |= bit;
        }
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "shift" | "ctrl" | "control" | "alt" | "super" | "logo"
        ) {
            return Err(ConfigError(format!(
                "binding '{text}' needs a non-modifier key"
            )));
        }
        if key.contains('\0') {
            return Err(ConfigError(format!("invalid key name in '{text}'")));
        }
        let mut sym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
        if sym.raw() == keysyms::KEY_NoSymbol {
            sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
        }
        if sym.raw() == keysyms::KEY_NoSymbol {
            return Err(ConfigError(format!("unknown key '{key}' in '{text}'")));
        }
        Ok(Self {
            modifiers: bits,
            key: normalized_key(sym.raw()),
        })
    }
}

#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl Error for ConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    keybindings: HashMap<String, String>,
    #[serde(default)]
    terminal: TerminalConfig,
    #[serde(default)]
    outputs: HashMap<String, OutputConfig>,
    #[serde(default)]
    layout: LayoutConfig,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutConfig {
    #[serde(default = "default_gap")]
    inner_gap: i32,
    #[serde(default = "default_gap")]
    outer_gap: i32,
}

fn default_gap() -> i32 {
    8
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            inner_gap: default_gap(),
            outer_gap: default_gap(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalConfig {
    #[serde(default = "default_terminal_command")]
    command: Vec<String>,
}

fn default_terminal_command() -> Vec<String> {
    vec!["alacritty".into()]
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            command: default_terminal_command(),
        }
    }
}

#[derive(Clone)]
pub struct Keybindings {
    bindings: HashMap<Chord, Action>,
    terminal: TerminalConfig,
    outputs: HashMap<String, OutputSettings>,
    layout: LayoutConfig,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self::from_overrides(HashMap::new(), TerminalConfig::default())
            .expect("built-in bindings must be valid")
    }
}

impl Keybindings {
    /// The named actions are overridden individually; unspecified defaults remain active.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: FileConfig =
            toml::from_str(text).map_err(|e| ConfigError(format!("invalid TOML: {e}")))?;
        let mut keys = Self::from_overrides(file.keybindings, file.terminal)?;
        for (name, value) in [
            ("inner_gap", file.layout.inner_gap),
            ("outer_gap", file.layout.outer_gap),
        ] {
            if value < 0 {
                return Err(ConfigError(format!(
                    "layout.{name} must be nonnegative logical pixels"
                )));
            }
        }
        keys.layout = file.layout;
        for (name, config) in file.outputs {
            if !valid_output_name(&name) {
                return Err(ConfigError(format!(
                    "invalid native output name '{name}'; expected a connector name such as HDMI-A-2 or DP-4"
                )));
            }
            if !(1..=4).contains(&config.scale) {
                return Err(ConfigError(format!(
                    "output '{name}' scale must be an integer from 1 to 4"
                )));
            }
            keys.outputs.insert(
                name,
                OutputSettings {
                    mode: config.mode.as_deref().map(parse_output_mode).transpose()?,
                    scale: config.scale,
                },
            );
        }
        Ok(keys)
    }

    pub fn load(explicit: Option<&Path>) -> Result<Self, ConfigError> {
        let path = explicit.map(Path::to_path_buf).or_else(default_path);
        let Some(path) = path else {
            return Ok(Self::default());
        };
        match fs::read_to_string(&path) {
            Ok(text) => {
                Self::parse(&text).map_err(|e| ConfigError(format!("{}: {e}", path.display())))
            }
            Err(error) if explicit.is_none() && error.kind() == io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(error) => Err(ConfigError(format!("{}: {error}", path.display()))),
        }
    }

    pub fn lookup(&self, modifiers: &ModifiersState, key: Keysym) -> Option<Action> {
        self.bindings
            .get(&Chord::from_input(modifiers, key))
            .copied()
    }

    pub fn terminal_command(&self) -> (&str, &[String]) {
        (&self.terminal.command[0], &self.terminal.command[1..])
    }

    pub fn output_settings(&self, connector: &str) -> OutputSettings {
        self.outputs.get(connector).copied().unwrap_or_default()
    }

    pub fn layout_gaps(&self) -> (i32, i32) {
        (self.layout.inner_gap, self.layout.outer_gap)
    }

    fn from_overrides(
        mut overrides: HashMap<String, String>,
        terminal: TerminalConfig,
    ) -> Result<Self, ConfigError> {
        if terminal.command.is_empty()
            || terminal
                .command
                .iter()
                .any(|part| part.is_empty() || part.contains('\0'))
        {
            return Err(ConfigError("terminal.command must be a nonempty array of nonempty program/argument strings without NUL bytes".into()));
        }
        let mut named = Vec::new();
        for number in 1..=9 {
            let workspace = WorkspaceId::new(number).expect("nonzero workspace");
            named.push((Action::SwitchWorkspace(workspace), format!("Alt+{number}")));
            named.push((
                Action::MoveFocusedToWorkspace(workspace),
                format!("Alt+Shift+{number}"),
            ));
        }
        named.extend([
            (Action::FocusTile(Direction::Next), "Alt+j".to_owned()),
            (Action::FocusTile(Direction::Previous), "Alt+k".to_owned()),
            (Action::SwapTile(Direction::Next), "Alt+Shift+j".to_owned()),
            (
                Action::SwapTile(Direction::Previous),
                "Alt+Shift+k".to_owned(),
            ),
            (Action::PromoteFocusedToMaster, "Alt+Return".to_owned()),
            (
                Action::AdjustMasterRatio(Direction::Previous),
                "Alt+h".to_owned(),
            ),
            (
                Action::AdjustMasterRatio(Direction::Next),
                "Alt+l".to_owned(),
            ),
            (Action::ToggleFloating, "Alt+space".to_owned()),
            (Action::LaunchTerminal, "Alt+Shift+Return".to_owned()),
            (Action::CloseFocused, "Alt+Ctrl+q".to_owned()),
            (Action::ToggleTilingMode, "Alt+t".to_owned()),
        ]);
        let mut bindings = HashMap::new();
        for (action, default) in named {
            let name = action_name(action);
            let binding = overrides.remove(&name).unwrap_or(default);
            let chord = Chord::parse(&binding)?;
            if let Some(other) = bindings.insert(chord, action) {
                return Err(ConfigError(format!(
                    "binding '{binding}' for '{name}' conflicts with '{}'",
                    action_name(other)
                )));
            }
        }
        for (name, binding) in overrides {
            let action = parse_action(&name)
                .ok_or_else(|| ConfigError(format!("unknown action '{name}'")))?;
            if action_name(action) != name {
                return Err(ConfigError(format!("unknown action '{name}'")));
            }
            let chord = Chord::parse(&binding)?;
            if let Some(other) = bindings.insert(chord, action) {
                return Err(ConfigError(format!(
                    "binding '{binding}' for '{name}' conflicts with '{}'",
                    action_name(other)
                )));
            }
        }
        Ok(Self {
            bindings,
            terminal,
            outputs: HashMap::new(),
            layout: LayoutConfig::default(),
        })
    }
}

fn action_name(action: Action) -> String {
    match action {
        Action::SwitchWorkspace(workspace) => format!("switch-workspace-{}", workspace.number()),
        Action::MoveFocusedToWorkspace(workspace) => {
            format!("move-focused-to-workspace-{}", workspace.number())
        }
        Action::FocusTile(Direction::Next) => "focus-next".into(),
        Action::FocusTile(Direction::Previous) => "focus-previous".into(),
        Action::SwapTile(Direction::Next) => "swap-next".into(),
        Action::SwapTile(Direction::Previous) => "swap-previous".into(),
        Action::PromoteFocusedToMaster => "promote-master".into(),
        Action::AdjustMasterRatio(Direction::Previous) => "shrink-master".into(),
        Action::AdjustMasterRatio(Direction::Next) => "grow-master".into(),
        Action::ToggleFloating => "toggle-floating".into(),
        Action::LaunchTerminal => "launch-terminal".into(),
        Action::CloseFocused => "close-focused".into(),
        Action::ToggleTilingMode => "toggle-tiling-mode".into(),
    }
}

fn parse_action(name: &str) -> Option<Action> {
    let (prefix, number) = name.rsplit_once('-')?;
    let workspace = WorkspaceId::new(number.parse().ok()?)?;
    match prefix {
        "switch-workspace" => Some(Action::SwitchWorkspace(workspace)),
        "move-focused-to-workspace" => Some(Action::MoveFocusedToWorkspace(workspace)),
        _ => None,
    }
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("devel-wm/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_output_settings_default_and_validate_requests() {
        let keys = Keybindings::parse(
            "[outputs.HDMI-A-2]\nmode = '3840x2160@60'\nscale = 2\n\n[outputs.DP-4]\nscale = 1\n",
        )
        .unwrap();
        assert_eq!(
            keys.output_settings("HDMI-A-2"),
            OutputSettings {
                mode: Some(OutputMode {
                    width: 3840,
                    height: 2160,
                    refresh_hz: 60
                }),
                scale: 2,
            }
        );
        assert_eq!(keys.output_settings("DP-4"), OutputSettings::default());
        assert_eq!(
            Keybindings::parse("[outputs.HDMI-A-1]\nscale = 2\n")
                .unwrap()
                .output_settings("HDMI-A-1")
                .scale,
            2
        );
        assert_eq!(
            Keybindings::default().output_settings("HDMI-A-2"),
            OutputSettings::default()
        );
        for invalid in [
            "[outputs.DP-4]\nscale = 0\n",
            "[outputs.DP-4]\nscale = 5\n",
            "[outputs.DP-4]\nscale = 1.5\n",
            "[outputs.DP-4]\nmode = '0x1080@144'\n",
            "[outputs.DP-4]\nmode = '1920x1080@0'\n",
            "[outputs.DP-4]\nmode = '1920x1080@144@60'\n",
            "[outputs.DP-4]\nunknown = 1\n",
            "[outputs.'HDMI-A-']\nscale = 2\n",
            "[outputs.'DP-x']\nscale = 2\n",
        ] {
            assert!(Keybindings::parse(invalid).is_err(), "{invalid}");
        }
    }

    fn mods(shift: bool, logo: bool) -> ModifiersState {
        ModifiersState {
            shift,
            logo,
            alt: !logo,
            ..Default::default()
        }
    }

    #[test]
    fn built_in_shortcuts_cover_every_workspace_and_window_action() {
        let keys = Keybindings::default();
        for number in 1..=9 {
            let key = Keysym::new(keysyms::KEY_1 + number - 1);
            let workspace = WorkspaceId::new(number).unwrap();
            assert_eq!(
                keys.lookup(&mods(false, false), key),
                Some(Action::SwitchWorkspace(workspace))
            );
            assert_eq!(
                keys.lookup(&mods(true, false), key),
                Some(Action::MoveFocusedToWorkspace(workspace))
            );
        }
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_j)),
            Some(Action::FocusTile(Direction::Next))
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_k)),
            Some(Action::FocusTile(Direction::Previous))
        );
        assert_eq!(
            keys.lookup(&mods(true, false), Keysym::new(keysyms::KEY_J)),
            Some(Action::SwapTile(Direction::Next))
        );
        assert_eq!(
            keys.lookup(&mods(true, false), Keysym::new(keysyms::KEY_K)),
            Some(Action::SwapTile(Direction::Previous))
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_Return)),
            Some(Action::PromoteFocusedToMaster)
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_space)),
            Some(Action::ToggleFloating)
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_h)),
            Some(Action::AdjustMasterRatio(Direction::Previous))
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_l)),
            Some(Action::AdjustMasterRatio(Direction::Next))
        );
        assert_eq!(
            keys.lookup(&mods(true, false), Keysym::new(keysyms::KEY_Return)),
            Some(Action::LaunchTerminal)
        );
        let ctrl_alt = ModifiersState {
            alt: true,
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            keys.lookup(&ctrl_alt, Keysym::new(keysyms::KEY_q)),
            Some(Action::CloseFocused)
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_t)),
            Some(Action::ToggleTilingMode)
        );
        assert_eq!(keys.terminal_command(), ("alacritty", &[][..]));
    }

    #[test]
    fn close_focused_binding_can_be_overridden_and_conflicts_are_rejected() {
        let keys = Keybindings::parse("[keybindings]\nclose-focused = 'Super+q'\n").unwrap();
        let logo = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert_eq!(
            keys.lookup(&logo, Keysym::new(keysyms::KEY_q)),
            Some(Action::CloseFocused)
        );
        assert_eq!(
            keys.lookup(
                &ModifiersState {
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                },
                Keysym::new(keysyms::KEY_q)
            ),
            None
        );
        assert!(Keybindings::parse("[keybindings]\nclose-focused = 'Alt+j'\n").is_err());
    }

    #[test]
    fn tiling_mode_binding_can_be_overridden_and_conflicts_are_rejected() {
        let keys = Keybindings::parse("[keybindings]\ntoggle-tiling-mode = 'Super+t'\n").unwrap();
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_t)),
            Some(Action::ToggleTilingMode)
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_t)),
            None
        );
        assert!(Keybindings::parse("[keybindings]\ntoggle-tiling-mode = 'Alt+j'\n").is_err());
    }

    #[test]
    fn defaults_and_partial_overrides_preserve_unmentioned_actions() {
        let keys = Keybindings::parse(
            "[keybindings]\nfocus-next = 'Super+h'\nswitch-workspace-2 = 'Super+2'\nshrink-master = 'Super+Left'\n",
        )
        .unwrap();
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_h)),
            Some(Action::FocusTile(Direction::Next))
        );
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_2)),
            Some(Action::SwitchWorkspace(WorkspaceId::new(2).unwrap()))
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_j)),
            None
        );
        assert_eq!(
            keys.lookup(&mods(true, false), Keysym::new(keysyms::KEY_J)),
            Some(Action::SwapTile(Direction::Next))
        );
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_Left)),
            Some(Action::AdjustMasterRatio(Direction::Previous))
        );
        assert_eq!(
            keys.lookup(&mods(false, false), Keysym::new(keysyms::KEY_h)),
            None
        );
    }

    #[test]
    fn layout_gaps_default_and_override_independently() {
        assert_eq!(Keybindings::default().layout_gaps(), (8, 8));
        assert_eq!(Keybindings::parse("").unwrap().layout_gaps(), (8, 8));
        assert_eq!(
            Keybindings::parse("[layout]\ninner_gap = 0\nouter_gap = 24\n")
                .unwrap()
                .layout_gaps(),
            (0, 24)
        );
        assert_eq!(
            Keybindings::parse("[layout]\nouter_gap = 0\n")
                .unwrap()
                .layout_gaps(),
            (8, 0)
        );
        for text in [
            "[layout]\ninner_gap = -1\n",
            "[layout]\nouter_gap = -1\n",
            "[layout]\ninner_gap = 1.5\n",
            "[layout]\nouter_gap = '8'\n",
            "[layout]\ngap = 8\n",
            "[layout]\nouter_gap = 2147483648\n",
        ] {
            assert!(Keybindings::parse(text).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn collision_and_invalid_names_are_reported() {
        for text in [
            "[keybindings]\nfocus-next = 'Alt+k'",
            "[keybindings]\nfocus-next = 'Alt+Shift+J'",
            "[keybindings]\nfocus-next = 'Alt+Alt+j'",
            "[keybindings]\nfocus-next = 'Alt+not_a_keysym'",
            "[keybindings]\nunknown-command = 'Super+x'",
            "[keybindings]\nswitch-workspace-01 = 'Super+1'",
            "[keybindings]\nfocus-next = 'Alt+'",
            "[keybindings]\nfocus-next = 'Meta+j'",
            "[keybindings]\nfocus-next = 'Alt+Shift'",
            "[unexpected]\nfoo = 1",
        ] {
            assert!(Keybindings::parse(text).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn extra_workspace_bindings_are_supported_and_explicit_missing_file_fails() {
        let keys = Keybindings::parse("[keybindings]\nswitch-workspace-12 = 'Super+F12'").unwrap();
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_F12)),
            Some(Action::SwitchWorkspace(WorkspaceId::new(12).unwrap()))
        );
        assert!(
            Keybindings::load(Some(Path::new("/unlikely/devel-wm-missing-config.toml"))).is_err()
        );
    }

    #[test]
    fn reordered_modifiers_match_the_same_chord() {
        let keys = Keybindings::parse("[keybindings]\nswap-next = 'shift+SUPER+H'\n").unwrap();
        assert_eq!(
            keys.lookup(&mods(true, true), Keysym::new(keysyms::KEY_h)),
            Some(Action::SwapTile(Direction::Next))
        );
    }

    #[test]
    fn terminal_program_arguments_and_shortcut_are_configurable() {
        let keys = Keybindings::parse(
            "[terminal]\ncommand = ['foot', '--title', 'my terminal']\n[keybindings]\nlaunch-terminal = 'Super+t'\n",
        ).unwrap();
        assert_eq!(
            keys.terminal_command(),
            (
                "foot",
                &["--title".to_owned(), "my terminal".to_owned()][..]
            )
        );
        assert_eq!(
            keys.lookup(&mods(false, true), Keysym::new(keysyms::KEY_t)),
            Some(Action::LaunchTerminal)
        );
        assert_eq!(
            keys.lookup(&mods(true, false), Keysym::new(keysyms::KEY_Return)),
            None
        );
    }

    #[test]
    fn invalid_terminal_commands_and_shortcut_conflicts_are_rejected() {
        for text in [
            "[terminal]\ncommand = []",
            "[terminal]\ncommand = ['']",
            "[terminal]\ncommand = ['foot', '']",
            "[terminal]\ncommand = [\"foo\\u0000bar\"]",
            "[terminal]\ncommand = 'foot'",
            "[terminal]\nunknown = 'foot'",
            "[keybindings]\nlaunch-terminal = 'Alt+Return'",
        ] {
            assert!(Keybindings::parse(text).is_err(), "accepted: {text}");
        }
    }
}
