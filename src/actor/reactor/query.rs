use std::sync::mpsc::{RecvError, SyncSender, sync_channel};

use objc2_core_foundation::{CGPoint, CGRect};
use rift_protocol::{
    ApplicationData, ContainerTreeNode, LayoutMode, LayoutStateData, Point, Rect, Size,
    WindowLayoutPosition, WorkspaceLayoutData,
};

use crate::actor::app::WindowId;
use crate::actor::reactor::{Event, Reactor, Sender};
use crate::common::collections::{HashMap, HashSet};
use crate::model::WindowWorkspaceInfo;
use crate::model::command_palette::{
    PaletteAction, PaletteEntry, PaletteEntryId, PaletteEntryKind, PaletteFocusOrigin,
    PaletteSnapshot,
};
use crate::model::server::{
    RuntimeDisplayData, RuntimeWindowData, RuntimeWorkspaceData, protocol_rect,
};
use crate::model::virtual_workspace::VirtualWorkspaceId;
use crate::sys::screen::{ScreenInfo, SpaceId};

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x = a.origin.x.min(b.origin.x);
    let y = a.origin.y.min(b.origin.y);
    let max_x = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let max_y = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect {
        origin: Point { x, y },
        size: Size {
            width: max_x - x,
            height: max_y - y,
        },
    }
}

fn attach_target_frames(
    node: &mut ContainerTreeNode,
    window_frames: &HashMap<WindowId, Rect>,
) -> Option<Rect> {
    if let Some(window) = node.window_id {
        node.frame = *window_frames.get(&WindowId::new(window.pid, window.idx))?;
        return Some(node.frame);
    }

    node.frame = node
        .children
        .iter_mut()
        .filter_map(|child| attach_target_frames(child, window_frames))
        .reduce(union_rect)?;
    Some(node.frame)
}

fn propagate_single_child_allocations(node: &mut ContainerTreeNode) {
    if let [child] = node.children.as_mut_slice()
        && child.window_id.is_none()
    {
        child.frame = node.frame;
    }
    node.children.iter_mut().for_each(propagate_single_child_allocations);
}

fn logical_window_positions(tree: &ContainerTreeNode) -> HashMap<WindowId, WindowLayoutPosition> {
    tree.children
        .iter()
        .enumerate()
        .filter(|(_, column)| column.role.as_deref() == Some("column"))
        .flat_map(|(column, node)| {
            node.children.iter().enumerate().filter_map(move |(row, node)| {
                let window = node.window_id?;
                Some((WindowId::new(window.pid, window.idx), WindowLayoutPosition {
                    column,
                    row,
                }))
            })
        })
        .collect()
}

#[derive(Clone)]
pub struct ReactorQueryHandle {
    tx: Sender,
}

impl ReactorQueryHandle {
    pub(super) fn new(tx: Sender) -> Self { Self { tx } }

    fn send_query<T>(
        &self,
        build: impl FnOnce(SyncSender<T>) -> QueryRequest,
    ) -> Result<T, RecvError> {
        let (tx, rx) = sync_channel(1);
        if self.tx.try_send(Event::Query(build(tx))).is_err() {
            return Err(RecvError);
        }
        rx.recv().map_err(|_| RecvError)
    }

    pub fn query_workspaces(&self, space_id: Option<SpaceId>) -> Vec<RuntimeWorkspaceData> {
        self.send_query(|resp| QueryRequest::Workspaces { space_id, resp })
            .unwrap_or_default()
    }

    pub fn query_windows(&self, space_id: Option<SpaceId>) -> Vec<RuntimeWindowData> {
        self.send_query(|resp| QueryRequest::Windows { space_id, resp })
            .unwrap_or_default()
    }

    pub fn query_active_workspace(&self, space_id: Option<SpaceId>) -> Option<VirtualWorkspaceId> {
        self.send_query(|resp| QueryRequest::ActiveWorkspace { space_id, resp })
            .ok()
            .flatten()
    }

    pub fn query_displays(&self) -> Vec<RuntimeDisplayData> {
        self.send_query(QueryRequest::Displays).unwrap_or_default()
    }

    pub fn query_workspace_layouts(
        &self,
        space_id: Option<SpaceId>,
        workspace_id: Option<usize>,
    ) -> Vec<WorkspaceLayoutData> {
        self.send_query(|resp| QueryRequest::WorkspaceLayouts { space_id, workspace_id, resp })
            .unwrap_or_default()
    }

    pub fn query_window_info(&self, window_id: WindowId) -> Option<RuntimeWindowData> {
        self.send_query(|resp| QueryRequest::WindowInfo { window_id, resp })
            .ok()
            .flatten()
    }

    pub fn query_applications(&self) -> Vec<ApplicationData> {
        self.send_query(QueryRequest::Applications).unwrap_or_default()
    }

    pub fn query_command_palette(&self) -> PaletteSnapshot {
        self.send_query(QueryRequest::CommandPalette).unwrap_or_default()
    }

    pub fn query_layout_state(
        &self,
        space_id: Option<u64>,
        workspace_id: Option<usize>,
    ) -> Option<LayoutStateData> {
        self.send_query(|resp| QueryRequest::LayoutState { space_id, workspace_id, resp })
            .ok()
            .flatten()
    }

    pub fn query_metrics(&self) -> serde_json::Value {
        self.send_query(QueryRequest::Metrics).unwrap_or_else(|_| serde_json::json!({}))
    }
}

#[derive(Debug)]
pub enum QueryRequest {
    Workspaces {
        space_id: Option<SpaceId>,
        resp: SyncSender<Vec<RuntimeWorkspaceData>>,
    },
    Windows {
        space_id: Option<SpaceId>,
        resp: SyncSender<Vec<RuntimeWindowData>>,
    },
    ActiveWorkspace {
        space_id: Option<SpaceId>,
        resp: SyncSender<Option<VirtualWorkspaceId>>,
    },
    Displays(SyncSender<Vec<RuntimeDisplayData>>),
    WorkspaceLayouts {
        space_id: Option<SpaceId>,
        workspace_id: Option<usize>,
        resp: SyncSender<Vec<WorkspaceLayoutData>>,
    },
    WindowInfo {
        window_id: WindowId,
        resp: SyncSender<Option<RuntimeWindowData>>,
    },
    Applications(SyncSender<Vec<ApplicationData>>),
    CommandPalette(SyncSender<PaletteSnapshot>),
    LayoutState {
        space_id: Option<u64>,
        workspace_id: Option<usize>,
        resp: SyncSender<Option<LayoutStateData>>,
    },
    Metrics(SyncSender<serde_json::Value>),
}

impl Reactor {
    pub(super) fn handle_query_request(&mut self, req: QueryRequest) {
        match req {
            QueryRequest::Workspaces { space_id, resp } => {
                let _ = resp.send(self.query_workspaces(space_id));
            }
            QueryRequest::Windows { space_id, resp } => {
                let _ = resp.send(self.query_windows(space_id));
            }
            QueryRequest::ActiveWorkspace { space_id, resp } => {
                let _ = resp.send(self.query_active_workspace(space_id));
            }
            QueryRequest::Displays(resp) => {
                let _ = resp.send(self.query_displays());
            }
            QueryRequest::WorkspaceLayouts { space_id, workspace_id, resp } => {
                let _ = resp.send(self.query_workspace_layouts(space_id, workspace_id));
            }
            QueryRequest::WindowInfo { window_id, resp } => {
                let _ = resp.send(self.query_window_info(window_id));
            }
            QueryRequest::Applications(resp) => {
                let _ = resp.send(self.query_applications());
            }
            QueryRequest::CommandPalette(resp) => {
                let _ = resp.send(self.query_command_palette());
            }
            QueryRequest::LayoutState { space_id, workspace_id, resp } => {
                let _ = resp.send(self.query_layout_state(space_id, workspace_id));
            }
            QueryRequest::Metrics(resp) => {
                let _ = resp.send(self.query_metrics());
            }
        }
    }

    fn default_query_space(&self) -> Option<SpaceId> {
        self.workspace_command_space()
            .or_else(|| self.active_display_space())
            .or_else(|| self.raw_command_space())
    }

    #[cfg(test)]
    pub(crate) fn test_default_query_space(&self) -> Option<SpaceId> { self.default_query_space() }

    pub fn query_workspaces(&mut self, space_id: Option<SpaceId>) -> Vec<RuntimeWorkspaceData> {
        self.handle_workspace_query(space_id)
    }

    pub fn query_windows(&self, space_id: Option<SpaceId>) -> Vec<RuntimeWindowData> {
        self.handle_windows_query(space_id)
    }

    pub fn query_active_workspace(&self, space_id: Option<SpaceId>) -> Option<VirtualWorkspaceId> {
        self.handle_active_workspace_query(space_id)
    }

    pub fn query_displays(&self) -> Vec<RuntimeDisplayData> { self.handle_displays_query() }

    pub fn query_workspace_layouts(
        &mut self,
        space_id: Option<SpaceId>,
        workspace_id: Option<usize>,
    ) -> Vec<WorkspaceLayoutData> {
        self.handle_workspace_layouts_query(space_id, workspace_id)
    }

    pub fn query_window_info(&self, window_id: WindowId) -> Option<RuntimeWindowData> {
        self.handle_window_info_query(window_id)
    }

    pub fn query_applications(&self) -> Vec<ApplicationData> { self.handle_applications_query() }

    pub fn query_command_palette(&self) -> PaletteSnapshot { self.handle_command_palette_query() }

    pub fn query_layout_state(
        &self,
        space_id: Option<u64>,
        workspace_id: Option<usize>,
    ) -> Option<LayoutStateData> {
        self.handle_layout_state_query(space_id, workspace_id)
    }

    pub fn query_metrics(&self) -> serde_json::Value { self.handle_metrics_query() }

    pub(super) fn menu_bar_space(&self) -> Option<SpaceId> {
        self.resolve_menu_bar_space_with_preferred(self.space_state.menu_bar_space)
    }

    fn resolve_menu_bar_space_with_preferred(
        &self,
        preferred_space: Option<SpaceId>,
    ) -> Option<SpaceId> {
        preferred_space
            .filter(|space| {
                self.space_state.screens.iter().any(|screen| screen.space == Some(*space))
            })
            .or_else(|| self.default_query_space())
    }

    #[cfg(test)]
    pub(crate) fn test_resolve_menu_bar_space_with_preferred(
        &self,
        preferred_space: Option<SpaceId>,
    ) -> Option<SpaceId> {
        self.resolve_menu_bar_space_with_preferred(preferred_space)
    }

    fn handle_workspace_query(
        &mut self,
        space_id_param: Option<SpaceId>,
    ) -> Vec<RuntimeWorkspaceData> {
        let Some(space) = space_id_param.or_else(|| self.default_query_space()) else {
            return Vec::new();
        };
        let workspace_list = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space);
        self.project_workspaces(space, &workspace_list)
    }

    /// Build observable workspace data without initializing or mutating topology.
    pub(super) fn snapshot_workspaces(&self, space: SpaceId) -> Vec<RuntimeWorkspaceData> {
        let workspace_list = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .existing_workspaces(space);
        self.project_workspaces(space, &workspace_list)
    }

    fn project_workspaces(
        &self,
        space: SpaceId,
        workspace_list: &[(crate::model::VirtualWorkspaceId, String)],
    ) -> Vec<RuntimeWorkspaceData> {
        let mut workspaces = Vec::with_capacity(workspace_list.len());
        let active_workspace = self.layout_manager.layout_engine.active_workspace(space);

        for (index, (workspace_id, workspace_name)) in workspace_list.iter().enumerate() {
            let is_active = active_workspace == Some(*workspace_id);
            let workspace_windows_ids = self
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .workspace_windows(&self.state.windows, space, *workspace_id);

            let predicted_positions = if !is_active {
                let screen_info = self
                    .space_state
                    .screens
                    .iter()
                    .find(|s| s.space == Some(space))
                    .or_else(|| self.space_state.screens.first());

                if let Some(screen) = screen_info {
                    let display_uuid = screen.display_uuid_opt();
                    let gaps = self.config.settings.layout.gaps.effective_for_display(display_uuid);
                    self.layout_manager.layout_engine.calculate_layout_for_workspace(
                        &self.state.windows,
                        space,
                        *workspace_id,
                        screen.frame,
                        &gaps,
                        self.config.settings.ui.stack_line.thickness(),
                        self.config.settings.ui.stack_line.horiz_placement,
                        self.config.settings.ui.stack_line.vert_placement,
                    )
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            let predicted_map: std::collections::HashMap<WindowId, CGRect> =
                predicted_positions.into_iter().collect();

            let logical_positions = self
                .layout_manager
                .layout_engine
                .query_workspace_layout(space, Some(index))
                .map(|snapshot| logical_window_positions(&snapshot.container_tree))
                .unwrap_or_default();

            let mut windows: Vec<RuntimeWindowData> = Vec::new();
            for wid in workspace_windows_ids.into_iter() {
                if let Some(mut wd) = self.create_window_data(wid) {
                    if !wd.is_floating {
                        wd.layout_position = logical_positions.get(&wid).copied();
                    }
                    if !is_active {
                        if let Some(pred) = predicted_map.get(&wid).copied() {
                            wd.info.frame = pred;
                        }
                    }
                    windows.push(wd);
                }
            }
            // Scrolling windows are returned in their logical visual order. Floating and
            // non-column layouts retain their existing stable membership order afterward.
            windows.sort_by_key(|window| {
                window.layout_position.map_or((1, usize::MAX, usize::MAX), |position| {
                    (0, position.column, position.row)
                })
            });

            let layout_mode = self
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .workspace_info(space, *workspace_id)
                .map(|ws| ws.layout_mode().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            workspaces.push(RuntimeWorkspaceData {
                id: format!("{:?}", workspace_id),
                name: workspace_name.to_string(),
                layout_mode,
                is_active,
                window_count: windows.len(),
                windows,
                index,
            });
        }

        workspaces
    }

    fn handle_workspace_layouts_query(
        &mut self,
        space_id_param: Option<SpaceId>,
        workspace_id: Option<usize>,
    ) -> Vec<WorkspaceLayoutData> {
        let Some(space) = space_id_param.or_else(|| self.default_query_space()) else {
            return Vec::new();
        };

        let workspace_list = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager_mut()
            .list_workspaces(space);
        let active_workspace = self.layout_manager.layout_engine.active_workspace(space);

        workspace_list
            .iter()
            .enumerate()
            .filter(|(index, _)| workspace_id.map(|target| target == *index).unwrap_or(true))
            .filter_map(|(index, (id, name))| {
                let layout_mode = self
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .workspace_info(space, *id)
                    .map(|ws| ws.layout_mode().to_string())?;

                Some(WorkspaceLayoutData {
                    id: format!("{:?}", id),
                    index,
                    name: name.clone(),
                    layout_mode,
                    is_active: active_workspace == Some(*id),
                })
            })
            .collect()
    }

    fn handle_active_workspace_query(
        &self,
        space_id_param: Option<SpaceId>,
    ) -> Option<VirtualWorkspaceId> {
        let space_id = space_id_param.or_else(|| self.default_query_space())?;
        self.layout_manager.layout_engine.active_workspace(space_id)
    }

    fn handle_displays_query(&self) -> Vec<RuntimeDisplayData> {
        let active_context_space = self.active_display_space();
        let active_space_ids = self.active_space_ids();
        let active_space_set: HashSet<u64> = active_space_ids.iter().copied().collect();
        self.space_state
            .screens
            .iter()
            .map(|screen| {
                let space_for_screen = screen.space;
                let all_space_ids = self
                    .space_state
                    .display_space_ids
                    .get(&screen.display_uuid)
                    .cloned()
                    .unwrap_or_else(|| space_for_screen.map(|s| vec![s]).unwrap_or_default());
                let per_display_active_space_ids: Vec<u64> = all_space_ids
                    .iter()
                    .filter(|space| active_space_set.contains(&space.get()))
                    .map(|space| space.get())
                    .collect();
                let per_display_inactive_space_ids: Vec<u64> = all_space_ids
                    .iter()
                    .filter(|space| !active_space_set.contains(&space.get()))
                    .map(|space| space.get())
                    .collect();
                RuntimeDisplayData {
                    info: ScreenInfo {
                        space: space_for_screen,
                        ..screen.clone()
                    },
                    is_active_space: space_for_screen
                        .map(|s| active_space_set.contains(&s.get()))
                        .unwrap_or(false),
                    is_active_context: match (space_for_screen, active_context_space) {
                        (Some(s1), Some(s2)) => s1 == s2,
                        _ => false,
                    },
                    active_space_ids: per_display_active_space_ids,
                    inactive_space_ids: per_display_inactive_space_ids,
                }
            })
            .collect()
    }

    fn handle_windows_query(&self, space_id: Option<SpaceId>) -> Vec<RuntimeWindowData> {
        let target_space = space_id.or_else(|| self.default_query_space());

        if let Some(space) = target_space {
            let active_windows = self
                .layout_manager
                .layout_engine
                .windows_in_active_workspace(&self.state.windows, space);

            active_windows
                .into_iter()
                .filter_map(|wid| self.create_window_data(wid))
                .collect()
        } else {
            self.state
                .windows
                .iter_windows()
                .map(|(wid, _)| wid)
                .filter_map(|wid| self.create_window_data(wid))
                .collect()
        }
    }

    fn handle_window_info_query(&self, window_id: WindowId) -> Option<RuntimeWindowData> {
        self.create_window_data(window_id)
    }

    fn handle_applications_query(&self) -> Vec<ApplicationData> {
        self.app_manager
            .apps
            .iter()
            .map(|(&pid, app)| {
                let window_count = self.state.windows.window_ids_for_pid(pid).count();

                let is_frontmost = self
                    .main_window_tracker
                    .main_window()
                    .map(|wid| wid.pid == pid)
                    .unwrap_or(false);

                ApplicationData {
                    pid,
                    bundle_id: app.info.bundle_id.clone(),
                    name: app.info.localized_name.clone().unwrap_or_else(|| "Unknown".to_string()),
                    is_frontmost,
                    window_count,
                }
            })
            .collect()
    }

    fn handle_command_palette_query(&self) -> PaletteSnapshot {
        #[derive(Clone)]
        struct WorkspaceLabel {
            name: String,
            is_active: bool,
        }

        #[cfg(not(test))]
        let server_focus = crate::sys::window_server::key_focused_window();
        #[cfg(test)]
        let server_focus: Option<(WindowId, SpaceId)> = None;
        let focused_window = server_focus.map(|(window, _)| window).or_else(|| self.main_window());
        let focused_record = focused_window.and_then(|window| self.state.windows.record(window));
        let focused_display = focused_window
            .and_then(|window| self.state.windows.window(window))
            .and_then(|window| {
                let center = CGPoint::new(
                    window.frame_monotonic.origin.x + window.frame_monotonic.size.width / 2.0,
                    window.frame_monotonic.origin.y + window.frame_monotonic.size.height / 2.0,
                );
                self.space_state.screens.iter().find(|screen| {
                    center.x >= screen.frame.origin.x
                        && center.x <= screen.frame.origin.x + screen.frame.size.width
                        && center.y >= screen.frame.origin.y
                        && center.y <= screen.frame.origin.y + screen.frame.size.height
                })
            });
        let focused_space = focused_record
            .and_then(|record| record.native_space().or(record.workspace().map(|item| item.space)))
            .or_else(|| focused_display.and_then(|screen| screen.space))
            .or_else(|| self.default_query_space());

        let mut display_by_space = HashMap::default();
        for screen in &self.space_state.screens {
            let Some(space) = screen.space else {
                continue;
            };
            display_by_space.insert(
                space,
                (
                    screen.name.clone().unwrap_or_else(|| "Display".to_string()),
                    screen.display_uuid.clone(),
                    screen.id.as_u32(),
                ),
            );
        }

        let mut workspace_labels: HashMap<WindowWorkspaceInfo, WorkspaceLabel> = HashMap::default();
        for screen in &self.space_state.screens {
            let Some(space) = screen.space else {
                continue;
            };
            let active = self.layout_manager.layout_engine.active_workspace(space);
            for (workspace_id, name) in self
                .layout_manager
                .layout_engine
                .virtual_workspace_manager()
                .existing_workspaces(space)
                .iter()
            {
                workspace_labels.insert(
                    WindowWorkspaceInfo {
                        space,
                        workspace_id: *workspace_id,
                    },
                    WorkspaceLabel {
                        name: name.clone(),
                        is_active: active == Some(*workspace_id),
                    },
                );
            }
        }

        let bundle_ids = self
            .app_manager
            .apps
            .values()
            .filter_map(|app| app.info.bundle_id.clone())
            .collect::<HashSet<_>>();
        let auxiliary_pids = self
            .app_manager
            .apps
            .iter()
            .filter_map(|(&pid, app)| {
                app.info
                    .bundle_id
                    .as_deref()
                    .is_some_and(|bundle| is_auxiliary_bundle(bundle, &bundle_ids))
                    .then_some(pid)
            })
            .collect::<HashSet<_>>();

        let mut entries = Vec::new();
        for (window_id, state) in self.state.windows.iter_windows() {
            let Some(app) = self.app_manager.apps.get(&window_id.pid) else {
                continue;
            };
            if !is_palette_application_eligible(app.info.bundle_id.as_deref())
                || !is_palette_window_eligible(state)
            {
                continue;
            }
            let record = self.state.windows.record(window_id);
            let assignment = record.and_then(|record| record.workspace());
            let workspace = assignment.and_then(|assignment| workspace_labels.get(&assignment));
            let native_space = record
                .and_then(|record| record.native_space())
                .or_else(|| assignment.map(|assignment| assignment.space));
            let display = native_space.and_then(|space| display_by_space.get(&space));
            let app_name = app.info.localized_name.clone().unwrap_or_else(|| "Unknown".to_string());
            let title = if state.info.title.is_empty() {
                app_name.clone()
            } else {
                state.info.title.clone()
            };
            let workspace_name = workspace.map(|workspace| workspace.name.as_str());
            let display_name = display.map(|display| display.0.as_str());
            let mut metadata = vec![app_name.clone()];
            if let Some(workspace) = workspace_name {
                metadata.push(workspace.to_string());
            } else {
                metadata.push("Unmanaged".to_string());
            }
            if let Some(display) = display_name {
                metadata.push(display.to_string());
            }
            let mut keywords = vec![app_name.clone()];
            if let Some(bundle_id) = app.info.bundle_id.as_ref() {
                keywords.push(bundle_id.clone());
            }
            if let Some(workspace) = workspace_name {
                keywords.push(workspace.to_string());
            }
            if let Some(display) = display_name {
                keywords.push(display.to_string());
            }
            keywords.push(if state.is_admitted() {
                "managed".to_string()
            } else {
                "unmanaged".to_string()
            });
            let current_workspace = workspace.is_some_and(|workspace| workspace.is_active);
            let current_display = native_space.is_some_and(|space| Some(space) == focused_space);
            let entry = PaletteEntry::new(
                PaletteEntryId::Window(window_id),
                PaletteEntryKind::Window,
                title,
                metadata.join(" · "),
                keywords,
                Some(window_id.pid),
                PaletteAction::FocusWindow {
                    window_id,
                    window_server_id: state.info.sys_id,
                },
            )
            .with_location_boosts(current_workspace, current_display);
            entries.push(if auxiliary_pids.contains(&window_id.pid) {
                entry.with_search_penalty(1_500)
            } else {
                entry
            });
        }

        for (&pid, app) in &self.app_manager.apps {
            if auxiliary_pids.contains(&pid)
                || !is_palette_application_eligible(app.info.bundle_id.as_deref())
            {
                continue;
            }
            let window_count = self
                .state
                .windows
                .window_ids_for_pid(pid)
                .filter(|&window_id| {
                    self.state.windows.window(window_id).is_some_and(is_palette_window_eligible)
                })
                .count();
            let app_name = app.info.localized_name.clone().unwrap_or_else(|| "Unknown".to_string());
            let mut keywords = Vec::new();
            if let Some(bundle_id) = app.info.bundle_id.as_ref() {
                keywords.push(bundle_id.clone());
            }
            let entry = PaletteEntry::new(
                PaletteEntryId::Application(pid),
                PaletteEntryKind::Application,
                app_name,
                if window_count == 1 {
                    "1 window".to_string()
                } else {
                    format!("{window_count} windows")
                },
                keywords,
                Some(pid),
                PaletteAction::ActivateApplication(pid),
            );
            entries.push(if window_count == 0 {
                entry
            } else {
                entry.hidden_when_empty()
            });
        }

        let command_workspace_names = focused_space
            .map(|space| {
                self.layout_manager
                    .layout_engine
                    .virtual_workspace_manager()
                    .existing_workspaces(space)
                    .iter()
                    .map(|(_, name)| name.clone())
                    .collect::<Vec<_>>()
            })
            .filter(|names| !names.is_empty())
            .unwrap_or_else(|| self.config.virtual_workspaces.workspace_names.clone());
        for (index, name) in command_workspace_names.iter().enumerate() {
            entries.push(command_entry(
                format!("workspace.switch.{index}"),
                format!("Switch Workspace → {name}"),
                ["switch".to_string(), "workspace".to_string(), name.clone()],
                PaletteAction::SwitchWorkspace(index),
            ));
            entries.push(command_entry(
                format!("workspace.move.{index}"),
                format!("Move Window → {name}"),
                ["move".to_string(), "window".to_string(), name.clone()],
                PaletteAction::MoveWindowToWorkspace(index),
            ));
        }
        entries.extend([
            command_entry(
                "workspace.next".to_string(),
                "Next Workspace".to_string(),
                ["workspace".to_string(), "forward".to_string()],
                PaletteAction::NextWorkspace,
            ),
            command_entry(
                "workspace.previous".to_string(),
                "Previous Workspace".to_string(),
                ["workspace".to_string(), "back".to_string()],
                PaletteAction::PreviousWorkspace,
            ),
            command_entry(
                "workspace.last".to_string(),
                "Last Workspace".to_string(),
                ["workspace".to_string(), "recent".to_string()],
                PaletteAction::LastWorkspace,
            ),
            command_entry(
                "window.floating".to_string(),
                "Toggle Floating".to_string(),
                ["window".to_string(), "tile".to_string()],
                PaletteAction::ToggleFloating,
            ),
            command_entry(
                "window.fullscreen".to_string(),
                "Toggle Fullscreen".to_string(),
                ["window".to_string(), "maximize".to_string()],
                PaletteAction::ToggleFullscreen,
            ),
            command_entry(
                "window.fullscreen-within-gaps".to_string(),
                "Toggle Fullscreen Within Gaps".to_string(),
                [
                    "window".to_string(),
                    "maximize".to_string(),
                    "gaps".to_string(),
                ],
                PaletteAction::ToggleFullscreenWithinGaps,
            ),
            command_entry(
                "config.reload".to_string(),
                "Reload Rift Config".to_string(),
                ["configuration".to_string(), "refresh".to_string()],
                PaletteAction::ReloadConfig,
            ),
        ]);
        for mode in [
            LayoutMode::Traditional,
            LayoutMode::Bsp,
            LayoutMode::Stack,
            LayoutMode::MasterStack,
            LayoutMode::Scrolling,
        ] {
            entries.push(command_entry(
                format!("layout.{mode}"),
                format!("Set Layout → {mode}"),
                ["layout".to_string(), mode.to_string()],
                PaletteAction::SetLayout(mode),
            ));
        }
        for screen in &self.space_state.screens {
            let name = screen.name.clone().unwrap_or_else(|| "Display".to_string());
            entries.push(command_entry(
                format!("display.focus.{}", screen.display_uuid),
                format!("Focus Display → {name}"),
                ["display".to_string(), "screen".to_string(), name],
                PaletteAction::FocusDisplay(screen.display_uuid.clone()),
            ));
        }

        #[cfg(not(test))]
        let frontmost_pid = crate::sys::window_server::frontmost_process_pid()
            .or_else(|| self.main_window_tracker.frontmost_pid());
        #[cfg(test)]
        let frontmost_pid = self.main_window_tracker.frontmost_pid();
        let focus_origin = frontmost_pid.map(|app_pid| {
            let window_id = focused_window.filter(|window| window.pid == app_pid);
            let window_server_id = window_id
                .and_then(|window| self.state.windows.window(window))
                .and_then(|state| state.info.sys_id);
            PaletteFocusOrigin {
                app_pid,
                window_id,
                window_server_id,
            }
        });
        let target_display_id = focused_display
            .map(|screen| screen.id.as_u32())
            .or_else(|| {
                focused_space
                    .and_then(|space| display_by_space.get(&space).map(|display| display.2))
            })
            .or_else(|| {
                self.space_state
                    .screens
                    .iter()
                    .find(|screen| screen.space == self.active_display_space())
                    .map(|screen| screen.id.as_u32())
            });

        PaletteSnapshot {
            entries,
            focus_origin,
            target_display_id,
        }
    }

    fn handle_layout_state_query(
        &self,
        space_id_u64: Option<u64>,
        workspace_id: Option<usize>,
    ) -> Option<LayoutStateData> {
        let space_id = match space_id_u64 {
            Some(space_id) => SpaceId::new(space_id),
            None => self.default_query_space()?,
        };
        if !self.space_state.iter_known_spaces().any(|space| space == space_id) {
            return None;
        }

        let mut snapshot = self
            .layout_manager
            .layout_engine
            .query_workspace_layout(space_id, workspace_id)?;
        let screen = self.space_state.screen_by_space(space_id)?;
        let display_uuid = screen.display_uuid_owned();
        let gaps = self.config.settings.layout.gaps.effective_for_display(display_uuid.as_deref());
        let target_frames = self.layout_manager.layout_engine.calculate_workspace_layout(
            space_id,
            snapshot.workspace_id,
            screen.frame,
            &gaps,
            self.config.settings.ui.stack_line.thickness(),
            self.config.settings.ui.stack_line.horiz_placement,
            self.config.settings.ui.stack_line.vert_placement,
        );
        let target_frames: HashMap<WindowId, Rect> = target_frames
            .into_iter()
            .map(|(window, frame)| (window, protocol_rect(frame)))
            .collect();
        let tiling_area = crate::layout_engine::utils::compute_tiling_area(screen.frame, &gaps);
        attach_target_frames(&mut snapshot.container_tree, &target_frames);
        snapshot.container_tree.frame = protocol_rect(tiling_area);
        propagate_single_child_allocations(&mut snapshot.container_tree);
        let workspace_windows = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .workspace_windows(&self.state.windows, space_id, snapshot.workspace_id);
        let floating_windows: Vec<WindowId> = workspace_windows
            .iter()
            .filter(|&&wid| self.layout_manager.layout_engine.is_window_floating(wid))
            .copied()
            .collect();

        let tiled_windows: Vec<WindowId> = workspace_windows
            .iter()
            .filter(|&&wid| !self.layout_manager.layout_engine.is_window_floating(wid))
            .copied()
            .collect();

        let focused_window = self.main_window().filter(|wid| workspace_windows.contains(wid));

        Some(LayoutStateData {
            space_id: space_id.get(),
            workspace_id: snapshot.workspace_index,
            is_active_workspace: snapshot.is_active,
            mode: snapshot.mode.to_string(),
            floating_windows: floating_windows.into_iter().map(Into::into).collect(),
            tiled_windows: tiled_windows.into_iter().map(Into::into).collect(),
            focused_window: focused_window.map(Into::into),
            selected_window: snapshot.selected_window.map(Into::into),
            container_tree: snapshot.container_tree,
        })
    }

    fn handle_metrics_query(&self) -> serde_json::Value {
        let stats = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .get_stats(&self.state.windows);

        let workspace_stats: crate::common::collections::HashMap<String, usize> = stats
            .workspace_window_counts
            .iter()
            .map(|(id, count)| (format!("{:?}", id), *count))
            .collect();

        serde_json::json!({
               "windows_managed": self.state.windows.tracked_window_count(),
            "workspaces": stats.total_workspaces,
            "applications": self.app_manager.apps.len(),
            "screens": self.space_state.screens.len(),
            "workspace_stats": workspace_stats,
        })
    }

    pub(crate) fn serialize_state(&mut self) -> Result<String, serde_json::Error> {
        let layout_engine_ron = self.layout_manager.layout_engine.serialize_to_string();
        let stats = self
            .layout_manager
            .layout_engine
            .virtual_workspace_manager()
            .get_stats(&self.state.windows);
        let mut workspace_window_counts = serde_json::Map::new();
        for (ws_id, count) in &stats.workspace_window_counts {
            workspace_window_counts.insert(format!("{:?}", ws_id), serde_json::json!(*count));
        }

        let mut spaces_intermediate: Vec<(
            u64,
            Vec<(
                crate::model::VirtualWorkspaceId,
                String,
                bool,
                Vec<crate::actor::app::WindowId>,
                Option<crate::actor::app::WindowId>,
                Vec<(crate::actor::app::WindowId, objc2_core_foundation::CGRect)>,
            )>,
        )> = Vec::new();

        for screen in &self.space_state.screens {
            if let Some(space) = screen.space {
                let workspaces = self
                    .layout_manager
                    .layout_engine
                    .virtual_workspace_manager_mut()
                    .list_workspaces(space);
                let active_ws = self.layout_manager.layout_engine.active_workspace(space);

                let mut ws_entries = Vec::new();
                for (workspace_id, workspace_name) in workspaces {
                    let window_ids: Vec<crate::actor::app::WindowId> =
                        self.state.windows.workspace_windows(space, workspace_id);

                    let last_focused = self
                        .layout_manager
                        .layout_engine
                        .virtual_workspace_manager()
                        .last_focused_window(space, workspace_id);

                    let floating_positions = self
                        .layout_manager
                        .layout_engine
                        .workspace_floating_positions(space, workspace_id);

                    ws_entries.push((
                        workspace_id,
                        workspace_name,
                        active_ws == Some(workspace_id),
                        window_ids,
                        last_focused,
                        floating_positions,
                    ));
                }

                spaces_intermediate.push((space.get(), ws_entries));
            }
        }

        let mut mapping_intermediate: Vec<(
            u64,
            crate::actor::app::WindowId,
            crate::model::VirtualWorkspaceId,
        )> = Vec::new();
        for (window_id, assignment) in self.state.windows.iter_workspace_assignments() {
            mapping_intermediate.push((assignment.space.get(), window_id, assignment.workspace_id));
        }

        let mut included_windows: HashSet<crate::actor::app::WindowId> = HashSet::default();

        let mut spaces_json = Vec::new();
        for (space_num, ws_entries) in spaces_intermediate {
            let mut ws_json = Vec::new();
            for (
                workspace_id,
                workspace_name,
                is_active,
                window_ids,
                last_focused,
                floating_positions,
            ) in ws_entries
            {
                let mut windows_json = Vec::new();
                for wid in window_ids {
                    if let Some(window_data) = self.create_window_data(wid) {
                        let v = serde_json::to_value(&window_data)
                            .unwrap_or_else(|_| serde_json::json!({ "id": wid.to_debug_string() }));
                        windows_json.push(v);
                    } else {
                        windows_json.push(serde_json::json!({ "id": wid.to_debug_string() }));
                    }

                    let _ = included_windows.insert(wid);
                }

                let last_focused_json = last_focused.map(|w| w.to_debug_string());

                let floating_json: Vec<serde_json::Value> = floating_positions
                    .into_iter()
                    .map(|(wid, rect)| {
                        serde_json::json!({
                            "window": wid.to_debug_string(),
                            "rect": {
                                "x": rect.origin.x,
                                "y": rect.origin.y,
                                "w": rect.size.width,
                                "h": rect.size.height
                            }
                        })
                    })
                    .collect();

                let id_str = workspace_id.to_string();
                let digits: String = id_str.chars().filter(|c| c.is_ascii_digit()).collect();
                let id_num = digits.parse::<u64>().unwrap_or(0);

                ws_json.push(serde_json::json!({
                    "id": id_str,
                    "id_num": id_num,
                    "name": workspace_name,
                    "is_active": is_active,
                    "windows": windows_json,
                    "last_focused": last_focused_json,
                    "floating_positions": floating_json,
                }));
            }

            spaces_json.push(serde_json::json!({
                "space": space_num,
                "workspaces": ws_json,
            }));
        }

        let mut mapping = Vec::new();
        for (space_num, window_id, workspace_id) in mapping_intermediate {
            let window_json = if let Some(window_data) = self.create_window_data(window_id) {
                serde_json::to_value(&window_data)
                    .unwrap_or_else(|_| serde_json::json!({ "id": window_id.to_debug_string() }))
            } else {
                serde_json::json!({ "id": window_id.to_debug_string() })
            };

            let _ = included_windows.insert(window_id);

            mapping.push(serde_json::json!({
                "space": space_num,
                "window": window_json,
                "workspace": workspace_id.to_string()
            }));
        }

        let known_managed_windows: Vec<serde_json::Value> = self
            .state
            .windows
            .iter_windows()
            .map(|(wid, _)| wid)
            .filter(|w| !included_windows.contains(w))
            .map(|w| {
                if let Some(window_data) = self.create_window_data(w) {
                    serde_json::to_value(&window_data)
                        .unwrap_or_else(|_| serde_json::json!({ "id": w.to_debug_string() }))
                } else {
                    serde_json::json!({ "id": w.to_debug_string() })
                }
            })
            .collect();

        let reactor_summary = serde_json::json!({
            "apps": self.app_manager.apps.len(),
            "managed_windows": self.state.windows.tracked_window_count(),
            "window_server_info": self.state.windows.window_server_info_count(),
            "visible_window_server_ids": self.state.windows.visible_window_server_count(),
            "screens": self.space_state.screens.len(),
            "known_managed_windows": known_managed_windows,
        });

        let out = serde_json::json!({
            "layout_engine_ron": layout_engine_ron,
            "virtual_workspace_manager": {
                "total_workspaces": stats.total_workspaces,
                "total_windows": stats.total_windows,
                "active_spaces": stats.active_spaces,
                "workspace_window_counts": workspace_window_counts,
            },
            "spaces": spaces_json,
            "window_to_workspace": mapping,
            "reactor": reactor_summary,
        });

        serde_json::to_string_pretty(&out)
    }
}

fn command_entry(
    id: String,
    label: String,
    keywords: impl IntoIterator<Item = String>,
    action: PaletteAction,
) -> PaletteEntry {
    PaletteEntry::new(
        PaletteEntryId::Command(id),
        PaletteEntryKind::Command,
        label,
        "Rift Command".to_string(),
        keywords,
        None,
        action,
    )
    .hidden_when_empty()
}

fn is_auxiliary_bundle(bundle_id: &str, running_bundle_ids: &HashSet<String>) -> bool {
    running_bundle_ids.iter().any(|candidate| {
        candidate.len() < bundle_id.len()
            && bundle_id.starts_with(candidate)
            && bundle_id.as_bytes().get(candidate.len()) == Some(&b'.')
    })
}

fn is_palette_application_eligible(bundle_id: Option<&str>) -> bool {
    bundle_id != Some("com.apple.loginwindow")
}

fn is_palette_window_eligible(state: &crate::model::reactor::WindowState) -> bool {
    // AX applications commonly expose dialogs, status items, popovers, and other
    // implementation-detail windows alongside their real document windows. They
    // can have a WindowServer id while still being impossible to focus as a
    // normal user window, so ids and geometry are not sufficient eligibility
    // signals. Keep unmanaged windows, but require AX's standard-window semantic.
    state.info.is_standard
}
