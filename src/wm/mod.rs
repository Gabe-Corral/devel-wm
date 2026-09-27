//! Smithay-independent window ownership, workspace selection, and focus.

use std::collections::{BTreeMap, btree_map::Entry};

use crate::layout::{MasterRatio, Rect};

const MASTER_RATIO_STEP: i32 = 50;
const MASTER_RATIO_MIN: i32 = 200;
const MASTER_RATIO_MAX: i32 = 800;

/// Internal identity of a monitor. Allocated by [`WmState`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorId(u64);

impl MonitorId {
    pub const fn number(self) -> u64 {
        self.0
    }
}

/// Internal identity of a window. Allocated by [`WmState`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(u64);

impl WindowId {
    pub const fn number(self) -> u64 {
        self.0
    }
}

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

/// Policy operations; keyboard shortcuts and future IPC translate into these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WmCommand {
    SwitchWorkspace {
        monitor: MonitorId,
        workspace: WorkspaceId,
    },
    MoveFocusedToWorkspace {
        monitor: MonitorId,
        workspace: WorkspaceId,
    },
    FocusTile {
        monitor: MonitorId,
        direction: Direction,
    },
    SwapTile {
        monitor: MonitorId,
        direction: Direction,
    },
    PromoteFocusedToMaster {
        monitor: MonitorId,
    },
    AdjustMasterRatio {
        monitor: MonitorId,
        direction: Direction,
    },
    ToggleFocusedFloating {
        monitor: MonitorId,
        tiled_rect: Rect,
    },
    FloatFocusedAt {
        monitor: MonitorId,
        rect: Rect,
    },
    MoveFloating {
        monitor: MonitorId,
        window: WindowId,
        x: i32,
        y: i32,
    },
    ResizeFloating {
        monitor: MonitorId,
        window: WindowId,
        rect: Rect,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandError {
    UnknownMonitor,
    NoFocusedWindow,
    UnknownWindow,
    NotFloating,
    WrongMonitor,
    InvalidGeometry,
}

/// Describes what an adapter must reconcile after a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandEffect {
    Unchanged,
    FocusChanged {
        monitor: MonitorId,
    },
    TileOrderChanged {
        monitor: MonitorId,
    },
    LayoutChanged {
        monitor: MonitorId,
    },
    GeometryChanged {
        monitor: MonitorId,
        window: WindowId,
    },
    WorkspaceSwitched {
        monitor: MonitorId,
    },
    WindowMoved {
        monitor: MonitorId,
        window: WindowId,
    },
}

struct WorkspaceState {
    focused_window: Option<WindowId>,
    tiled: Vec<WindowId>,
    floating: Vec<WindowId>,
    master_ratio: MasterRatio,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            focused_window: None,
            tiled: Vec::new(),
            floating: Vec::new(),
            master_ratio: MasterRatio::new(600).expect("valid default ratio"),
        }
    }
}

impl WorkspaceState {
    fn remove_window(&mut self, window: WindowId, floating: bool) {
        let (order, other) = if floating {
            (&mut self.floating, &self.tiled)
        } else {
            (&mut self.tiled, &self.floating)
        };
        let index = order
            .iter()
            .position(|&id| id == window)
            .expect("registered window");
        order.remove(index);
        if self.focused_window == Some(window) {
            self.focused_window = successor(order, index).or_else(|| other.last().copied());
        }
    }
}

struct MonitorState {
    active_workspace: WorkspaceId,
    workspaces: BTreeMap<WorkspaceId, WorkspaceState>,
}

/// Monitor-local workspace ownership, tile order, and focus policy.
#[derive(Default)]
pub struct WmState {
    next_monitor_id: u64,
    next_window_id: u64,
    active_monitor: Option<MonitorId>,
    monitors: BTreeMap<MonitorId, MonitorState>,
    windows: BTreeMap<WindowId, WindowOwner>,
    floating_rects: BTreeMap<WindowId, Rect>,
    saved_floating_rects: BTreeMap<WindowId, Rect>,
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

    pub fn monitors(&self) -> impl Iterator<Item = MonitorId> + '_ {
        self.monitors.keys().copied()
    }

    pub fn workspaces(&self, monitor: MonitorId) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.monitors
            .get(&monitor)
            .into_iter()
            .flat_map(|state| state.workspaces.keys().copied())
    }

    pub fn windows(&self) -> impl Iterator<Item = (WindowId, WindowOwner)> + '_ {
        self.windows.iter().map(|(&id, &owner)| (id, owner))
    }

    pub fn tiled_windows(&self, monitor: MonitorId, workspace: WorkspaceId) -> Option<&[WindowId]> {
        Some(
            &self
                .monitors
                .get(&monitor)?
                .workspaces
                .get(&workspace)?
                .tiled,
        )
    }

    pub fn floating_windows(
        &self,
        monitor: MonitorId,
        workspace: WorkspaceId,
    ) -> Option<&[WindowId]> {
        Some(
            &self
                .monitors
                .get(&monitor)?
                .workspaces
                .get(&workspace)?
                .floating,
        )
    }

    pub fn floating_rect(&self, window: WindowId) -> Option<Rect> {
        self.floating_rects.get(&window).copied()
    }

    pub fn master_ratio(&self, monitor: MonitorId, workspace: WorkspaceId) -> Option<MasterRatio> {
        Some(
            self.monitors
                .get(&monitor)?
                .workspaces
                .get(&workspace)?
                .master_ratio,
        )
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

    /// Switch only this monitor's view, creating the workspace if needed.
    pub fn set_active_workspace(&mut self, monitor: MonitorId, workspace: WorkspaceId) -> bool {
        let Some(state) = self.monitors.get_mut(&monitor) else {
            return false;
        };
        state.workspaces.entry(workspace).or_default();
        state.active_workspace = workspace;
        true
    }

    pub fn execute(&mut self, command: WmCommand) -> Result<CommandEffect, CommandError> {
        match command {
            WmCommand::SwitchWorkspace { monitor, workspace } => {
                if !self.monitors.contains_key(&monitor) {
                    return Err(CommandError::UnknownMonitor);
                }
                if self.active_workspace(monitor) == Some(workspace) {
                    return Ok(CommandEffect::Unchanged);
                }
                self.set_active_workspace(monitor, workspace);
                Ok(CommandEffect::WorkspaceSwitched { monitor })
            }
            WmCommand::MoveFocusedToWorkspace { monitor, workspace } => {
                let source = self
                    .monitors
                    .get(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?
                    .active_workspace;
                let window = self
                    .focused_window(monitor, source)
                    .ok_or(CommandError::NoFocusedWindow)?;
                if source == workspace {
                    return Ok(CommandEffect::Unchanged);
                }
                let monitor_state = self.monitors.get_mut(&monitor).expect("known monitor");
                let floating = self.floating_rects.contains_key(&window);
                let source_state = monitor_state
                    .workspaces
                    .get_mut(&source)
                    .expect("active workspace");
                source_state.remove_window(window, floating);
                let target = monitor_state.workspaces.entry(workspace).or_default();
                target.focused_window.get_or_insert(window);
                if floating {
                    target.floating.push(window);
                } else {
                    target.tiled.push(window);
                }
                self.windows
                    .get_mut(&window)
                    .expect("focused window must exist")
                    .workspace = workspace;
                Ok(CommandEffect::WindowMoved { monitor, window })
            }
            WmCommand::FocusTile { monitor, direction } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let count = active.tiled.len() + active.floating.len();
                if count == 0 {
                    return Ok(CommandEffect::Unchanged);
                }
                let index = active.focused_window.and_then(|id| {
                    active
                        .tiled
                        .iter()
                        .position(|&tile| tile == id)
                        .or_else(|| {
                            active
                                .floating
                                .iter()
                                .position(|&tile| tile == id)
                                .map(|i| active.tiled.len() + i)
                        })
                });
                let next = match (index, direction) {
                    (Some(i), Direction::Next) => (i + 1) % count,
                    (Some(i), Direction::Previous) => (i + count - 1) % count,
                    (None, Direction::Next) => 0,
                    (None, Direction::Previous) => count - 1,
                };
                let window = active
                    .tiled
                    .get(next)
                    .or_else(|| active.floating.get(next - active.tiled.len()))
                    .copied()
                    .expect("nonempty order");
                if active.focused_window == Some(window) && self.active_monitor == Some(monitor) {
                    return Ok(CommandEffect::Unchanged);
                }
                active.focused_window = Some(window);
                self.active_monitor = Some(monitor);
                Ok(CommandEffect::FocusChanged { monitor })
            }
            WmCommand::SwapTile { monitor, direction } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                let index = active.tiled.iter().position(|&id| id == window);
                let Some(index) = index else {
                    return Ok(CommandEffect::Unchanged);
                };
                let neighbor = match direction {
                    Direction::Previous => index.checked_sub(1),
                    Direction::Next => (index + 1 < active.tiled.len()).then_some(index + 1),
                };
                let Some(neighbor) = neighbor else {
                    return Ok(CommandEffect::Unchanged);
                };
                active.tiled.swap(index, neighbor);
                Ok(CommandEffect::TileOrderChanged { monitor })
            }
            WmCommand::PromoteFocusedToMaster { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                let index = active.tiled.iter().position(|&id| id == window);
                let Some(index) = index else {
                    return Ok(CommandEffect::Unchanged);
                };
                if index == 0 {
                    return Ok(CommandEffect::Unchanged);
                }
                active.tiled.remove(index);
                active.tiled.insert(0, window);
                Ok(CommandEffect::TileOrderChanged { monitor })
            }
            WmCommand::AdjustMasterRatio { monitor, direction } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                if active.tiled.len() < 2 {
                    return Ok(CommandEffect::Unchanged);
                }
                let delta = match direction {
                    Direction::Previous => -MASTER_RATIO_STEP,
                    Direction::Next => MASTER_RATIO_STEP,
                };
                let ratio = (i32::from(active.master_ratio.thousandths()) + delta)
                    .clamp(MASTER_RATIO_MIN, MASTER_RATIO_MAX) as u16;
                if active.master_ratio.thousandths() == ratio {
                    return Ok(CommandEffect::Unchanged);
                }
                active.master_ratio = MasterRatio::new(ratio).expect("clamped ratio");
                Ok(CommandEffect::LayoutChanged { monitor })
            }
            WmCommand::ToggleFocusedFloating {
                monitor,
                tiled_rect,
            } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                if let Some(rect) = self.floating_rects.remove(&window) {
                    active.floating.retain(|&id| id != window);
                    active.tiled.push(window);
                    self.saved_floating_rects.insert(window, rect);
                } else {
                    let rect = self
                        .saved_floating_rects
                        .get(&window)
                        .copied()
                        .unwrap_or(tiled_rect);
                    if !valid_floating_rect(rect) {
                        return Err(CommandError::InvalidGeometry);
                    }
                    active.tiled.retain(|&id| id != window);
                    active.floating.push(window);
                    self.saved_floating_rects.remove(&window);
                    self.floating_rects.insert(window, rect);
                }
                Ok(CommandEffect::TileOrderChanged { monitor })
            }
            WmCommand::FloatFocusedAt { monitor, rect } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                if self.floating_rects.contains_key(&window) {
                    return Ok(CommandEffect::Unchanged);
                }
                if !valid_floating_rect(rect) {
                    return Err(CommandError::InvalidGeometry);
                }
                let index = active
                    .tiled
                    .iter()
                    .position(|&id| id == window)
                    .expect("focused tile");
                active.tiled.remove(index);
                active.floating.push(window);
                self.saved_floating_rects.remove(&window);
                self.floating_rects.insert(window, rect);
                Ok(CommandEffect::TileOrderChanged { monitor })
            }
            WmCommand::MoveFloating {
                monitor,
                window,
                x,
                y,
            } => {
                let owner = self
                    .windows
                    .get(&window)
                    .ok_or(CommandError::UnknownWindow)?;
                if !self.monitors.contains_key(&monitor) {
                    return Err(CommandError::UnknownMonitor);
                }
                if owner.monitor != monitor {
                    return Err(CommandError::WrongMonitor);
                }
                let rect = self
                    .floating_rects
                    .get_mut(&window)
                    .ok_or(CommandError::NotFloating)?;
                if x.checked_add(rect.width).is_none() || y.checked_add(rect.height).is_none() {
                    return Err(CommandError::InvalidGeometry);
                }
                if (rect.x, rect.y) == (x, y) {
                    return Ok(CommandEffect::Unchanged);
                }
                rect.x = x;
                rect.y = y;
                Ok(CommandEffect::GeometryChanged { monitor, window })
            }
            WmCommand::ResizeFloating {
                monitor,
                window,
                rect,
            } => {
                let owner = self
                    .windows
                    .get(&window)
                    .ok_or(CommandError::UnknownWindow)?;
                if !self.monitors.contains_key(&monitor) {
                    return Err(CommandError::UnknownMonitor);
                }
                if owner.monitor != monitor {
                    return Err(CommandError::WrongMonitor);
                }
                let current = self
                    .floating_rects
                    .get_mut(&window)
                    .ok_or(CommandError::NotFloating)?;
                if !valid_floating_rect(rect) {
                    return Err(CommandError::InvalidGeometry);
                }
                if *current == rect {
                    return Ok(CommandEffect::Unchanged);
                }
                *current = rect;
                Ok(CommandEffect::GeometryChanged { monitor, window })
            }
        }
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
        let state = self
            .monitors
            .get_mut(&owner.monitor)
            .expect("known monitor");
        let visible = state.active_workspace == owner.workspace;
        let workspace = state
            .workspaces
            .get_mut(&owner.workspace)
            .expect("known workspace");
        workspace.tiled.push(id);
        if visible {
            workspace.focused_window = Some(id);
        }
        Ok(id)
    }

    pub fn owner(&self, window: WindowId) -> Option<WindowOwner> {
        self.windows.get(&window).copied()
    }

    /// Visibility follows the owning monitor's active workspace.
    pub fn is_window_visible(&self, window: WindowId) -> bool {
        let Some(owner) = self.owner(window) else {
            return false;
        };
        self.active_workspace(owner.monitor) == Some(owner.workspace)
    }

    pub fn remove_window(&mut self, window: WindowId) -> Option<WindowOwner> {
        let owner = self.windows.remove(&window)?;
        let floating = self.floating_rects.remove(&window).is_some();
        self.saved_floating_rects.remove(&window);
        let workspace = self
            .monitors
            .get_mut(&owner.monitor)
            .expect("window's monitor must exist")
            .workspaces
            .get_mut(&owner.workspace)
            .expect("window's workspace must exist");
        workspace.remove_window(window, floating);
        Some(owner)
    }
}

fn successor(tiled: &[WindowId], removed_index: usize) -> Option<WindowId> {
    tiled.get(removed_index).or_else(|| tiled.last()).copied()
}

fn valid_floating_rect(rect: Rect) -> bool {
    rect.width > 0
        && rect.height > 0
        && rect.x.checked_add(rect.width).is_some()
        && rect.y.checked_add(rect.height).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_ratio_is_workspace_local_and_clamped() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let other = WorkspaceId::new(2).unwrap();
        for _ in 0..2 {
            wm.add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
            wm.add_window(WindowOwner {
                monitor: second,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        }
        wm.add_workspace(first, other);
        let grow = WmCommand::AdjustMasterRatio {
            monitor: first,
            direction: Direction::Next,
        };
        assert_eq!(
            wm.execute(grow),
            Ok(CommandEffect::LayoutChanged { monitor: first })
        );
        assert_eq!(
            wm.master_ratio(first, WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            650
        );
        let tiled = wm.tiled_windows(first, WorkspaceId::FIRST).unwrap();
        let area = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        let before =
            crate::layout::master_stack(area, tiled, crate::layout::LayoutSettings::default())
                .unwrap();
        let after = crate::layout::master_stack(
            area,
            tiled,
            crate::layout::LayoutSettings {
                master_ratio: wm.master_ratio(first, WorkspaceId::FIRST).unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(after[0].content.width > before[0].content.width);
        assert_eq!(
            wm.master_ratio(second, WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            600
        );
        wm.execute(WmCommand::SwitchWorkspace {
            monitor: first,
            workspace: other,
        })
        .unwrap();
        assert_eq!(wm.master_ratio(first, other).unwrap().thousandths(), 600);
        assert_eq!(wm.execute(grow), Ok(CommandEffect::Unchanged));
        wm.execute(WmCommand::SwitchWorkspace {
            monitor: first,
            workspace: WorkspaceId::FIRST,
        })
        .unwrap();
        for _ in 0..10 {
            wm.execute(grow).unwrap();
        }
        assert_eq!(
            wm.master_ratio(first, WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            800
        );
        assert_eq!(wm.execute(grow), Ok(CommandEffect::Unchanged));
        for _ in 0..20 {
            wm.execute(WmCommand::AdjustMasterRatio {
                monitor: first,
                direction: Direction::Previous,
            })
            .unwrap();
        }
        assert_eq!(
            wm.master_ratio(first, WorkspaceId::FIRST)
                .unwrap()
                .thousandths(),
            200
        );
        assert_eq!(
            wm.execute(WmCommand::AdjustMasterRatio {
                monitor: MonitorId(u64::MAX),
                direction: Direction::Next
            }),
            Err(CommandError::UnknownMonitor)
        );
    }

    #[test]
    fn floating_resize_validates_ownership_and_dimensions() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let rect = Rect {
            x: 50,
            y: 60,
            width: 240,
            height: 180,
        };
        let command = WmCommand::ResizeFloating {
            monitor: first,
            window,
            rect,
        };
        assert_eq!(wm.execute(command), Err(CommandError::NotFloating));
        wm.execute(WmCommand::FloatFocusedAt {
            monitor: first,
            rect,
        })
        .unwrap();
        assert_eq!(wm.execute(command), Ok(CommandEffect::Unchanged));
        assert_eq!(
            wm.execute(WmCommand::ResizeFloating {
                monitor: second,
                window,
                rect
            }),
            Err(CommandError::WrongMonitor)
        );
        assert_eq!(
            wm.execute(WmCommand::ResizeFloating {
                monitor: first,
                window,
                rect: Rect { width: 0, ..rect }
            }),
            Err(CommandError::InvalidGeometry)
        );
        assert_eq!(wm.floating_rect(window), Some(rect));
        let resized = Rect {
            x: 20,
            y: 60,
            width: 270,
            height: 180,
        };
        assert_eq!(
            wm.execute(WmCommand::ResizeFloating {
                monitor: first,
                window,
                rect: resized
            }),
            Ok(CommandEffect::GeometryChanged {
                monitor: first,
                window
            })
        );
        assert_eq!(wm.floating_rect(window), Some(resized));
    }

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
        assert!(wm.set_active_workspace(first, workspace_two));
        assert!(!wm.set_active_workspace(MonitorId(u64::MAX), WorkspaceId::FIRST));
        assert!(!wm.add_workspace(first, workspace_two));
        assert_eq!(wm.active_workspace(first), Some(workspace_two));
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
        assert_eq!(wm.keyboard_focus(), Some(first_window));
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
    fn switching_to_new_workspace_starts_unfocused_and_restores_previous_focus() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        assert!(wm.set_focus(first, Some(window)));

        let third = WorkspaceId::new(3).unwrap();
        assert!(wm.set_active_workspace(first, third));
        assert_eq!(wm.active_workspace(first), Some(third));
        assert_eq!(wm.keyboard_focus(), None);
        assert_eq!(wm.focused_window(first, third), None);
        assert_eq!(wm.active_workspace(second), Some(WorkspaceId::FIRST));
        assert!(wm.set_active_workspace(first, WorkspaceId::FIRST));
        assert_eq!(wm.keyboard_focus(), Some(window));
        assert!(!wm.add_workspace(first, third));
    }

    #[test]
    fn switching_one_monitor_changes_only_its_window_visibility() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let a = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let b = wm
            .add_window(WindowOwner {
                monitor: second,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        assert!(wm.is_window_visible(a));
        assert!(wm.is_window_visible(b));
        wm.set_active_workspace(first, WorkspaceId::new(2).unwrap());
        assert!(!wm.is_window_visible(a));
        assert!(wm.is_window_visible(b));
        wm.set_active_workspace(first, WorkspaceId::FIRST);
        assert!(wm.is_window_visible(a));
        wm.remove_window(a);
        assert!(!wm.is_window_visible(a));
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

    #[test]
    fn commands_move_focus_without_switching_and_restore_it_on_target() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let other = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let other_window = wm
            .add_window(WindowOwner {
                monitor: other,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        assert!(wm.set_focus(monitor, Some(window)));
        let two = WorkspaceId::new(2).unwrap();
        assert_eq!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor,
                workspace: two
            }),
            Ok(CommandEffect::WindowMoved { monitor, window })
        );
        assert_eq!(
            wm.owner(window),
            Some(WindowOwner {
                monitor,
                workspace: two
            })
        );
        assert_eq!(wm.active_workspace(monitor), Some(WorkspaceId::FIRST));
        assert_eq!(wm.keyboard_focus(), None);
        assert!(!wm.is_window_visible(window));
        assert_eq!(wm.active_workspace(other), Some(WorkspaceId::FIRST));
        assert!(wm.is_window_visible(other_window));
        assert_eq!(
            wm.execute(WmCommand::SwitchWorkspace {
                monitor,
                workspace: two
            }),
            Ok(CommandEffect::WorkspaceSwitched { monitor })
        );
        assert_eq!(wm.keyboard_focus(), Some(window));
        assert!(wm.is_window_visible(window));
        assert_eq!(
            wm.execute(WmCommand::SwitchWorkspace {
                monitor,
                workspace: two
            }),
            Ok(CommandEffect::Unchanged)
        );
        assert_eq!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor,
                workspace: two
            }),
            Ok(CommandEffect::Unchanged)
        );
    }

    #[test]
    fn move_command_rejects_empty_or_unknown_monitor_without_creating_workspace() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let two = WorkspaceId::new(2).unwrap();
        assert_eq!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor,
                workspace: two
            }),
            Err(CommandError::NoFocusedWindow)
        );
        assert!(wm.add_workspace(monitor, two));
        let missing = MonitorId(u64::MAX);
        assert_eq!(
            wm.execute(WmCommand::SwitchWorkspace {
                monitor: missing,
                workspace: two
            }),
            Err(CommandError::UnknownMonitor)
        );
        assert_eq!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor: missing,
                workspace: two
            }),
            Err(CommandError::UnknownMonitor)
        );
    }

    #[test]
    fn moving_to_occupied_workspace_preserves_its_existing_focus() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let two = WorkspaceId::new(2).unwrap();
        assert!(wm.set_active_workspace(monitor, two));
        let resident = wm
            .add_window(WindowOwner {
                monitor,
                workspace: two,
            })
            .unwrap();
        assert!(wm.set_focus(monitor, Some(resident)));
        assert!(wm.set_active_workspace(monitor, WorkspaceId::FIRST));
        let moved = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        assert!(wm.set_focus(monitor, Some(moved)));
        assert!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor,
                workspace: two
            })
            .is_ok()
        );
        assert_eq!(wm.focused_window(monitor, two), Some(resident));
        assert_eq!(wm.focused_window(monitor, WorkspaceId::FIRST), None);
        assert_eq!(
            wm.owner(moved),
            Some(WindowOwner {
                monitor,
                workspace: two
            })
        );
    }

    #[test]
    fn new_visible_window_focuses_without_stealing_from_another_monitor_or_workspace() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let hidden = WorkspaceId::new(2).unwrap();
        wm.add_workspace(first, hidden);
        let owner = |monitor, workspace| WindowOwner { monitor, workspace };
        let a = wm.add_window(owner(first, WorkspaceId::FIRST)).unwrap();
        assert_eq!(wm.keyboard_focus(), Some(a));
        let b = wm.add_window(owner(second, WorkspaceId::FIRST)).unwrap();
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(wm.focused_window(second, WorkspaceId::FIRST), Some(b));
        let c = wm.add_window(owner(first, hidden)).unwrap();
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(wm.focused_window(first, hidden), None);
        wm.set_active_workspace(first, hidden);
        assert_eq!(wm.keyboard_focus(), None);
        assert_eq!(wm.tiled_windows(first, hidden), Some([c].as_slice()));
    }

    #[test]
    fn focus_wraps_but_swaps_stop_at_edges_and_promotion_preserves_focus() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let c = wm.add_window(owner).unwrap();
        let order = |wm: &WmState| {
            wm.tiled_windows(monitor, WorkspaceId::FIRST)
                .unwrap()
                .to_vec()
        };
        assert_eq!(order(&wm), vec![a, b, c]);
        assert_eq!(wm.keyboard_focus(), Some(c));
        assert_eq!(
            wm.execute(WmCommand::FocusTile {
                monitor,
                direction: Direction::Next
            }),
            Ok(CommandEffect::FocusChanged { monitor })
        );
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(
            wm.execute(WmCommand::SwapTile {
                monitor,
                direction: Direction::Previous
            }),
            Ok(CommandEffect::Unchanged)
        );
        assert_eq!(
            wm.execute(WmCommand::SwapTile {
                monitor,
                direction: Direction::Next
            }),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(order(&wm), vec![b, a, c]);
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(
            wm.execute(WmCommand::PromoteFocusedToMaster { monitor }),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(order(&wm), vec![a, b, c]);
        assert_eq!(
            wm.execute(WmCommand::PromoteFocusedToMaster { monitor }),
            Ok(CommandEffect::Unchanged)
        );
        assert_eq!(
            wm.execute(WmCommand::FocusTile {
                monitor,
                direction: Direction::Previous
            }),
            Ok(CommandEffect::FocusChanged { monitor })
        );
        assert_eq!(wm.keyboard_focus(), Some(c));
        assert_eq!(order(&wm), vec![a, b, c]);
    }

    #[test]
    fn closing_and_moving_focused_tiles_choose_neighbor_without_changing_other_workspace() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let c = wm.add_window(owner).unwrap();
        wm.set_focus(monitor, Some(b));
        wm.remove_window(b);
        assert_eq!(wm.keyboard_focus(), Some(c));
        let two = WorkspaceId::new(2).unwrap();
        assert_eq!(
            wm.execute(WmCommand::MoveFocusedToWorkspace {
                monitor,
                workspace: two
            }),
            Ok(CommandEffect::WindowMoved { monitor, window: c })
        );
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([a].as_slice())
        );
        assert_eq!(wm.tiled_windows(monitor, two), Some([c].as_slice()));
        wm.set_active_workspace(monitor, two);
        assert_eq!(wm.keyboard_focus(), Some(c));
        wm.remove_window(c);
        assert_eq!(wm.keyboard_focus(), None);
        wm.set_active_workspace(monitor, WorkspaceId::FIRST);
        assert_eq!(wm.keyboard_focus(), Some(a));
    }

    #[test]
    fn floating_toggle_move_and_workspace_transfer_preserve_geometry_and_focus() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let rect = Rect {
            x: 12,
            y: 20,
            width: 240,
            height: 180,
        };
        let toggle = WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: rect,
        };
        assert_eq!(
            wm.execute(toggle),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([a].as_slice())
        );
        assert_eq!(
            wm.floating_windows(monitor, WorkspaceId::FIRST),
            Some([b].as_slice())
        );
        assert_eq!(wm.floating_rect(b), Some(rect));
        assert_eq!(wm.keyboard_focus(), Some(b));
        assert_eq!(
            wm.execute(WmCommand::MoveFloating {
                monitor,
                window: b,
                x: 30,
                y: 40
            }),
            Ok(CommandEffect::GeometryChanged { monitor, window: b })
        );
        assert_eq!(
            wm.floating_rect(b),
            Some(Rect {
                x: 30,
                y: 40,
                ..rect
            })
        );
        assert_eq!(
            wm.execute(WmCommand::FocusTile {
                monitor,
                direction: Direction::Next
            }),
            Ok(CommandEffect::FocusChanged { monitor })
        );
        assert_eq!(wm.keyboard_focus(), Some(a));
        wm.set_focus(monitor, Some(b));
        let two = WorkspaceId::new(2).unwrap();
        wm.execute(WmCommand::MoveFocusedToWorkspace {
            monitor,
            workspace: two,
        })
        .unwrap();
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert_eq!(wm.floating_windows(monitor, two), Some([b].as_slice()));
        assert_eq!(
            wm.floating_rect(b),
            Some(Rect {
                x: 30,
                y: 40,
                ..rect
            })
        );
        wm.set_active_workspace(monitor, two);
        assert_eq!(wm.keyboard_focus(), Some(b));
        assert_eq!(
            wm.execute(toggle),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(wm.floating_rect(b), None);
        assert_eq!(wm.tiled_windows(monitor, two), Some([b].as_slice()));
        assert_eq!(
            wm.execute(toggle),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(
            wm.floating_rect(b),
            Some(Rect {
                x: 30,
                y: 40,
                ..rect
            })
        );
        wm.remove_window(b);
        assert_eq!(wm.floating_windows(monitor, two), Some([].as_slice()));
    }

    #[test]
    fn floating_commands_reject_invalid_geometry_and_owners_without_mutating_state() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let other = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let bad = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        };
        assert_eq!(
            wm.execute(WmCommand::ToggleFocusedFloating {
                monitor,
                tiled_rect: bad
            }),
            Err(CommandError::InvalidGeometry)
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([window].as_slice())
        );
        assert_eq!(
            wm.execute(WmCommand::MoveFloating {
                monitor,
                window,
                x: 5,
                y: 5
            }),
            Err(CommandError::NotFloating)
        );
        let rect = Rect {
            x: 1,
            y: 1,
            width: 50,
            height: 50,
        };
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: rect,
        })
        .unwrap();
        assert_eq!(
            wm.execute(WmCommand::MoveFloating {
                monitor: other,
                window,
                x: 2,
                y: 2
            }),
            Err(CommandError::WrongMonitor)
        );
        assert_eq!(
            wm.execute(WmCommand::MoveFloating {
                monitor,
                window,
                x: i32::MAX,
                y: 2
            }),
            Err(CommandError::InvalidGeometry)
        );
        assert_eq!(wm.floating_rect(window), Some(rect));
        wm.remove_window(window);
        assert_eq!(wm.floating_rect(window), None);
        assert_eq!(
            wm.execute(WmCommand::MoveFloating {
                monitor,
                window,
                x: 2,
                y: 2
            }),
            Err(CommandError::UnknownWindow)
        );
    }

    #[test]
    fn closing_a_floating_window_falls_back_to_another_float_then_a_tile() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let tile = wm.add_window(owner).unwrap();
        let rect = Rect {
            x: 2,
            y: 3,
            width: 80,
            height: 60,
        };
        let first = wm.add_window(owner).unwrap();
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: rect,
        })
        .unwrap();
        let second = wm.add_window(owner).unwrap();
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: rect,
        })
        .unwrap();
        wm.remove_window(second);
        assert_eq!(wm.keyboard_focus(), Some(first));
        wm.remove_window(first);
        assert_eq!(wm.keyboard_focus(), Some(tile));
    }

    #[test]
    fn dragging_a_tiled_window_uses_its_current_tile_not_old_floating_position() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let old = Rect {
            x: 10,
            y: 10,
            width: 80,
            height: 60,
        };
        let current_tile = Rect {
            x: 120,
            y: 80,
            width: 300,
            height: 250,
        };
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: old,
        })
        .unwrap();
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: old,
        })
        .unwrap();
        assert_eq!(wm.floating_rect(window), None);
        assert_eq!(
            wm.execute(WmCommand::FloatFocusedAt {
                monitor,
                rect: current_tile
            }),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(wm.floating_rect(window), Some(current_tile));
        assert_eq!(
            wm.execute(WmCommand::FloatFocusedAt { monitor, rect: old }),
            Ok(CommandEffect::Unchanged)
        );
    }
}
