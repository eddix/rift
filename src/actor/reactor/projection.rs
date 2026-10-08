use crate::actor::reactor::Reactor;
use crate::actor::{border, command_palette};
use crate::model::projection::{
    BorderTarget, DesktopState,
};

impl Reactor {
    pub(super) fn publish_desktop_snapshot(&mut self) {
        if !self.presentation_manager.projections.is_dirty() {
            return;
        }

        let displays = self.query_displays();
        let state = DesktopState {
            displays,
            focused_window: self.main_window(),
            mission_control_active: self.is_mission_control_active(),
            border_target: self.snapshot_border_target(),
            ..DesktopState::default()
        };

        let Some(snapshot) = self.presentation_manager.projections.publish(state) else {
            return;
        };
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
