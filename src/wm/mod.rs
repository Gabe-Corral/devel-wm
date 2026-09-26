//! Smithay-independent window ownership, workspace selection, and focus.

use std::collections::{BTreeMap, btree_map::Entry};

/// Internal identity of a monitor. Allocated by [`WmState`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorId(u64);

/// Internal identity of a window. Allocated by [`WmState`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(u64);

/// A numbered workspace, scoped to a monitor rather than globally unique.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(u32);

impl WorkspaceId {
    pub const FIRST: Self = Self(1);

    /// Workspace numbering begins at 1.
    pub const fn new(number: u32) -> Option<Self> {
        if number == 0 {
            None
        } else {
            Some(Self(number))
        }
    }

    pub const fn number(self) -> u32 {
        self.0
    }
}

/// A workspace is identified by both its monitor and its local number.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WindowOwner {
    pub monitor: MonitorId,
    pub workspace: WorkspaceId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddWindowError {
    UnknownMonitor,
    UnknownWorkspace,
}

#[derive(Default)]
struct WorkspaceState {
    focused_window: Option<WindowId>,
}

struct MonitorState {
    active_workspace: WorkspaceId,
    workspaces: BTreeMap<WorkspaceId, WorkspaceState>,
}

/// Minimal WM state; ordering, geometry, and commands come later.
#[derive(Default)]
pub struct WmState {
    next_monitor_id: u64,
    next_window_id: u64,
    active_monitor: Option<MonitorId>,
    monitors: BTreeMap<MonitorId, MonitorState>,
    windows: BTreeMap<WindowId, WindowOwner>,
}

impl WmState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Each new monitor starts on workspace 1. The first becomes active.
    pub fn add_monitor(&mut self) -> MonitorId {
        let id = MonitorId(self.next_monitor_id);
        self.next_monitor_id = self
            .next_monitor_id
            .checked_add(1)
            .expect("monitor ID exhausted");
        self.monitors.insert(
            id,
            MonitorState {
                active_workspace: WorkspaceId::FIRST,
                workspaces: BTreeMap::from([(WorkspaceId::FIRST, WorkspaceState::default())]),
            },
        );
        self.active_monitor.get_or_insert(id);
        id
    }

    /// Register a numbered workspace on a monitor without changing its view.
    pub fn add_workspace(&mut self, monitor: MonitorId, workspace: WorkspaceId) -> bool {
        self.monitors.get_mut(&monitor).is_some_and(|state| {
            match state.workspaces.entry(workspace) {
                Entry::Vacant(entry) => {
                    entry.insert(WorkspaceState::default());
                    true
                }
                Entry::Occupied(_) => false,
            }
        })
    }

    pub fn active_monitor(&self) -> Option<MonitorId> {
        self.active_monitor
    }

    /// Select an existing monitor as the keyboard-focus source.
    pub fn set_active_monitor(&mut self, monitor: MonitorId) -> bool {
        if !self.monitors.contains_key(&monitor) {
            return false;
        }
        self.active_monitor = Some(monitor);
        true
    }

    pub fn active_workspace(&self, monitor: MonitorId) -> Option<WorkspaceId> {
        self.monitors
            .get(&monitor)
            .map(|state| state.active_workspace)
    }

    /// Switch only this monitor's view; an existing workspace is required.
    pub fn set_active_workspace(&mut self, monitor: MonitorId, workspace: WorkspaceId) -> bool {
        let Some(state) = self.monitors.get_mut(&monitor) else {
            return false;
        };
        if !state.workspaces.contains_key(&workspace) {
            return false;
        }
        state.active_workspace = workspace;
        true
    }

    pub fn focused_window(&self, monitor: MonitorId, workspace: WorkspaceId) -> Option<WindowId> {
        self.monitors
            .get(&monitor)?
            .workspaces
            .get(&workspace)?
            .focused_window
    }

    pub fn keyboard_focus(&self) -> Option<WindowId> {
        let monitor = self.active_monitor?;
        let workspace = self.active_workspace(monitor)?;
        self.focused_window(monitor, workspace)
    }

    /// Focus a window on this monitor's visible workspace, or clear its focus.
    /// Focusing a window also makes its monitor active.
    pub fn set_focus(&mut self, monitor: MonitorId, window: Option<WindowId>) -> bool {
        let Some(state) = self.monitors.get_mut(&monitor) else {
            return false;
        };
        if let Some(window) = window {
            if self.windows.get(&window)
                != Some(&WindowOwner {
                    monitor,
                    workspace: state.active_workspace,
                })
            {
                return false;
            }
            self.active_monitor = Some(monitor);
        }
        state
            .workspaces
            .get_mut(&state.active_workspace)
            .expect("active workspace must exist")
            .focused_window = window;
        true
    }

    pub fn add_window(&mut self, owner: WindowOwner) -> Result<WindowId, AddWindowError> {
        let workspaces = self
            .monitors
            .get(&owner.monitor)
            .ok_or(AddWindowError::UnknownMonitor)?;
        if !workspaces.workspaces.contains_key(&owner.workspace) {
            return Err(AddWindowError::UnknownWorkspace);
        }
        let id = WindowId(self.next_window_id);
        self.next_window_id = self
            .next_window_id
            .checked_add(1)
            .expect("window ID exhausted");
        self.windows.insert(id, owner);
        Ok(id)
    }

    pub fn owner(&self, window: WindowId) -> Option<WindowOwner> {
        self.windows.get(&window).copied()
    }

    pub fn remove_window(&mut self, window: WindowId) -> Option<WindowOwner> {
        let owner = self.windows.remove(&window)?;
        let focus = &mut self
            .monitors
            .get_mut(&owner.monitor)
            .expect("window's monitor must exist")
            .workspaces
            .get_mut(&owner.workspace)
            .expect("window's workspace must exist")
            .focused_window;
        if *focus == Some(window) {
            *focus = None;
        }
        Some(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_are_numbered_from_one() {
        assert_eq!(WorkspaceId::new(0), None);
        assert_eq!(WorkspaceId::new(1), Some(WorkspaceId::FIRST));
        assert_eq!(WorkspaceId::new(9).unwrap().number(), 9);
    }

    #[test]
    fn identical_workspace_numbers_on_different_monitors_have_distinct_owners() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let first_owner = WindowOwner {
            monitor: first,
            workspace: WorkspaceId::FIRST,
        };
        let second_owner = WindowOwner {
            monitor: second,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(first_owner).unwrap();
        let b = wm.add_window(second_owner).unwrap();
        assert_ne!(a, b);
        assert_eq!(wm.owner(a), Some(first_owner));
        assert_eq!(wm.owner(b), Some(second_owner));
    }

    #[test]
    fn windows_require_an_existing_monitor_and_workspace() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let missing_monitor = MonitorId(u64::MAX);
        let second = WorkspaceId::new(2).unwrap();
        assert_eq!(
            wm.add_window(WindowOwner {
                monitor: missing_monitor,
                workspace: WorkspaceId::FIRST
            }),
            Err(AddWindowError::UnknownMonitor)
        );
        assert_eq!(
            wm.add_window(WindowOwner {
                monitor,
                workspace: second
            }),
            Err(AddWindowError::UnknownWorkspace)
        );
        assert!(wm.add_workspace(monitor, second));
        assert!(
            wm.add_window(WindowOwner {
                monitor,
                workspace: second
            })
            .is_ok()
        );
    }

    #[test]
    fn removed_window_ids_are_not_reused() {
        let mut wm = WmState::new();
        let owner = WindowOwner {
            monitor: wm.add_monitor(),
            workspace: WorkspaceId::FIRST,
        };
        let first = wm.add_window(owner).unwrap();
        assert_eq!(wm.remove_window(first), Some(owner));
        assert_eq!(wm.owner(first), None);
        let second = wm.add_window(owner).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn monitors_select_workspaces_independently() {
        let mut wm = WmState::new();
        assert_eq!(wm.active_monitor(), None);
        assert_eq!(wm.keyboard_focus(), None);
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let workspace_two = WorkspaceId::new(2).unwrap();
        assert_eq!(wm.active_monitor(), Some(first));
        assert_eq!(wm.active_workspace(first), Some(WorkspaceId::FIRST));
        assert_eq!(wm.active_workspace(second), Some(WorkspaceId::FIRST));
        assert!(!wm.set_active_workspace(first, workspace_two));
        assert!(!wm.set_active_workspace(MonitorId(u64::MAX), WorkspaceId::FIRST));
        assert!(wm.add_workspace(first, workspace_two));
        assert!(wm.set_active_workspace(first, workspace_two));
        assert_eq!(wm.active_workspace(second), Some(WorkspaceId::FIRST));
        assert!(!wm.set_active_monitor(MonitorId(u64::MAX)));
        assert_eq!(wm.active_monitor(), Some(first));
        assert!(wm.set_active_monitor(second));
        assert_eq!(wm.active_monitor(), Some(second));
    }

    #[test]
    fn focus_is_per_workspace_and_keyboard_focus_follows_active_monitor() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let workspace_two = WorkspaceId::new(2).unwrap();
        wm.add_workspace(first, workspace_two);
        let first_window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let hidden_window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: workspace_two,
            })
            .unwrap();
        let second_window = wm
            .add_window(WindowOwner {
                monitor: second,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();

        assert!(!wm.set_focus(first, Some(hidden_window)));
        assert!(!wm.set_focus(first, Some(second_window)));
        assert!(!wm.set_focus(first, Some(WindowId(u64::MAX))));
        assert!(!wm.set_focus(MonitorId(u64::MAX), None));
        assert_eq!(wm.keyboard_focus(), None);
        assert!(wm.set_focus(first, Some(first_window)));
        assert_eq!(wm.keyboard_focus(), Some(first_window));
        assert!(wm.set_active_workspace(first, workspace_two));
        assert_eq!(wm.keyboard_focus(), None);
        assert!(wm.set_focus(first, Some(hidden_window)));
        assert_eq!(wm.keyboard_focus(), Some(hidden_window));
        assert!(!wm.add_workspace(first, workspace_two));
        assert_eq!(wm.focused_window(first, workspace_two), Some(hidden_window));

        assert!(wm.set_focus(second, Some(second_window)));
        assert_eq!(wm.active_monitor(), Some(second));
        assert_eq!(wm.keyboard_focus(), Some(second_window));
        assert!(wm.set_active_monitor(first));
        assert_eq!(wm.keyboard_focus(), Some(hidden_window));
        assert!(wm.set_active_workspace(first, WorkspaceId::FIRST));
        assert_eq!(wm.keyboard_focus(), Some(first_window));
        assert!(wm.set_focus(first, None));
        assert_eq!(wm.keyboard_focus(), None);
        assert_eq!(wm.focused_window(first, workspace_two), Some(hidden_window));
        assert_eq!(
            wm.focused_window(second, WorkspaceId::FIRST),
            Some(second_window)
        );
    }

    #[test]
    fn removing_focused_window_clears_only_its_workspace_focus() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let second = WorkspaceId::new(2).unwrap();
        wm.add_workspace(monitor, second);
        let first_window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let second_window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: second,
            })
            .unwrap();
        wm.set_focus(monitor, Some(first_window));
        wm.set_active_workspace(monitor, second);
        wm.set_focus(monitor, Some(second_window));
        assert_eq!(
            wm.remove_window(first_window).unwrap().workspace,
            WorkspaceId::FIRST
        );
        assert_eq!(wm.focused_window(monitor, WorkspaceId::FIRST), None);
        assert_eq!(wm.keyboard_focus(), Some(second_window));
        assert_eq!(wm.remove_window(second_window).unwrap().workspace, second);
        assert_eq!(wm.keyboard_focus(), None);
        assert_eq!(wm.remove_window(second_window), None);
        assert!(!wm.set_focus(monitor, Some(second_window)));
    }
}
