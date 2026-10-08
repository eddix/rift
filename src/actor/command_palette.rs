//! Main-thread command-palette lifecycle and action dispatch.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use dispatchr::queue;
use dispatchr::time::Time;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::MainThreadMarker;
use rift_protocol::{DisplaySelector, LayoutCommand, ReactorCommand, WorkspaceSelector};
use tracing::{debug, warn};

use super::reactor::{self, Command as ReactorTopCommand};
use super::wm_controller::{self, WmCmd, WmCommand, WmEvent};
use crate::actor;
use crate::common::config::{CommandPaletteSettings, Config};
use crate::model::command_palette::{
    PaletteAction, PaletteFocusOrigin, PaletteMode, PaletteModel, PaletteMru, PaletteSnapshot,
};
use crate::model::projection::DesktopSnapshot;
use crate::sys::app::NSRunningApplicationExt;
use crate::sys::dispatch::DispatchExt;
use crate::ui::command_palette::{
    CommandPalettePanel, PaletteInput, PalettePanelMode, PaletteRenderRow,
};

#[derive(Debug)]
pub enum Event {
    Toggle,
    ToggleCommands,
    Input(PaletteInput),
    ConfigUpdated(Box<Config>),
    DesktopSnapshot(Arc<DesktopSnapshot>),
    Snapshot {
        generation: u64,
        snapshot: PaletteSnapshot,
    },
    Dispatch(PaletteAction),
    Restore(PaletteFocusOrigin),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Hidden,
    Opening {
        generation: u64,
        mode: PaletteMode,
    },
    Visible {
        generation: u64,
        origin: Option<PaletteFocusOrigin>,
        mode: PaletteMode,
    },
}

impl Session {
    fn accepts_snapshot(self, generation: u64) -> bool {
        matches!(
            self,
            Session::Opening {
                generation: current,
                ..
            }
                | Session::Visible {
                    generation: current,
                    ..
                } if current == generation
        )
    }
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

pub struct CommandPalette {
    settings: CommandPaletteSettings,
    rx: Receiver,
    tx: Sender,
    reactor: reactor::ReactorHandle,
    wm: wm_controller::Sender,
    mtm: MainThreadMarker,
    panel: Option<CommandPalettePanel>,
    model: PaletteModel,
    mru: PaletteMru,
    session: Session,
    generation: u64,
}

impl CommandPalette {
    pub fn new(
        config: Config,
        rx: Receiver,
        tx: Sender,
        reactor: reactor::ReactorHandle,
        wm: wm_controller::Sender,
        mtm: MainThreadMarker,
    ) -> Self {
        let settings = config.settings.ui.command_palette;
        Self {
            settings,
            rx,
            tx,
            reactor,
            wm,
            mtm,
            panel: None,
            model: PaletteModel::new(),
            mru: PaletteMru::default(),
            session: Session::Hidden,
            generation: 0,
        }
    }

    pub async fn run(mut self) {
        if self.settings.enabled {
            self.ensure_panel();
        }
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Toggle => self.toggle(PaletteMode::Windows),
            Event::ToggleCommands => self.toggle(PaletteMode::Commands),
            Event::Input(input) => self.handle_input(input),
            Event::ConfigUpdated(config) => self.update_config(*config),
            Event::DesktopSnapshot(snapshot) => self.handle_desktop_snapshot(snapshot),
            Event::Snapshot { generation, snapshot } => {
                self.handle_palette_snapshot(generation, snapshot)
            }
            Event::Dispatch(action) => self.dispatch_action(action),
            Event::Restore(origin) => self.dispatch_action(focus_origin_action(origin)),
        }
    }

    fn ensure_panel(&mut self) {
        if self.panel.is_some() {
            return;
        }
        let sender = self.tx.clone();
        self.panel = Some(CommandPalettePanel::new(
            self.mtm,
            self.settings.width,
            self.settings.max_results,
            Rc::new(move |input| sender.send(Event::Input(input))),
        ));
    }

    fn toggle(&mut self, mode: PaletteMode) {
        if matches!(self.session, Session::Hidden) {
            self.show(mode);
        } else {
            self.cancel();
        }
    }

    fn show(&mut self, mode: PaletteMode) {
        if !self.settings.enabled {
            return;
        }
        let started = Instant::now();
        self.ensure_panel();
        self.generation = self.generation.wrapping_add(1);
        self.session = Session::Opening {
            generation: self.generation,
            mode,
        };
        self.reactor
            .send(reactor::Event::CommandPaletteSnapshotRequested { generation: self.generation });
        debug!(
            elapsed_us = started.elapsed().as_micros(),
            "command palette requested snapshot"
        );
    }

    fn handle_input(&mut self, input: PaletteInput) {
        if !matches!(self.session, Session::Visible { .. }) {
            return;
        }
        match input {
            PaletteInput::QueryChanged(query) => {
                let started = Instant::now();
                self.model.set_query(query, &self.mru);
                self.render();
                debug!(
                    query = self.model.query(),
                    selected = self.model.selected_entry().map(|entry| entry.primary.as_str()),
                    elapsed_us = started.elapsed().as_micros(),
                    "command palette filtered"
                );
            }
            PaletteInput::MoveSelection(delta) => {
                self.model.move_selection(delta);
                self.render();
            }
            PaletteInput::Execute => self.execute_selection(),
            PaletteInput::ExpandApplication => {
                if self.model.expand_selected_application(&self.mru) {
                    if let Some(panel) = self.panel.as_ref() {
                        panel.set_query(self.model.query());
                    }
                    self.render();
                }
            }
            PaletteInput::LeaveApplication => {
                if self.model.leave_application(&self.mru) {
                    if let Some(panel) = self.panel.as_ref() {
                        panel.set_query(self.model.query());
                    }
                    self.render();
                }
            }
            PaletteInput::Cancel => self.cancel(),
            PaletteInput::ClickResult(index) => {
                if self.model.select_index(index) {
                    self.execute_selection();
                }
            }
        }
    }

    fn execute_selection(&mut self) {
        let Some(entry) = self.model.selected_entry() else {
            return;
        };
        let action = match entry.action {
            PaletteAction::ActivateApplication(pid) => self
                .model
                .preferred_window_for_application(pid, &self.mru)
                .unwrap_or_else(|| entry.action.clone()),
            _ => entry.action.clone(),
        };
        if let PaletteAction::FocusWindow { window_id, .. } = action {
            self.mru.record_window(window_id);
        }
        debug!(?action, "command palette executing selection");
        self.session = Session::Hidden;
        if let Some(panel) = self.panel.as_ref() {
            panel.hide();
        }
        self.schedule(Event::Dispatch(action));
    }

    fn cancel(&mut self) {
        let origin = match std::mem::replace(&mut self.session, Session::Hidden) {
            Session::Hidden => return,
            Session::Opening { .. } => None,
            Session::Visible { origin, .. } => origin,
        };
        if let Some(panel) = self.panel.as_ref() {
            panel.hide();
        }
        if let Some(origin) = origin {
            self.schedule(Event::Restore(origin));
        }
    }

    fn render(&self) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        let rows = self
            .model
            .results()
            .map(|entry| PaletteRenderRow {
                primary: entry.primary.clone(),
                secondary: entry.secondary.clone(),
                kind: entry.kind,
                app_pid: entry.app_pid,
            })
            .collect();
        panel.render(rows, self.model.selected_index());
    }

    fn update_config(&mut self, config: Config) {
        let settings = config.settings.ui.command_palette;
        let recreate = settings.width != self.settings.width
            || settings.max_results != self.settings.max_results;
        if !settings.enabled && !matches!(self.session, Session::Hidden) {
            self.cancel();
        }
        self.settings = settings;
        if recreate && let Some(panel) = self.panel.take() {
            panel.hide();
        }
        if settings.enabled {
            self.ensure_panel();
        }
    }

    fn handle_desktop_snapshot(&mut self, snapshot: Arc<DesktopSnapshot>) {
        if let Some(window) = snapshot.state.focused_window {
            self.mru.record_window(window);
        }
        if let Session::Visible { generation, .. } = self.session {
            self.reactor
                .send(reactor::Event::CommandPaletteSnapshotRequested { generation });
        }
    }

    fn handle_palette_snapshot(&mut self, generation: u64, snapshot: PaletteSnapshot) {
        if !self.session.accepts_snapshot(generation) {
            return;
        }
        match self.session {
            Session::Hidden => {}
            Session::Opening { mode, .. } => {
                let origin = snapshot.focus_origin;
                self.model.begin_session(snapshot, &self.mru, mode);
                self.session = Session::Visible { generation, origin, mode };
                self.render();
                if let Some(panel) = self.panel.as_ref()
                    && !panel.show(
                        self.model.target_display_id(),
                        self.model.query(),
                        panel_mode(mode),
                    )
                {
                    warn!("command palette failed to become key window");
                }
            }
            Session::Visible { .. } => {
                self.model.set_snapshot(snapshot, &self.mru);
                self.render();
            }
        }
    }

    fn schedule(&self, event: Event) {
        queue::main().after_f_s(Time::NOW, (self.tx.clone(), event), |(sender, event)| {
            sender.send(event)
        });
    }

    fn dispatch_action(&self, action: PaletteAction) {
        match action {
            PaletteAction::FocusWindow { window_id, window_server_id } => {
                let _ = self.reactor.try_send(reactor::Event::Command(ReactorTopCommand::Reactor(
                    ReactorCommand::FocusWindow {
                        window_id: window_id.into(),
                        window_server_id: window_server_id.map(|window| window.as_u32()),
                    },
                )));
            }
            PaletteAction::ActivateApplication(pid) => {
                let Some(app) = NSRunningApplication::with_process_id(pid) else {
                    warn!(pid, "command palette application disappeared before activation");
                    return;
                };
                let _ = app.activateWithOptions(NSApplicationActivationOptions::empty());
            }
            PaletteAction::SwitchWorkspace(index) => {
                self.send_layout(LayoutCommand::SwitchToWorkspace(index));
            }
            PaletteAction::MoveWindowToWorkspace(index) => {
                self.send_layout(LayoutCommand::MoveWindowToWorkspace {
                    workspace: WorkspaceSelector::Index(index),
                    follow: true,
                    window_id: None,
                });
            }
            PaletteAction::NextWorkspace => self.send_layout(LayoutCommand::NextWorkspace(None)),
            PaletteAction::PreviousWorkspace => {
                self.send_layout(LayoutCommand::PrevWorkspace(None));
            }
            PaletteAction::LastWorkspace => {
                self.send_layout(LayoutCommand::SwitchToLastWorkspace);
            }
            PaletteAction::ToggleFloating => {
                self.send_layout(LayoutCommand::ToggleWindowFloating);
            }
            PaletteAction::ToggleFullscreen => {
                self.send_layout(LayoutCommand::ToggleFullscreen);
            }
            PaletteAction::ToggleFullscreenWithinGaps => {
                self.send_layout(LayoutCommand::ToggleFullscreenWithinGaps);
            }
            PaletteAction::SetLayout(mode) => {
                self.send_layout(LayoutCommand::SetWorkspaceLayout { workspace: None, mode });
            }
            PaletteAction::FocusDisplay(uuid) => {
                let _ = self.reactor.try_send(reactor::Event::Command(ReactorTopCommand::Reactor(
                    ReactorCommand::FocusDisplay(DisplaySelector::Uuid(uuid)),
                )));
            }
            PaletteAction::ReloadConfig => {
                self.wm.send(WmEvent::Command(WmCommand::Wm(WmCmd::ReloadConfig)))
            }
        }
    }

    fn send_layout(&self, command: LayoutCommand) {
        let _ = self
            .reactor
            .try_send(reactor::Event::Command(ReactorTopCommand::Layout(command)));
    }
}

fn panel_mode(mode: PaletteMode) -> PalettePanelMode {
    match mode {
        PaletteMode::Windows => PalettePanelMode::Windows,
        PaletteMode::Commands => PalettePanelMode::Commands,
    }
}

fn focus_origin_action(origin: PaletteFocusOrigin) -> PaletteAction {
    if let Some(window_id) = origin.window_id {
        PaletteAction::FocusWindow {
            window_id,
            window_server_id: origin.window_server_id,
        }
    } else {
        PaletteAction::ActivateApplication(origin.app_pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::app::WindowId;

    #[test]
    fn focus_origin_action_should_restore_exact_window_when_available() {
        let window = WindowId::new(5, 9);

        assert_eq!(
            focus_origin_action(PaletteFocusOrigin {
                app_pid: window.pid,
                window_id: Some(window),
                window_server_id: None,
            }),
            PaletteAction::FocusWindow {
                window_id: window,
                window_server_id: None,
            }
        );
    }

    #[test]
    fn focus_origin_action_should_restore_app_when_window_is_unavailable() {
        assert_eq!(
            focus_origin_action(PaletteFocusOrigin {
                app_pid: 7,
                window_id: None,
                window_server_id: None,
            }),
            PaletteAction::ActivateApplication(7)
        );
    }

    #[test]
    fn session_should_reject_stale_snapshot_generation() {
        assert!(
            !Session::Opening {
                generation: 9,
                mode: PaletteMode::Commands,
            }
            .accepts_snapshot(8)
        );
    }

    #[test]
    fn command_mode_should_map_to_the_commands_panel_header() {
        assert_eq!(panel_mode(PaletteMode::Commands), PalettePanelMode::Commands);
    }
}
