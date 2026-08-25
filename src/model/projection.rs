use std::sync::Arc;

use objc2_core_foundation::CGRect;

use crate::actor::app::WindowId;
use crate::model::VirtualWorkspaceId;
use crate::model::server::{RuntimeDisplayData, RuntimeWindowData, RuntimeWorkspaceData};
use crate::sys::screen::SpaceId;
use crate::sys::window_server::WindowServerId;

/// A monotonically increasing version assigned to an observable desktop state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateRevision(u64);

impl StateRevision {
    pub fn get(self) -> u64 { self.0 }

    fn next(self) -> Self {
        let next = self.0.wrapping_add(1);
        Self(if next == 0 { 1 } else { next })
    }
}

/// Workspace data for the display context currently represented by the menu bar.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceContext {
    pub active_space: SpaceId,
    pub active_space_is_activated: bool,
    pub workspaces: Vec<RuntimeWorkspaceData>,
    pub active_workspace_idx: Option<u64>,
    pub active_workspace: Option<VirtualWorkspaceId>,
    pub windows: Vec<RuntimeWindowData>,
}

/// Focused window geometry needed by the border renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderTarget {
    pub window: WindowId,
    pub window_server_id: WindowServerId,
    pub frame: CGRect,
}

/// Immutable data from which presentation actors derive their own views.
///
/// This is intentionally small while presentation consumers are migrated. New
/// consumers should extend this state instead of querying the reactor or
/// subscribing to a second source of macOS events.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DesktopState {
    pub displays: Vec<RuntimeDisplayData>,
    pub focused_window: Option<WindowId>,
    pub mission_control_active: bool,
    pub border_target: Option<BorderTarget>,
    pub menu_bar_context: Option<WorkspaceContext>,
}

/// One committed version of [`DesktopState`].
#[derive(Debug, Clone)]
pub struct DesktopSnapshot {
    pub revision: StateRevision,
    pub state: DesktopState,
}

/// Commits changed desktop states and owns their revision sequence.
///
/// Dirty tracking avoids rebuilding projections for query-only work. Exact
/// state comparison makes every renderer idempotent without maintaining its
/// own lossy signature of the domain data.
#[derive(Debug, Default)]
pub struct ProjectionHub {
    revision: StateRevision,
    current: Option<Arc<DesktopSnapshot>>,
    dirty: bool,
}

impl ProjectionHub {
    pub fn mark_dirty(&mut self) { self.dirty = true }

    pub fn is_dirty(&self) -> bool { self.dirty }

    pub fn current(&self) -> Option<&Arc<DesktopSnapshot>> { self.current.as_ref() }

    pub fn publish(&mut self, state: DesktopState) -> Option<Arc<DesktopSnapshot>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }

        if self.current.as_ref().is_some_and(|snapshot| snapshot.state == state) {
            return None;
        }

        self.revision = self.revision.next();
        let snapshot = Arc::new(DesktopSnapshot { revision: self.revision, state });
        self.current = Some(snapshot.clone());
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_should_require_an_explicit_dirty_mark() {
        let mut hub = ProjectionHub::default();

        assert!(hub.publish(DesktopState::default()).is_none());
    }

    #[test]
    fn publish_should_not_advance_revision_for_equivalent_state() {
        let mut hub = ProjectionHub::default();
        hub.mark_dirty();
        let first = hub.publish(DesktopState::default()).expect("first snapshot");

        hub.mark_dirty();
        let duplicate = hub.publish(DesktopState::default());

        assert!(duplicate.is_none());
        assert_eq!(
            hub.current().map(|snapshot| snapshot.revision),
            Some(first.revision)
        );
    }

    #[test]
    fn publish_should_advance_revision_after_state_changes() {
        let mut hub = ProjectionHub::default();
        hub.mark_dirty();
        let first = hub.publish(DesktopState::default()).expect("first snapshot");

        hub.mark_dirty();
        let second = hub
            .publish(DesktopState {
                focused_window: Some(WindowId::new(42, 7)),
                ..DesktopState::default()
            })
            .expect("changed snapshot");

        assert_eq!(second.revision.get(), first.revision.get() + 1);
    }
}
