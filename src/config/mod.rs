//! Validated, action-oriented TOML keybindings. The WM itself never parses keys.

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
}

#[derive(Clone)]
pub struct Keybindings {
    bindings: HashMap<Chord, Action>,
}

impl Default for Keybindings {
    fn default() -> Self {
        Self::from_overrides(HashMap::new()).expect("built-in bindings must be valid")
    }
}

impl Keybindings {
    /// The named actions are overridden individually; unspecified defaults remain active.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let file: FileConfig =
            toml::from_str(text).map_err(|e| ConfigError(format!("invalid TOML: {e}")))?;
        Self::from_overrides(file.keybindings)
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

    fn from_overrides(mut overrides: HashMap<String, String>) -> Result<Self, ConfigError> {
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
        Ok(Self { bindings })
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
    }

    #[test]
    fn defaults_and_partial_overrides_preserve_unmentioned_actions() {
        let keys = Keybindings::parse(
            "[keybindings]\nfocus-next = 'Super+h'\nswitch-workspace-2 = 'Super+2'\n",
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
}
