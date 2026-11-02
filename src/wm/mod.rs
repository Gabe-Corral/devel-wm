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
    ToggleTilingMode {
        monitor: MonitorId,
    },
    ToggleFloatingMode {
        monitor: MonitorId,
    },
    ResizeColumnPair {
        monitor: MonitorId,
        left: WindowId,
        right: WindowId,
        left_width: i32,
        right_width: i32,
        delta: i32,
    },
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
    InsertTile {
        monitor: MonitorId,
        window: WindowId,
        target: WindowId,
        after: bool,
    },
    GroupTile {
        monitor: MonitorId,
        window: WindowId,
        target: WindowId,
    },
    GroupFocusedWithNext {
        monitor: MonitorId,
    },
    DetachFocusedTab {
        monitor: MonitorId,
    },
    CycleGroupTab {
        monitor: MonitorId,
    },
    ActivateGroupTab {
        monitor: MonitorId,
        window: WindowId,
    },
    PromoteFocusedToMaster {
        monitor: MonitorId,
    },
    AdjustMasterRatio {
        monitor: MonitorId,
        direction: Direction,
    },
    SetMasterRatio {
        monitor: MonitorId,
        workspace: WorkspaceId,
        ratio: u16,
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
    /// Keep the window floating while assigning it to the destination's
    /// currently visible workspace and transferring keyboard focus.
    TransferFloating {
        window: WindowId,
        destination: MonitorId,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TilingMode {
    #[default]
    MasterStack,
    SideBySide,
    Floating,
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
    WindowTransferred {
        from: MonitorId,
        to: MonitorId,
        window: WindowId,
    },
}

struct WorkspaceState {
    focused_window: Option<WindowId>,
    tiled: Vec<WindowId>,
    floating: Vec<WindowId>,
    master_ratio: MasterRatio,
    tiling_mode: TilingMode,
    previous_tiling_mode: TilingMode,
    workspace_float_rects: BTreeMap<WindowId, Rect>,
    column_weights: BTreeMap<WindowId, u32>,
    groups: BTreeMap<WindowId, WindowGroup>,
}

struct WindowGroup {
    tabs: Vec<WindowId>,
    active: WindowId,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            focused_window: None,
            tiled: Vec::new(),
            floating: Vec::new(),
            master_ratio: MasterRatio::new(600).expect("valid default ratio"),
            tiling_mode: TilingMode::default(),
            previous_tiling_mode: TilingMode::default(),
            workspace_float_rects: BTreeMap::new(),
            column_weights: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }
}

impl WorkspaceState {
    fn tile_root(&self, window: WindowId) -> Option<WindowId> {
        self.tiled.iter().copied().find(|root| {
            *root == window
                || self
                    .groups
                    .get(root)
                    .is_some_and(|group| group.tabs.contains(&window))
        })
    }

    fn selected(&self, root: WindowId) -> WindowId {
        self.groups.get(&root).map_or(root, |group| group.active)
    }

    fn group_tile(&mut self, window: WindowId, target: WindowId) -> Result<bool, CommandError> {
        let source = self.tile_root(window).ok_or(CommandError::UnknownWindow)?;
        let target_root = self.tile_root(target).ok_or(CommandError::UnknownWindow)?;
        if source == target_root {
            return Ok(false);
        }
        self.detach_tab(window).expect("known source tile");
        let root = self.tile_root(target).expect("target tile remains");
        let group = self.groups.entry(root).or_insert_with(|| WindowGroup {
            tabs: vec![root],
            active: root,
        });
        group.tabs.push(window);
        group.active = window;
        self.focused_window = Some(window);
        Ok(true)
    }

    /// Remove a tab without removing its layout slot unless it was the last tab.
    fn detach_tab(&mut self, window: WindowId) -> Option<usize> {
        let root = self.tile_root(window)?;
        let slot = self.tiled.iter().position(|&id| id == root)?;
        if let Some(mut group) = self.groups.remove(&root) {
            let index = group.tabs.iter().position(|&id| id == window)?;
            group.tabs.remove(index);
            if group.active == window {
                group.active = group.tabs[index.min(group.tabs.len() - 1)];
            }
            if group.tabs.len() > 1 {
                let next_root = group.tabs[0];
                self.tiled[slot] = next_root;
                if next_root != root
                    && let Some(rect) = self.workspace_float_rects.remove(&root)
                {
                    self.workspace_float_rects.insert(next_root, rect);
                }
                if next_root != root
                    && let Some(weight) = self.column_weights.remove(&root)
                {
                    self.column_weights.insert(next_root, weight);
                }
                self.groups.insert(next_root, group);
            } else {
                let remaining = group.tabs[0];
                self.tiled[slot] = remaining;
                if remaining != root
                    && let Some(rect) = self.workspace_float_rects.remove(&root)
                {
                    self.workspace_float_rects.insert(remaining, rect);
                }
                if remaining != root
                    && let Some(weight) = self.column_weights.remove(&root)
                {
                    self.column_weights.insert(remaining, weight);
                }
            }
            if self.focused_window == Some(window) {
                self.focused_window = Some(self.selected(self.tiled[slot]));
            }
        } else {
            self.tiled.remove(slot);
            self.column_weights.remove(&root);
            self.workspace_float_rects.remove(&root);
            if self.focused_window == Some(window) {
                self.focused_window = self
                    .tiled
                    .get(slot)
                    .or_else(|| self.tiled.last())
                    .map(|&id| self.selected(id))
                    .or_else(|| self.floating.last().copied());
            }
        }
        Some(slot)
    }

    fn remove_window(&mut self, window: WindowId, floating: bool) {
        if !floating {
            self.detach_tab(window).expect("registered tile");
            return;
        }
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
        self.column_weights.remove(&window);
        if self.focused_window == Some(window) {
            self.focused_window = successor(order, index)
                .or_else(|| other.last().copied().map(|root| self.selected(root)));
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
        self.floating_rects.get(&window).copied().or_else(|| {
            let owner = self.owner(window)?;
            let state = self
                .monitors
                .get(&owner.monitor)?
                .workspaces
                .get(&owner.workspace)?;
            (state.tiling_mode == TilingMode::Floating)
                .then(|| {
                    state
                        .workspace_float_rects
                        .get(&state.tile_root(window)?)
                        .copied()
                })
                .flatten()
        })
    }

    /// Remember a floating-mode placement without changing the tile order or
    /// turning a tile into an individually floating window.
    pub fn ensure_workspace_float_rect(&mut self, window: WindowId, rect: Rect) -> bool {
        if !valid_floating_rect(rect) {
            return false;
        }
        let Some(owner) = self.owner(window) else {
            return false;
        };
        let state = &mut self
            .monitors
            .get_mut(&owner.monitor)
            .expect("owner monitor")
            .workspaces
            .get_mut(&owner.workspace)
            .expect("owner workspace");
        if state.tiling_mode != TilingMode::Floating || !state.tiled.contains(&window) {
            return false;
        }
        state.workspace_float_rects.entry(window).or_insert(rect);
        true
    }

    pub fn clamp_workspace_floats(
        &mut self,
        monitor: MonitorId,
        workspace: WorkspaceId,
        area: Rect,
    ) {
        if let Some(state) = self
            .monitors
            .get_mut(&monitor)
            .and_then(|state| state.workspaces.get_mut(&workspace))
        {
            for rect in state.workspace_float_rects.values_mut() {
                *rect = clamp_floating_to_area(*rect, area);
            }
        }
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

    pub fn tiling_mode(&self, monitor: MonitorId, workspace: WorkspaceId) -> Option<TilingMode> {
        Some(
            self.monitors
                .get(&monitor)?
                .workspaces
                .get(&workspace)?
                .tiling_mode,
        )
    }

    pub fn column_weights(&self, monitor: MonitorId, workspace: WorkspaceId) -> Option<Vec<u32>> {
        let state = self.monitors.get(&monitor)?.workspaces.get(&workspace)?;
        Some(
            state
                .tiled
                .iter()
                .map(|id| state.column_weights.get(id).copied().unwrap_or(1000))
                .collect(),
        )
    }

    pub fn tile_root(&self, window: WindowId) -> Option<WindowId> {
        let owner = self.owner(window)?;
        self.monitors
            .get(&owner.monitor)?
            .workspaces
            .get(&owner.workspace)?
            .tile_root(window)
    }

    pub fn group_tabs(&self, window: WindowId) -> Option<Vec<WindowId>> {
        let root = self.tile_root(window)?;
        let owner = self.owner(window)?;
        let state = self
            .monitors
            .get(&owner.monitor)?
            .workspaces
            .get(&owner.workspace)?;
        Some(
            state
                .groups
                .get(&root)
                .map_or_else(|| vec![root], |group| group.tabs.clone()),
        )
    }

    pub fn selected_tile(&self, root: WindowId) -> Option<WindowId> {
        let owner = self.owner(root)?;
        let state = self
            .monitors
            .get(&owner.monitor)?
            .workspaces
            .get(&owner.workspace)?;
        state.tiled.contains(&root).then(|| state.selected(root))
    }

    /// Select an existing monitor as the keyboard-focus source.
    pub fn set_active_monitor(&mut self, monitor: MonitorId) -> bool {
        if !self.monitors.contains_key(&monitor) {
            return false;
        }
        self.active_monitor = Some(monitor);
        true
    }

    /// On loss of a monitor, keep its workspace definitions but migrate all
    /// windows (including those on inactive workspaces) to the surviving
    /// monitor's visible workspace. With no survivor, leave everything intact.
    pub fn evacuate_monitor(
        &mut self,
        source: MonitorId,
        survivor: Option<(MonitorId, Rect)>,
    ) -> Result<(), CommandError> {
        if !self.monitors.contains_key(&source) {
            return Err(CommandError::UnknownMonitor);
        }
        let Some((target, area)) = survivor else {
            return Ok(());
        };
        if source == target
            || !self.monitors.contains_key(&target)
            || area.width <= 0
            || area.height <= 0
        {
            return Err(CommandError::UnknownMonitor);
        }
        let workspace = self.active_workspace(target).expect("validated monitor");
        let target_focus = self.focused_window(target, workspace);
        let focused = self
            .active_workspace(source)
            .and_then(|ws| self.focused_window(source, ws));
        let groups: Vec<_> = self.monitors[&source]
            .workspaces
            .values()
            .flat_map(|workspace| {
                workspace
                    .groups
                    .values()
                    .map(|group| (group.tabs.clone(), group.active))
            })
            .collect();
        let moving: Vec<_> = self
            .windows
            .iter()
            .filter(|(_, owner)| owner.monitor == source)
            .map(|(&id, &owner)| (id, owner.workspace, self.floating_rects.contains_key(&id)))
            .collect();
        for &(id, from_workspace, floating) in &moving {
            let from = self
                .monitors
                .get_mut(&source)
                .expect("known source")
                .workspaces
                .get_mut(&from_workspace)
                .expect("window workspace");
            let mode_rect = from
                .tile_root(id)
                .and_then(|root| from.workspace_float_rects.get(&root).copied())
                .map(|rect| clamp_floating_to_area(rect, area));
            from.remove_window(id, floating);
            if let Some(rect) = self.floating_rects.get_mut(&id) {
                *rect = clamp_floating_to_area(*rect, area);
            }
            if let Some(rect) = self.saved_floating_rects.get_mut(&id) {
                *rect = clamp_floating_to_area(*rect, area);
            }
            self.windows
                .get_mut(&id)
                .expect("registered window")
                .monitor = target;
            self.windows
                .get_mut(&id)
                .expect("registered window")
                .workspace = workspace;
            let dest = self
                .monitors
                .get_mut(&target)
                .expect("known destination")
                .workspaces
                .get_mut(&workspace)
                .expect("active workspace");
            if floating {
                dest.floating.push(id);
            } else {
                dest.tiled.push(id);
                if let Some(rect) = mode_rect {
                    dest.workspace_float_rects.insert(id, rect);
                }
            }
        }
        {
            let dest = self
                .monitors
                .get_mut(&target)
                .expect("target monitor")
                .workspaces
                .get_mut(&workspace)
                .expect("target workspace");
            for (tabs, selected) in groups {
                let root = tabs[0];
                for &tab in &tabs[1..] {
                    dest.group_tile(tab, root).expect("migrated group members");
                }
                dest.groups.get_mut(&root).expect("recreated group").active = selected;
            }
            dest.focused_window = target_focus;
        }
        if let Some(focused) = focused.filter(|_| self.active_monitor == Some(source)) {
            self.monitors
                .get_mut(&target)
                .expect("known destination")
                .workspaces
                .get_mut(&workspace)
                .expect("active workspace")
                .focused_window = Some(focused);
        }
        if self.active_monitor == Some(source) {
            self.active_monitor = Some(target);
        }
        Ok(())
    }

    /// Preserve floating positions relative to a monitor when its logical
    /// origin shifts after a neighbor is removed or reattached.
    pub fn shift_monitor_floats(&mut self, monitor: MonitorId, dx: i32, dy: i32, area: Rect) {
        if let Some(state) = self.monitors.get_mut(&monitor) {
            for workspace in state.workspaces.values_mut() {
                for rect in workspace.workspace_float_rects.values_mut() {
                    *rect = clamp_floating_to_area(
                        Rect {
                            x: rect.x.saturating_add(dx),
                            y: rect.y.saturating_add(dy),
                            ..*rect
                        },
                        area,
                    );
                }
            }
        }
        for (&id, rect) in &mut self.floating_rects {
            if self
                .windows
                .get(&id)
                .is_some_and(|owner| owner.monitor == monitor)
            {
                let shifted = Rect {
                    x: rect.x.saturating_add(dx),
                    y: rect.y.saturating_add(dy),
                    ..*rect
                };
                *rect = clamp_floating_to_area(shifted, area);
            }
        }
        for (&id, rect) in &mut self.saved_floating_rects {
            if self
                .windows
                .get(&id)
                .is_some_and(|owner| owner.monitor == monitor)
            {
                *rect = clamp_floating_to_area(
                    Rect {
                        x: rect.x.saturating_add(dx),
                        y: rect.y.saturating_add(dy),
                        ..*rect
                    },
                    area,
                );
            }
        }
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
            WmCommand::GroupTile {
                monitor,
                window,
                target,
            } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                if active.group_tile(window, target)? {
                    self.active_monitor = Some(monitor);
                    Ok(CommandEffect::TileOrderChanged { monitor })
                } else {
                    Ok(CommandEffect::Unchanged)
                }
            }
            WmCommand::GroupFocusedWithNext { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                let root = match active.tile_root(window) {
                    Some(root) => root,
                    None => return Ok(CommandEffect::Unchanged),
                };
                if active.tiled.len() < 2 {
                    return Ok(CommandEffect::Unchanged);
                }
                let slot = active
                    .tiled
                    .iter()
                    .position(|&id| id == root)
                    .expect("known root");
                let target = active.tiled[(slot + 1) % active.tiled.len()];
                if active.group_tile(window, target)? {
                    Ok(CommandEffect::TileOrderChanged { monitor })
                } else {
                    Ok(CommandEffect::Unchanged)
                }
            }
            WmCommand::DetachFocusedTab { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let window = active.focused_window.ok_or(CommandError::NoFocusedWindow)?;
                let root = match active.tile_root(window) {
                    Some(root) => root,
                    None => return Ok(CommandEffect::Unchanged),
                };
                if !active.groups.contains_key(&root) {
                    return Ok(CommandEffect::Unchanged);
                }
                let slot = active.detach_tab(window).expect("known tab");
                active.tiled.insert(slot + 1, window);
                active.focused_window = Some(window);
                Ok(CommandEffect::TileOrderChanged { monitor })
            }
            WmCommand::CycleGroupTab { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let Some(focused) = active.focused_window else {
                    return Ok(CommandEffect::Unchanged);
                };
                let root = match active.tile_root(focused) {
                    Some(root) => root,
                    None => return Ok(CommandEffect::Unchanged),
                };
                let Some(group) = active.groups.get_mut(&root) else {
                    return Ok(CommandEffect::Unchanged);
                };
                let index = group
                    .tabs
                    .iter()
                    .position(|&id| id == group.active)
                    .expect("selected tab");
                group.active = group.tabs[(index + 1) % group.tabs.len()];
                active.focused_window = Some(group.active);
                Ok(CommandEffect::FocusChanged { monitor })
            }
            WmCommand::ActivateGroupTab { monitor, window } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let root = active
                    .tile_root(window)
                    .ok_or(CommandError::UnknownWindow)?;
                if let Some(group) = active.groups.get_mut(&root) {
                    group.active = window;
                }
                if active.focused_window == Some(window) && self.active_monitor == Some(monitor) {
                    return Ok(CommandEffect::Unchanged);
                }
                active.focused_window = Some(window);
                self.active_monitor = Some(monitor);
                Ok(CommandEffect::FocusChanged { monitor })
            }
            WmCommand::ToggleTilingMode { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                active.tiling_mode = match active.tiling_mode {
                    TilingMode::MasterStack => TilingMode::SideBySide,
                    TilingMode::SideBySide => TilingMode::MasterStack,
                    TilingMode::Floating => match active.previous_tiling_mode {
                        TilingMode::MasterStack => TilingMode::SideBySide,
                        _ => TilingMode::MasterStack,
                    },
                };
                active.previous_tiling_mode = active.tiling_mode;
                Ok(CommandEffect::LayoutChanged { monitor })
            }
            WmCommand::ToggleFloatingMode { monitor } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                if active.tiling_mode == TilingMode::Floating {
                    active.tiling_mode = active.previous_tiling_mode;
                } else {
                    active.previous_tiling_mode = active.tiling_mode;
                    active.tiling_mode = TilingMode::Floating;
                }
                Ok(CommandEffect::LayoutChanged { monitor })
            }
            WmCommand::ResizeColumnPair {
                monitor,
                left,
                right,
                left_width,
                right_width,
                delta,
            } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                if active.tiling_mode != TilingMode::SideBySide {
                    return Ok(CommandEffect::Unchanged);
                }
                if !active.tiled.windows(2).any(|pair| pair == [left, right]) {
                    return Err(CommandError::UnknownWindow);
                }
                if left_width <= 0 || right_width <= 0 {
                    return Err(CommandError::InvalidGeometry);
                }
                let pair_width = i64::from(left_width) + i64::from(right_width);
                let minimum = 80_i64.min(pair_width / 2);
                let width =
                    (i64::from(left_width) + i64::from(delta)).clamp(minimum, pair_width - minimum);
                let old_left = active.column_weights.get(&left).copied().unwrap_or(1000);
                let old_right = active.column_weights.get(&right).copied().unwrap_or(1000);
                let sum = u64::from(old_left) + u64::from(old_right);
                let left_weight = ((width as u64 * sum + pair_width as u64 / 2) / pair_width as u64)
                    .clamp(1, sum - 1) as u32;
                let right_weight = (sum - u64::from(left_weight)) as u32;
                if left_weight == old_left && right_weight == old_right {
                    return Ok(CommandEffect::Unchanged);
                }
                active.column_weights.insert(left, left_weight);
                active.column_weights.insert(right, right_weight);
                Ok(CommandEffect::LayoutChanged { monitor })
            }
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
                let mode_rect = source_state
                    .tile_root(window)
                    .and_then(|root| source_state.workspace_float_rects.get(&root).copied());
                source_state.remove_window(window, floating);
                let target = monitor_state.workspaces.entry(workspace).or_default();
                target.focused_window.get_or_insert(window);
                if floating {
                    target.floating.push(window);
                } else {
                    target.tiled.push(window);
                    if let Some(rect) = mode_rect {
                        target.workspace_float_rects.insert(window, rect);
                    }
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
                    let id = active.tile_root(id).unwrap_or(id);
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
                let window = active
                    .tile_root(window)
                    .map_or(window, |root| active.selected(root));
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
                let index = active
                    .tiled
                    .iter()
                    .position(|&id| id == active.tile_root(window).unwrap_or(window));
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
            WmCommand::InsertTile {
                monitor,
                window,
                target,
                after,
            } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                let active = state
                    .workspaces
                    .get_mut(&state.active_workspace)
                    .expect("active workspace");
                let source_root = active
                    .tile_root(window)
                    .ok_or(CommandError::UnknownWindow)?;
                let target = active
                    .tile_root(target)
                    .ok_or(CommandError::UnknownWindow)?;
                // A tab dropped onto its own group's content becomes a new
                // adjacent tile. Remember a sibling: detaching the root tab
                // can change the group's root identity.
                let target_tab = if source_root == target {
                    active
                        .groups
                        .get(&target)
                        .and_then(|group| group.tabs.iter().copied().find(|&tab| tab != window))
                } else {
                    Some(target)
                };
                let source = active
                    .tiled
                    .iter()
                    .position(|&id| id == source_root)
                    .ok_or(CommandError::UnknownWindow)?;
                let destination = active
                    .tiled
                    .iter()
                    .position(|&id| id == target)
                    .ok_or(CommandError::UnknownWindow)?;
                if source == destination && target_tab.is_none() {
                    return Ok(CommandEffect::Unchanged);
                }
                let insertion = destination + usize::from(after);
                if source_root == window
                    && !active.groups.contains_key(&source_root)
                    && source == insertion - usize::from(source < insertion)
                {
                    return Ok(CommandEffect::Unchanged);
                }
                let weight = (source_root == window && !active.groups.contains_key(&source_root))
                    .then(|| active.column_weights.get(&window).copied())
                    .flatten();
                active.detach_tab(window).expect("known source tab");
                let destination = active
                    .tiled
                    .iter()
                    .position(|&id| {
                        id == active
                            .tile_root(target_tab.expect("sibling remains"))
                            .expect("target remains")
                    })
                    .expect("target tile remains");
                active
                    .tiled
                    .insert(destination + usize::from(after), window);
                if let Some(weight) = weight {
                    active.column_weights.insert(window, weight);
                }
                active.focused_window = Some(window);
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
                let index = active
                    .tiled
                    .iter()
                    .position(|&id| id == active.tile_root(window).unwrap_or(window));
                let Some(index) = index else {
                    return Ok(CommandEffect::Unchanged);
                };
                if index == 0 {
                    return Ok(CommandEffect::Unchanged);
                }
                let root = active.tiled.remove(index);
                active.tiled.insert(0, root);
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
                if active.tiled.len() < 2 || active.tiling_mode != TilingMode::MasterStack {
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
            WmCommand::SetMasterRatio {
                monitor,
                workspace,
                ratio,
            } => {
                let state = self
                    .monitors
                    .get_mut(&monitor)
                    .ok_or(CommandError::UnknownMonitor)?;
                if state.active_workspace != workspace {
                    return Ok(CommandEffect::Unchanged);
                }
                let active = state
                    .workspaces
                    .get_mut(&workspace)
                    .expect("active workspace");
                if active.tiling_mode != TilingMode::MasterStack || active.tiled.len() < 2 {
                    return Ok(CommandEffect::Unchanged);
                }
                let ratio = ratio.clamp(MASTER_RATIO_MIN as u16, MASTER_RATIO_MAX as u16);
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
                    if active.tiling_mode == TilingMode::Floating {
                        active.workspace_float_rects.insert(window, rect);
                    }
                    self.saved_floating_rects.insert(window, rect);
                } else {
                    let root = active
                        .tile_root(window)
                        .ok_or(CommandError::UnknownWindow)?;
                    let rect = active
                        .workspace_float_rects
                        .get(&root)
                        .copied()
                        .or_else(|| self.saved_floating_rects.get(&window).copied())
                        .unwrap_or(tiled_rect);
                    if !valid_floating_rect(rect) {
                        return Err(CommandError::InvalidGeometry);
                    }
                    active
                        .detach_tab(window)
                        .ok_or(CommandError::UnknownWindow)?;
                    if !active.tiled.contains(&root) {
                        active.workspace_float_rects.remove(&root);
                    }
                    active.floating.push(window);
                    active.focused_window = Some(window);
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
                active
                    .detach_tab(window)
                    .ok_or(CommandError::UnknownWindow)?;
                active.workspace_float_rects.remove(&window);
                active.floating.push(window);
                active.focused_window = Some(window);
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
                let state = self
                    .monitors
                    .get_mut(&owner.monitor)
                    .expect("known monitor")
                    .workspaces
                    .get_mut(&owner.workspace)
                    .expect("known workspace");
                let root = state.tile_root(window);
                let rect = self
                    .floating_rects
                    .get_mut(&window)
                    .or_else(|| {
                        (state.tiling_mode == TilingMode::Floating)
                            .then(|| state.workspace_float_rects.get_mut(&root?))
                            .flatten()
                    })
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
            WmCommand::TransferFloating {
                window,
                destination,
                x,
                y,
            } => {
                let owner = *self
                    .windows
                    .get(&window)
                    .ok_or(CommandError::UnknownWindow)?;
                let target_workspace = self
                    .active_workspace(destination)
                    .ok_or(CommandError::UnknownMonitor)?;
                let rect = self
                    .floating_rect(window)
                    .ok_or(CommandError::NotFloating)?;
                if x.checked_add(rect.width).is_none() || y.checked_add(rect.height).is_none() {
                    return Err(CommandError::InvalidGeometry);
                }
                if owner.monitor == destination {
                    return self.execute(WmCommand::MoveFloating {
                        monitor: destination,
                        window,
                        x,
                        y,
                    });
                }
                let source = self
                    .monitors
                    .get_mut(&owner.monitor)
                    .expect("window's monitor must exist")
                    .workspaces
                    .get_mut(&owner.workspace)
                    .expect("window's workspace must exist");
                source.remove_window(window, self.floating_rects.contains_key(&window));
                let target = self
                    .monitors
                    .get_mut(&destination)
                    .expect("validated destination")
                    .workspaces
                    .get_mut(&target_workspace)
                    .expect("active workspace must exist");
                target.floating.push(window);
                target.focused_window = Some(window);
                self.active_monitor = Some(destination);
                *self.windows.get_mut(&window).expect("known window") = WindowOwner {
                    monitor: destination,
                    workspace: target_workspace,
                };
                self.floating_rects.insert(window, Rect { x, y, ..rect });
                Ok(CommandEffect::WindowTransferred {
                    from: owner.monitor,
                    to: destination,
                    window,
                })
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
                let state = self
                    .monitors
                    .get_mut(&owner.monitor)
                    .expect("known monitor")
                    .workspaces
                    .get_mut(&owner.workspace)
                    .expect("known workspace");
                let root = state.tile_root(window);
                let current = self
                    .floating_rects
                    .get_mut(&window)
                    .or_else(|| {
                        (state.tiling_mode == TilingMode::Floating)
                            .then(|| state.workspace_float_rects.get_mut(&root?))
                            .flatten()
                    })
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
        let active = state
            .workspaces
            .get_mut(&state.active_workspace)
            .expect("active workspace must exist");
        if let Some(window) = window
            && let Some(root) = active.tile_root(window)
            && let Some(group) = active.groups.get_mut(&root)
        {
            group.active = window;
        }
        active.focused_window = window;
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

    /// Only the selected tab of a group is rendered in its layout slot.
    pub fn is_window_visible(&self, window: WindowId) -> bool {
        let Some(owner) = self.owner(window) else {
            return false;
        };
        if self.active_workspace(owner.monitor) != Some(owner.workspace) {
            return false;
        }
        let Some(state) = self
            .monitors
            .get(&owner.monitor)
            .and_then(|monitor| monitor.workspaces.get(&owner.workspace))
        else {
            return false;
        };
        state
            .tile_root(window)
            .is_none_or(|root| state.selected(root) == window)
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
        if floating {
            workspace.workspace_float_rects.remove(&window);
        }
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

fn clamp_floating_to_area(rect: Rect, area: Rect) -> Rect {
    let axis = |start: i32, size: i32, edge: i32, extent: i32| {
        let visible = 32.min(size).min(extent);
        let min = i64::from(edge) + i64::from(visible) - i64::from(size);
        let max = i64::from(edge) + i64::from(extent) - i64::from(visible);
        i64::from(start)
            .clamp(min, max)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX) - i64::from(size)) as i32
    };
    Rect {
        x: axis(rect.x, rect.width, area.x, area.width),
        y: axis(rect.y, rect.height, area.y, area.height),
        ..rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_mode_preserves_tiles_explicit_floats_and_workspace_modes() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let other = wm.add_monitor();
        let ws = WorkspaceId::FIRST;
        let windows: Vec<_> = (0..3)
            .map(|_| {
                wm.add_window(WindowOwner {
                    monitor,
                    workspace: ws,
                })
                .unwrap()
            })
            .collect();
        wm.set_focus(monitor, Some(windows[2]));
        let permanent = Rect {
            x: 90,
            y: 80,
            width: 420,
            height: 320,
        };
        wm.execute(WmCommand::FloatFocusedAt {
            monitor,
            rect: permanent,
        })
        .unwrap();
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        assert_eq!(wm.tiling_mode(monitor, ws), Some(TilingMode::Floating));
        assert_eq!(wm.tiling_mode(other, ws), Some(TilingMode::MasterStack));
        let temporary = Rect {
            x: 200,
            y: 50,
            width: 900,
            height: 640,
        };
        assert!(wm.ensure_workspace_float_rect(windows[0], temporary));
        assert_eq!(wm.floating_rect(windows[0]), Some(temporary));
        wm.execute(WmCommand::MoveFloating {
            monitor,
            window: windows[0],
            x: 240,
            y: 70,
        })
        .unwrap();
        let changed = Rect {
            x: 240,
            y: 70,
            width: 800,
            height: 600,
        };
        wm.execute(WmCommand::ResizeFloating {
            monitor,
            window: windows[0],
            rect: changed,
        })
        .unwrap();
        assert_eq!(wm.floating_rect(windows[0]), Some(changed));
        assert_eq!(wm.floating_rect(windows[2]), Some(permanent));
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        assert_eq!(wm.tiling_mode(monitor, ws), Some(TilingMode::MasterStack));
        assert_eq!(wm.floating_rect(windows[0]), None);
        assert_eq!(wm.floating_rect(windows[2]), Some(permanent));
        assert_eq!(wm.tiled_windows(monitor, ws), Some(&windows[..2]));
        assert_eq!(wm.keyboard_focus(), Some(windows[2]));
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        assert_eq!(wm.floating_rect(windows[0]), Some(changed));
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        assert_eq!(wm.tiling_mode(monitor, ws), Some(TilingMode::SideBySide));
    }

    #[test]
    fn floating_mode_moved_window_keeps_reachable_placement_and_cleanup() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let ws = WorkspaceId::FIRST;
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: ws,
            })
            .unwrap();
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        let rect = Rect {
            x: 2000,
            y: 30,
            width: 400,
            height: 300,
        };
        assert!(wm.ensure_workspace_float_rect(window, rect));
        wm.clamp_workspace_floats(
            monitor,
            ws,
            Rect {
                x: 0,
                y: 0,
                width: 1000,
                height: 800,
            },
        );
        assert!(wm.floating_rect(window).unwrap().x < 1000);
        let next = WorkspaceId::new(2).unwrap();
        wm.execute(WmCommand::MoveFocusedToWorkspace {
            monitor,
            workspace: next,
        })
        .unwrap();
        assert_eq!(wm.floating_rect(window), None);
        wm.execute(WmCommand::SwitchWorkspace {
            monitor,
            workspace: next,
        })
        .unwrap();
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        assert!(wm.floating_rect(window).is_some());
        wm.remove_window(window);
        assert_eq!(wm.floating_rect(window), None);
    }

    #[test]
    fn floating_mode_restores_side_by_side_and_explicit_float_does_not_retile() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let ws = WorkspaceId::FIRST;
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: ws,
            })
            .unwrap();
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        let rect = Rect {
            x: 40,
            y: 60,
            width: 550,
            height: 410,
        };
        wm.ensure_workspace_float_rect(window, rect);
        wm.execute(WmCommand::ToggleFocusedFloating {
            monitor,
            tiled_rect: Rect {
                x: 0,
                y: 0,
                width: 1200,
                height: 900,
            },
        })
        .unwrap();
        assert_eq!(wm.floating_rect(window), Some(rect));
        wm.execute(WmCommand::ToggleFloatingMode { monitor })
            .unwrap();
        assert_eq!(wm.tiling_mode(monitor, ws), Some(TilingMode::SideBySide));
        assert_eq!(wm.floating_windows(monitor, ws), Some(&[window][..]));
        assert_eq!(wm.floating_rect(window), Some(rect));
    }

    #[test]
    fn disconnect_migrates_every_workspace_to_survivor_and_preserves_floats() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let hidden = WorkspaceId::new(2).unwrap();
        let survivor_workspace = WorkspaceId::new(3).unwrap();
        wm.set_active_workspace(second, survivor_workspace);
        let resident = wm
            .add_window(WindowOwner {
                monitor: second,
                workspace: survivor_workspace,
            })
            .unwrap();
        let focused = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let float_rect = Rect {
            x: -300,
            y: 1200,
            width: 280,
            height: 240,
        };
        wm.execute(WmCommand::FloatFocusedAt {
            monitor: first,
            rect: float_rect,
        })
        .unwrap();
        wm.add_workspace(first, hidden);
        let hidden_window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: hidden,
            })
            .unwrap();
        let area = Rect {
            x: 3840,
            y: 0,
            width: 1920,
            height: 1080,
        };
        wm.evacuate_monitor(first, Some((second, area))).unwrap();
        assert_eq!(wm.active_monitor(), Some(second));
        assert_eq!(wm.keyboard_focus(), Some(focused));
        assert_eq!(
            wm.owner(focused),
            Some(WindowOwner {
                monitor: second,
                workspace: survivor_workspace
            })
        );
        assert_eq!(wm.owner(hidden_window), wm.owner(focused));
        assert_eq!(
            wm.floating_rect(focused),
            Some(Rect {
                x: 3592,
                y: 1048,
                ..float_rect
            })
        );
        assert_eq!(
            wm.floating_windows(second, survivor_workspace),
            Some(&[focused][..])
        );
        assert_eq!(
            wm.tiled_windows(second, survivor_workspace),
            Some(&[resident, hidden_window][..])
        );
        assert_eq!(wm.tiled_windows(first, hidden), Some(&[][..]));
        assert_eq!(wm.windows().count(), 3);
    }

    #[test]
    fn all_outputs_gone_keeps_windows_and_focus_for_reconnection() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        wm.evacuate_monitor(monitor, None).unwrap();
        assert_eq!(
            wm.owner(window),
            Some(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST
            })
        );
        assert_eq!(wm.keyboard_focus(), Some(window));
        assert_eq!(wm.windows().count(), 1);
    }

    #[test]
    fn disconnecting_inactive_monitor_does_not_steal_survivor_focus() {
        let mut wm = WmState::new();
        let survivor = wm.add_monitor();
        let disconnected = wm.add_monitor();
        let resident = wm
            .add_window(WindowOwner {
                monitor: survivor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let migrant = wm
            .add_window(WindowOwner {
                monitor: disconnected,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        wm.evacuate_monitor(
            disconnected,
            Some((
                survivor,
                Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            )),
        )
        .unwrap();
        assert_eq!(wm.keyboard_focus(), Some(resident));
        assert_eq!(wm.owner(migrant).unwrap().monitor, survivor);
    }

    #[test]
    fn surviving_monitor_reflows_floating_windows_when_origin_moves() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        wm.execute(WmCommand::FloatFocusedAt {
            monitor,
            rect: Rect {
                x: 4000,
                y: 70,
                width: 300,
                height: 200,
            },
        })
        .unwrap();
        wm.shift_monitor_floats(
            monitor,
            -3840,
            0,
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(
            wm.floating_rect(window),
            Some(Rect {
                x: 160,
                y: 70,
                width: 300,
                height: 200
            })
        );
        wm.shift_monitor_floats(
            monitor,
            3840,
            0,
            Rect {
                x: 3840,
                y: 0,
                width: 1920,
                height: 1080,
            },
        );
        assert_eq!(wm.floating_rect(window).unwrap().x, 4000);
    }

    #[test]
    fn floating_transfer_updates_ownership_and_focus_without_duplicate_windows() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let destination_workspace = WorkspaceId::new(3).unwrap();
        wm.set_active_workspace(second, destination_workspace);
        let survivor = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let moving = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let resident = wm
            .add_window(WindowOwner {
                monitor: second,
                workspace: destination_workspace,
            })
            .unwrap();
        let rect = Rect {
            x: 400,
            y: 40,
            width: 300,
            height: 200,
        };
        wm.execute(WmCommand::FloatFocusedAt {
            monitor: first,
            rect,
        })
        .unwrap();
        let transfer = WmCommand::TransferFloating {
            window: moving,
            destination: second,
            x: 900,
            y: 60,
        };
        assert_eq!(
            wm.execute(transfer),
            Ok(CommandEffect::WindowTransferred {
                from: first,
                to: second,
                window: moving
            })
        );
        assert_eq!(
            wm.owner(moving),
            Some(WindowOwner {
                monitor: second,
                workspace: destination_workspace
            })
        );
        assert_eq!(
            wm.floating_rect(moving),
            Some(Rect {
                x: 900,
                y: 60,
                ..rect
            })
        );
        assert_eq!(wm.focused_window(first, WorkspaceId::FIRST), Some(survivor));
        assert_eq!(wm.keyboard_focus(), Some(moving));
        assert_eq!(
            wm.floating_windows(second, destination_workspace),
            Some(&[moving][..])
        );
        assert_eq!(
            wm.tiled_windows(second, destination_workspace),
            Some(&[resident][..])
        );
        assert_eq!(wm.windows().count(), 3);
        assert_eq!(
            wm.execute(WmCommand::TransferFloating {
                window: moving,
                destination: first,
                x: 100,
                y: 40
            }),
            Ok(CommandEffect::WindowTransferred {
                from: second,
                to: first,
                window: moving
            })
        );
        assert_eq!(
            wm.focused_window(second, destination_workspace),
            Some(resident)
        );
        assert_eq!(wm.keyboard_focus(), Some(moving));
        assert_eq!(
            wm.floating_windows(second, destination_workspace),
            Some(&[][..])
        );
        assert_eq!(
            wm.floating_windows(first, WorkspaceId::FIRST),
            Some(&[moving][..])
        );
    }

    #[test]
    fn floating_transfer_rejects_invalid_targets_without_mutating_owners() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        let transfer = |destination, x| WmCommand::TransferFloating {
            window,
            destination,
            x,
            y: 0,
        };
        assert_eq!(
            wm.execute(transfer(second, 0)),
            Err(CommandError::NotFloating)
        );
        let rect = Rect {
            x: 0,
            y: 0,
            width: 300,
            height: 200,
        };
        wm.execute(WmCommand::FloatFocusedAt {
            monitor: first,
            rect,
        })
        .unwrap();
        assert_eq!(
            wm.execute(transfer(MonitorId(u64::MAX), 0)),
            Err(CommandError::UnknownMonitor)
        );
        assert_eq!(
            wm.execute(transfer(second, i32::MAX)),
            Err(CommandError::InvalidGeometry)
        );
        assert_eq!(
            wm.owner(window),
            Some(WindowOwner {
                monitor: first,
                workspace: WorkspaceId::FIRST
            })
        );
        assert_eq!(wm.floating_rect(window), Some(rect));
        assert_eq!(
            wm.floating_windows(second, WorkspaceId::FIRST),
            Some(&[][..])
        );
    }

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
    fn tiling_mode_is_workspace_local_and_preserves_order_focus_and_master_ratio() {
        let mut wm = WmState::new();
        let first = wm.add_monitor();
        let second = wm.add_monitor();
        let other = WorkspaceId::new(2).unwrap();
        let windows: Vec<_> = (0..3)
            .map(|_| {
                wm.add_window(WindowOwner {
                    monitor: first,
                    workspace: WorkspaceId::FIRST,
                })
                .unwrap()
            })
            .collect();
        wm.add_workspace(first, other);
        let ratio = WmCommand::AdjustMasterRatio {
            monitor: first,
            direction: Direction::Next,
        };
        wm.execute(ratio).unwrap();
        let before = wm.master_ratio(first, WorkspaceId::FIRST).unwrap();
        let focused = wm.keyboard_focus();
        let toggle = WmCommand::ToggleTilingMode { monitor: first };
        assert_eq!(
            wm.tiling_mode(first, WorkspaceId::FIRST),
            Some(TilingMode::MasterStack)
        );
        assert_eq!(
            wm.execute(toggle),
            Ok(CommandEffect::LayoutChanged { monitor: first })
        );
        assert_eq!(
            wm.tiling_mode(first, WorkspaceId::FIRST),
            Some(TilingMode::SideBySide)
        );
        assert_eq!(wm.execute(ratio), Ok(CommandEffect::Unchanged));
        assert_eq!(wm.master_ratio(first, WorkspaceId::FIRST), Some(before));
        assert_eq!(
            wm.tiled_windows(first, WorkspaceId::FIRST),
            Some(windows.as_slice())
        );
        assert_eq!(wm.keyboard_focus(), focused);
        assert_eq!(
            wm.tiling_mode(second, WorkspaceId::FIRST),
            Some(TilingMode::MasterStack)
        );
        wm.execute(WmCommand::SwitchWorkspace {
            monitor: first,
            workspace: other,
        })
        .unwrap();
        assert_eq!(wm.tiling_mode(first, other), Some(TilingMode::MasterStack));
        wm.execute(WmCommand::SwitchWorkspace {
            monitor: first,
            workspace: WorkspaceId::FIRST,
        })
        .unwrap();
        assert_eq!(
            wm.tiling_mode(first, WorkspaceId::FIRST),
            Some(TilingMode::SideBySide)
        );
        wm.execute(toggle).unwrap();
        assert_eq!(
            wm.tiling_mode(first, WorkspaceId::FIRST),
            Some(TilingMode::MasterStack)
        );
        assert_eq!(wm.master_ratio(first, WorkspaceId::FIRST), Some(before));
        assert_eq!(
            wm.execute(WmCommand::ToggleTilingMode {
                monitor: MonitorId(u64::MAX)
            }),
            Err(CommandError::UnknownMonitor)
        );
    }

    #[test]
    fn tiling_mode_toggle_does_not_move_floating_windows() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let window = wm
            .add_window(WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        wm.set_focus(monitor, Some(window));
        let rect = Rect {
            x: 40,
            y: 50,
            width: 300,
            height: 200,
        };
        wm.execute(WmCommand::FloatFocusedAt { monitor, rect })
            .unwrap();
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        assert_eq!(wm.floating_rect(window), Some(rect));
        assert_eq!(
            wm.floating_windows(monitor, WorkspaceId::FIRST),
            Some(&[window][..])
        );
        assert_eq!(wm.tiled_windows(monitor, WorkspaceId::FIRST), Some(&[][..]));
        assert_eq!(wm.keyboard_focus(), Some(window));
    }

    #[test]
    fn resizing_columns_changes_only_adjacent_weights_and_clamps_minimums() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let windows: Vec<_> = (0..3)
            .map(|_| {
                wm.add_window(WindowOwner {
                    monitor,
                    workspace: WorkspaceId::FIRST,
                })
                .unwrap()
            })
            .collect();
        let command = |delta| WmCommand::ResizeColumnPair {
            monitor,
            left: windows[0],
            right: windows[1],
            left_width: 300,
            right_width: 300,
            delta,
        };
        assert_eq!(wm.execute(command(100)).unwrap(), CommandEffect::Unchanged);
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        assert_eq!(
            wm.execute(command(100)),
            Ok(CommandEffect::LayoutChanged { monitor })
        );
        let weights = wm.column_weights(monitor, WorkspaceId::FIRST).unwrap();
        assert!(weights[0] > 1000 && weights[1] < 1000);
        assert_eq!(weights[0] + weights[1], 2000);
        assert_eq!(weights[2], 1000);
        wm.execute(command(i32::MAX)).unwrap();
        let weights = wm.column_weights(monitor, WorkspaceId::FIRST).unwrap();
        assert!(weights[0] < 2000 && weights[1] > 0);
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        assert_eq!(
            wm.column_weights(monitor, WorkspaceId::FIRST),
            Some(weights)
        );
        assert_eq!(
            wm.execute(WmCommand::ResizeColumnPair {
                monitor,
                left: windows[0],
                right: windows[2],
                left_width: 300,
                right_width: 300,
                delta: 10,
            }),
            Err(CommandError::UnknownWindow)
        );
        assert_eq!(
            wm.execute(WmCommand::ResizeColumnPair {
                monitor,
                left: windows[0],
                right: windows[1],
                left_width: 0,
                right_width: 300,
                delta: 10,
            }),
            Err(CommandError::InvalidGeometry)
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
    fn inserting_a_tile_shifts_neighbors_without_changing_focus_or_weights() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let ids: Vec<_> = (0..4).map(|_| wm.add_window(owner).unwrap()).collect();
        let [a, b, c, d] = [ids[0], ids[1], ids[2], ids[3]];
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        wm.execute(WmCommand::ResizeColumnPair {
            monitor,
            left: a,
            right: b,
            left_width: 200,
            right_width: 200,
            delta: 20,
        })
        .unwrap();
        let weights = wm.column_weights(monitor, WorkspaceId::FIRST);
        let insert = |window, target, after| WmCommand::InsertTile {
            monitor,
            window,
            target,
            after,
        };
        assert_eq!(
            wm.execute(insert(d, b, false)),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([a, d, b, c].as_slice())
        );
        assert_eq!(wm.keyboard_focus(), Some(d));
        assert_eq!(wm.execute(insert(d, a, true)), Ok(CommandEffect::Unchanged));
        assert_eq!(
            wm.execute(insert(a, d, false)),
            Ok(CommandEffect::Unchanged)
        );
        assert_eq!(
            wm.execute(insert(a, c, true)),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([d, b, c, a].as_slice())
        );
        assert_eq!(
            wm.column_weights(monitor, WorkspaceId::FIRST),
            Some(vec![1000, weights.unwrap()[1], 1000, 1100])
        );
        assert_eq!(
            wm.execute(insert(a, a, false)),
            Ok(CommandEffect::Unchanged)
        );
        assert_eq!(
            wm.execute(insert(a, WindowId(u64::MAX), false)),
            Err(CommandError::UnknownWindow)
        );
        let other = wm.add_monitor();
        let other_tile = wm
            .add_window(WindowOwner {
                monitor: other,
                workspace: WorkspaceId::FIRST,
            })
            .unwrap();
        assert_eq!(
            wm.execute(insert(a, other_tile, false)),
            Err(CommandError::UnknownWindow)
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([d, b, c, a].as_slice())
        );
    }

    #[test]
    fn grouping_cycles_detaches_and_preserves_one_layout_slot() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let c = wm.add_window(owner).unwrap();
        assert_eq!(
            wm.execute(WmCommand::GroupTile {
                monitor,
                window: a,
                target: b
            }),
            Ok(CommandEffect::TileOrderChanged { monitor })
        );
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([b, c].as_slice())
        );
        assert_eq!(wm.group_tabs(b), Some(vec![b, a]));
        assert_eq!(wm.keyboard_focus(), Some(a));
        assert!(!wm.is_window_visible(b));
        assert!(wm.is_window_visible(a));
        wm.execute(WmCommand::CycleGroupTab { monitor }).unwrap();
        assert_eq!(wm.keyboard_focus(), Some(b));
        assert!(wm.is_window_visible(b));
        assert!(!wm.is_window_visible(a));
        wm.execute(WmCommand::GroupTile {
            monitor,
            window: c,
            target: a,
        })
        .unwrap();
        assert_eq!(wm.group_tabs(b), Some(vec![b, a, c]));
        wm.execute(WmCommand::DetachFocusedTab { monitor }).unwrap();
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([b, c].as_slice())
        );
        assert_eq!(wm.group_tabs(b), Some(vec![b, a]));
        wm.remove_window(b);
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([a, c].as_slice())
        );
        assert_eq!(wm.group_tabs(a), Some(vec![a]));
    }

    #[test]
    fn grouped_tabs_support_keyboard_navigation_floating_and_collapse() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let c = wm.add_window(owner).unwrap();
        assert_eq!(
            wm.execute(WmCommand::CycleGroupTab { monitor }),
            Ok(CommandEffect::Unchanged)
        );
        wm.set_focus(monitor, Some(a));
        wm.execute(WmCommand::GroupFocusedWithNext { monitor })
            .unwrap();
        assert_eq!(wm.group_tabs(b), Some(vec![b, a]));
        wm.execute(WmCommand::GroupTile {
            monitor,
            window: c,
            target: b,
        })
        .unwrap();
        assert_eq!(wm.group_tabs(b), Some(vec![b, a, c]));
        wm.execute(WmCommand::CycleGroupTab { monitor }).unwrap();
        assert_eq!(wm.keyboard_focus(), Some(b));
        wm.execute(WmCommand::FocusTile {
            monitor,
            direction: Direction::Next,
        })
        .unwrap();
        assert_eq!(wm.keyboard_focus(), Some(b)); // One group is one tile.
        wm.execute(WmCommand::FloatFocusedAt {
            monitor,
            rect: Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            },
        })
        .unwrap();
        assert_eq!(wm.group_tabs(a), Some(vec![a, c]));
        assert!(wm.floating_rect(b).is_some());
        wm.set_focus(monitor, Some(a));
        wm.execute(WmCommand::DetachFocusedTab { monitor }).unwrap();
        assert_eq!(wm.group_tabs(c), Some(vec![c]));
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([c, a].as_slice())
        );
        wm.remove_window(c);
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([a].as_slice())
        );
    }

    #[test]
    fn output_migration_preserves_group_tabs_and_selection() {
        let mut wm = WmState::new();
        let source = wm.add_monitor();
        let target = wm.add_monitor();
        let owner = WindowOwner {
            monitor: source,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        wm.execute(WmCommand::GroupTile {
            monitor: source,
            window: b,
            target: a,
        })
        .unwrap();
        wm.evacuate_monitor(
            source,
            Some((
                target,
                Rect {
                    x: 0,
                    y: 0,
                    width: 500,
                    height: 500,
                },
            )),
        )
        .unwrap();
        assert_eq!(wm.group_tabs(a), Some(vec![a, b]));
        assert_eq!(wm.selected_tile(a), Some(b));
        assert_eq!(
            wm.tiled_windows(target, WorkspaceId::FIRST),
            Some([a].as_slice())
        );
        assert!(wm.is_window_visible(b));
        assert!(!wm.is_window_visible(a));
    }

    #[test]
    fn closing_group_root_keeps_its_column_weight_and_selected_tab() {
        let mut wm = WmState::new();
        let monitor = wm.add_monitor();
        let owner = WindowOwner {
            monitor,
            workspace: WorkspaceId::FIRST,
        };
        let a = wm.add_window(owner).unwrap();
        let b = wm.add_window(owner).unwrap();
        let c = wm.add_window(owner).unwrap();
        wm.execute(WmCommand::ToggleTilingMode { monitor }).unwrap();
        wm.execute(WmCommand::ResizeColumnPair {
            monitor,
            left: a,
            right: b,
            left_width: 200,
            right_width: 200,
            delta: 20,
        })
        .unwrap();
        let weight = wm.column_weights(monitor, WorkspaceId::FIRST).unwrap()[0];
        wm.execute(WmCommand::GroupTile {
            monitor,
            window: b,
            target: a,
        })
        .unwrap();
        wm.remove_window(a);
        assert_eq!(
            wm.tiled_windows(monitor, WorkspaceId::FIRST),
            Some([b, c].as_slice())
        );
        assert_eq!(
            wm.column_weights(monitor, WorkspaceId::FIRST).unwrap()[0],
            weight
        );
        assert_eq!(wm.keyboard_focus(), Some(b));
    }

    #[test]
    fn dropping_either_tab_onto_its_own_content_detaches_it() {
        for detach_root in [false, true] {
            let mut wm = WmState::new();
            let monitor = wm.add_monitor();
            let owner = WindowOwner {
                monitor,
                workspace: WorkspaceId::FIRST,
            };
            let a = wm.add_window(owner).unwrap();
            let b = wm.add_window(owner).unwrap();
            wm.execute(WmCommand::GroupTile {
                monitor,
                window: b,
                target: a,
            })
            .unwrap();
            let window = if detach_root { a } else { b };
            wm.execute(WmCommand::InsertTile {
                monitor,
                window,
                target: a,
                after: true,
            })
            .unwrap();
            let remaining = if detach_root { b } else { a };
            assert_eq!(
                wm.tiled_windows(monitor, WorkspaceId::FIRST),
                Some([remaining, window].as_slice())
            );
            assert_eq!(wm.group_tabs(remaining), Some(vec![remaining]));
            assert_eq!(wm.keyboard_focus(), Some(window));
        }
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
