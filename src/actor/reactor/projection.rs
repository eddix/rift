use crate::actor::reactor::Reactor;
use crate::actor::{border, command_palette, menu_bar};
use crate::model::projection::{
    BorderTarget, DesktopState, DisplayWorkspaceContext, WorkspaceContext,
};

impl Reactor {
    pub(super) fn publish_desktop_snapshot(&mut self) {
        if !self.presentation_manager.projections.is_dirty() {
            return;
        }

        let displays = self.query_displays();
        let menu_bar_space = self.menu_bar_space();
        let mut menu_bar_sources = displays
            .iter()
            .filter(|display| display.info.space.is_some())
            .collect::<Vec<_>>();
        menu_bar_sources.sort_by(|a, b| {
            a.info
                .frame
                .origin
                .x
                .total_cmp(&b.info.frame.origin.x)
                .then_with(|| a.info.frame.origin.y.total_cmp(&b.info.frame.origin.y))
                .then_with(|| a.info.display_uuid.cmp(&b.info.display_uuid))
        });
        let menu_bar_displays = menu_bar_sources
            .into_iter()
            .filter_map(|display| {
                let space = display.info.space?;
                Some(DisplayWorkspaceContext {
                    display_uuid: display.info.display_uuid.clone(),
                    space,
                    is_active_context: display.is_active_context,
                    workspaces: self.snapshot_workspaces(space),
                })
            })
            .collect::<Vec<_>>();
        let menu_bar_context = menu_bar_space.map(|active_space| {
            let workspaces = menu_bar_displays
                .iter()
                .find(|display| display.space == active_space)
                .map(|display| display.workspaces.clone())
                .unwrap_or_else(|| self.snapshot_workspaces(active_space));
            WorkspaceContext {
                active_space,
                active_space_is_activated: self.is_space_active(active_space),
                workspaces,
                active_workspace_idx: self
                    .layout_manager
                    .layout_engine
                    .active_workspace_idx(active_space),
                active_workspace: self.layout_manager.layout_engine.active_workspace(active_space),
                windows: self.query_windows(Some(active_space)),
            }
        });
        let state = DesktopState {
            displays,
            focused_window: self.main_window(),
            mission_control_active: self.is_mission_control_active(),
            border_target: self.snapshot_border_target(),
            menu_bar_context,
            menu_bar_displays,
        };

        let Some(snapshot) = self.presentation_manager.projections.publish(state) else {
            return;
        };
        if let Some(menu_tx) = self.presentation_manager.menu_tx.as_ref() {
            menu_tx.send(menu_bar::Event::Snapshot(snapshot.clone()));
        }
        if let Some(border_tx) = self.presentation_manager.border_tx.as_ref() {
            border_tx.send(border::Event::Snapshot(snapshot.clone()));
        }
        if let Some(palette_tx) = self.presentation_manager.command_palette_tx.as_ref() {
            palette_tx.send(command_palette::Event::DesktopSnapshot(snapshot));
        }
    }

    fn snapshot_border_target(&self) -> Option<BorderTarget> {
        let window = self.main_window()?;
        let state = self.state.windows.window(window)?;
        if state.info.is_minimized
            || !state.info.is_standard
            || state.frame_monotonic.size.width <= 0.0
            || state.frame_monotonic.size.height <= 0.0
        {
            return None;
        }

        let window_server_id = state.info.sys_id?;
        let space = self.best_space_for_window_id(window)?;
        let corner_radius = self
            .state
            .windows
            .get_window_server_info(window_server_id)
            .and_then(|info| info.corner_radius);
        self.state.windows.is_window_visible(window_server_id).then_some(BorderTarget {
            window,
            window_server_id,
            space,
            frame: state.frame_monotonic,
            corner_radius,
        })
    }
}
