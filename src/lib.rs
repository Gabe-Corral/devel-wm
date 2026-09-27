//! Core modules for devel-wm.

pub mod compositor;
pub mod config;
pub mod input;
pub mod ipc;
pub mod layout;
pub mod perf;
pub mod render;
pub mod wm;

use smithay::reexports::wayland_server::DisplayHandle;

use compositor::Compositor;

pub struct LoopData {
    pub compositor: Compositor,
    pub display_handle: DisplayHandle,
    pub nested: Option<compositor::NestedOutput>,
    pub ipc: Option<ipc::IpcServer>,
}
