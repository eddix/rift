use tracing::trace;

use crate::actor::app::{WindowId, WindowInfo, native_tab_frames_match};
use crate::actor::reactor::WindowState;
use crate::actor::reactor::events::EventOutcome;
use crate::actor::reactor::managers::LayoutManager;
use crate::actor::reactor::transaction_manager::TransactionManager;
use crate::model::RiftState;
use crate::sys::window_server::WindowServerInfo;

#[derive(Debug)]
pub struct NativeTabFocusedPayload {
    pub previous: WindowId,
    pub current: WindowId,
    pub window: WindowInfo,
    pub window_server_info: Option<WindowServerInfo>,
}

pub fn handle_native_tab_focused(
    state: &mut RiftState,
    layout: &mut LayoutManager,
    transactions: &TransactionManager,
    payload: NativeTabFocusedPayload,
) -> anyhow::Result<EventOutcome> {
    let NativeTabFocusedPayload {
        previous,
        current,
        mut window,
        window_server_info,
    } = payload;

    let Some(previous_state) = state.windows.window(previous).cloned() else {
        trace!(
            ?previous,
            ?current,
            "Ignoring native-tab transition with a stale source"
        );
        return Ok(EventOutcome::no_change());
    };
    let Some(assignment) = state.windows.workspace_info_for_window(previous) else {
        trace!(
            ?previous,
            ?current,
            "Ignoring native-tab transition without a layout slot"
        );
        return Ok(EventOutcome::no_change());
    };
    let Some(current_wsid) = window.sys_id else {
        trace!(
            ?previous,
            ?current,
            "Ignoring native-tab transition without a WindowServer id"
        );
        return Ok(EventOutcome::no_change());
    };

    let is_replacement = previous != current
        && previous.pid == current.pid
        && native_tab_frames_match(previous_state.frame_monotonic, window.frame);
    if !is_replacement {
        trace!(?previous, ?current, "Ignoring unverified native-tab transition");
        return Ok(EventOutcome::no_change());
    }

    let previous_wsid = previous_state.info.sys_id;
    window.frame = previous_state.frame_monotonic;
    let current_state = WindowState {
        info: window,
        frame_monotonic: previous_state.frame_monotonic,
        is_manageable: previous_state.is_manageable,
        manage_override: previous_state.manage_override,
    };

    if let Some(info) = window_server_info {
        state.windows.clear_window_server_observed(info.id);
        state.windows.track_window_server_info(info);
    }
    state.windows.insert_window(current, current_state);
    state.windows.clear_window_server_observed(current_wsid);
    layout
        .layout_engine
        .rekey_window_identity(&mut state.windows, previous, current);
    state.windows.set_window_server_space(current_wsid, Some(assignment.space));
    state.windows.mark_window_visible(current_wsid);

    if let Some(previous_wsid) = previous_wsid {
        transactions.remove_for_window(previous_wsid);
    }
    state.windows.remove_window(previous);

    let mut outcome = EventOutcome::layout_changed(false)
        .with_arrange_space_scope(Some(assignment.space))
        .with_focused_window_broadcast(current);
    outcome.focused_window = Some(current);
    Ok(outcome)
}
