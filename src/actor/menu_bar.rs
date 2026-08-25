use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use objc2::MainThreadMarker;
use tokio::sync::mpsc::UnboundedSender;

use crate::actor::{config, reactor};
use crate::common::config::{Config, ConfigCommand};
use crate::layout_engine::LayoutCommand;
use crate::model::projection::{DesktopSnapshot, StateRevision};
use crate::ui::menu_bar::{MenuAction, MenuIcon};
use crate::{actor, common};

pub enum Event {
    Snapshot(Arc<DesktopSnapshot>),
    ConfigUpdated(Config),
}

enum DebounceCommand {
    Arm,
    Shutdown,
}

pub struct Menu {
    config: Config,
    rx: Receiver,
    reactor_tx: reactor::Sender,
    config_tx: config::Sender,
    action_tx: UnboundedSender<MenuAction>,
    action_rx: tokio::sync::mpsc::UnboundedReceiver<MenuAction>,
    icon: Option<MenuIcon>,
    mtm: MainThreadMarker,
    last_snapshot: Option<Arc<DesktopSnapshot>>,
    last_applied_revision: Option<StateRevision>,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

impl Menu {
    pub fn new(
        config: Config,
        rx: Receiver,
        reactor_tx: reactor::Sender,
        config_tx: config::Sender,
        mtm: MainThreadMarker,
    ) -> Self {
        let (action_tx, action_rx) = tokio::sync::mpsc::unbounded_channel();
        let layout_folder = config.settings.ui.menu_bar.resolved_layout_folder();
        let mut icon = config
            .settings
            .ui
            .menu_bar
            .enabled
            .then(|| MenuIcon::new(mtm, action_tx.clone(), &layout_folder));
        if let Some(icon) = &mut icon {
            icon.update_config(&config.settings.ui.menu_bar, &config.keys);
        }
        Self {
            icon,
            config,
            rx,
            reactor_tx,
            config_tx,
            action_tx,
            action_rx,
            mtm,
            last_snapshot: None,
            last_applied_revision: None,
        }
    }

    pub async fn run(mut self) {
        const DEBOUNCE: Duration = Duration::from_millis(150);

        let mut pending: Option<Event> = None;
        let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let debounce_tx = Self::spawn_debouncer(DEBOUNCE, tick_tx);

        loop {
            tokio::select! {
                maybe_tick = tick_rx.recv() => {
                    if maybe_tick.is_none() {
                        if let Some(ev) = pending.take() {
                            self.handle_event(ev);
                        }
                        break;
                    }

                    if let Some(ev) = pending.take() {
                        self.handle_event(ev);
                    }
                }

                maybe = self.rx.recv() => {
                    match maybe {
                        Some((span, event)) => {
                            let _enter = span.enter();
                            match event {
                                Event::Snapshot(_) => {
                                    pending = Some(event);
                                    let _ = debounce_tx.send(DebounceCommand::Arm);
                                }
                                Event::ConfigUpdated(cfg) => self.handle_config_updated(cfg),
                            }
                        }
                        None => {
                            let _ = debounce_tx.send(DebounceCommand::Shutdown);
                            if let Some(ev) = pending.take() {
                                self.handle_event(ev);
                            }
                            break;
                        }
                    }
                }

                maybe_action = self.action_rx.recv() => {
                    if let Some(action) = maybe_action {
                        self.handle_action(action);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Snapshot(snapshot) => self.handle_snapshot(snapshot),
            Event::ConfigUpdated(cfg) => self.handle_config_updated(cfg),
        }
    }

    fn handle_snapshot(&mut self, snapshot: Arc<DesktopSnapshot>) {
        if self.last_applied_revision.is_some_and(|revision| revision >= snapshot.revision) {
            return;
        }
        self.last_applied_revision = Some(snapshot.revision);

        let Some(context) = snapshot.state.menu_bar_context.as_ref() else {
            self.last_snapshot = Some(snapshot);
            return;
        };
        let Some(icon) = &mut self.icon else {
            self.last_snapshot = Some(snapshot);
            return;
        };

        icon.sync_workspace_topology(&context.workspaces, &self.config.keys);
        icon.update_menu_state(context.active_space_is_activated, &context.workspaces);
        icon.update_status_icon(&context.workspaces, &self.config.settings.ui.menu_bar);
        self.last_snapshot = Some(snapshot);
    }

    fn handle_config_updated(&mut self, new_config: Config) {
        let should_enable = new_config.settings.ui.menu_bar.enabled;

        self.config = new_config;

        if should_enable && self.icon.is_none() {
            let layout_folder = self.config.settings.ui.menu_bar.resolved_layout_folder();
            self.icon = Some(MenuIcon::new(self.mtm, self.action_tx.clone(), &layout_folder));
        } else if !should_enable && self.icon.is_some() {
            self.icon = None;
        }

        if let Some(icon) = &mut self.icon {
            icon.update_config(&self.config.settings.ui.menu_bar, &self.config.keys);
        }

        self.last_applied_revision = None;
        if let Some(snapshot) = self.last_snapshot.take() {
            self.handle_snapshot(snapshot);
        }
    }

    fn handle_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::SetLayout(mode) => self
                .send_layout_command(LayoutCommand::SetWorkspaceLayout { workspace: None, mode }),
            MenuAction::NextWorkspace => {
                self.send_layout_command(LayoutCommand::NextWorkspace(None));
            }
            MenuAction::PrevWorkspace => {
                self.send_layout_command(LayoutCommand::PrevWorkspace(None));
            }
            MenuAction::SwitchToWorkspace(workspace) => {
                self.send_layout_command(LayoutCommand::SwitchToWorkspace(workspace));
            }
            MenuAction::RestoreLayout { path, scope, source } => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::RestoreLayout { path, scope, source },
                )));
            }
            MenuAction::RestoreMasterFile(scope) => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::RestoreLayout {
                        path: common::config::restore_file(),
                        scope,
                        source: crate::layout_engine::RestoreSource::CurrentSpace,
                    },
                )));
            }
            MenuAction::SaveLayout(path) => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::SaveLayout { path },
                )));
                let action_tx = self.action_tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(150));
                    let _ = action_tx.send(MenuAction::RefreshLayoutFiles);
                });
            }
            MenuAction::SaveMasterFile => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::SaveLayout {
                        path: common::config::restore_file(),
                    },
                )));
            }
            MenuAction::ToggleSpaceActivated => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::ToggleSpaceActivated,
                )));
            }
            MenuAction::OpenGitHub => {
                Self::open_path_or_url("https://github.com/acsandmann/rift");
            }
            MenuAction::OpenDocumentation => {
                Self::open_path_or_url("https://github.com/acsandmann/rift#readme");
            }
            MenuAction::OpenMatrix => {
                Self::open_path_or_url("https://matrix.to/#/#rift:matrix.org");
            }
            MenuAction::OpenSponsor => {
                Self::open_path_or_url("https://github.com/sponsors/acsandmann");
            }
            MenuAction::OpenConfig => {
                Self::open_path_or_url(common::config::config_file());
            }
            MenuAction::ReloadConfig => self.reload_config(),
            MenuAction::RefreshLayoutFiles => {
                if let Some(icon) = &mut self.icon {
                    icon.refresh_layout_library();
                }
            }
            MenuAction::QuitRift => {
                self.reactor_tx.send(reactor::Event::Command(reactor::Command::Reactor(
                    reactor::ReactorCommand::SaveAndExit,
                )));
            }
        }
    }

    fn send_layout_command(&self, command: LayoutCommand) {
        self.reactor_tx.send(reactor::Event::Command(reactor::Command::Layout(command)));
    }

    fn open_path_or_url(target: impl AsRef<Path>) {
        let _ = ProcessCommand::new("open").arg(target.as_ref()).spawn();
    }

    fn reload_config(&self) {
        let (response, _fut) = r#continue::continuation();
        let msg = config::Event::ApplyConfig {
            cmd: ConfigCommand::ReloadConfig,
            response,
        };
        if let Err(e) = self.config_tx.try_send(msg) {
            let tokio::sync::mpsc::error::SendError((_span, msg)) = e;
            match msg {
                config::Event::ApplyConfig { response, .. } => std::mem::forget(response),
                config::Event::QueryConfig(response) => std::mem::forget(response),
            }
        }
    }

    fn spawn_debouncer(
        period: Duration,
        tick_tx: UnboundedSender<()>,
    ) -> mpsc::Sender<DebounceCommand> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<DebounceCommand>();

        std::thread::spawn(move || {
            loop {
                match cmd_rx.recv() {
                    Ok(DebounceCommand::Arm) => {
                        let deadline = Instant::now() + period;
                        loop {
                            let now = Instant::now();
                            if now >= deadline {
                                if tick_tx.send(()).is_err() {
                                    return;
                                }
                                break;
                            }
                            match cmd_rx.recv_timeout(deadline.saturating_duration_since(now)) {
                                Ok(DebounceCommand::Arm) => continue,
                                Ok(DebounceCommand::Shutdown)
                                | Err(RecvTimeoutError::Disconnected) => {
                                    return;
                                }
                                Err(RecvTimeoutError::Timeout) => {
                                    if tick_tx.send(()).is_err() {
                                        return;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    Ok(DebounceCommand::Shutdown) | Err(_) => return,
                }
            }
        });

        cmd_tx
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{DebounceCommand, Menu};

    #[test]
    fn debouncer_emits_within_one_period_during_continuous_updates() {
        let period = Duration::from_millis(40);
        let spam_duration = Duration::from_millis(250);
        let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel();
        let debounce_tx = Menu::spawn_debouncer(period, tick_tx);
        let spam_tx = debounce_tx.clone();
        let spammer = thread::spawn(move || {
            let deadline = Instant::now() + spam_duration;
            while Instant::now() < deadline {
                if spam_tx.send(DebounceCommand::Arm).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        });

        let started = Instant::now();
        debounce_tx.send(DebounceCommand::Arm).unwrap();
        tick_rx.blocking_recv().expect("debouncer should emit a tick");
        let elapsed = started.elapsed();
        let _ = debounce_tx.send(DebounceCommand::Shutdown);
        spammer.join().unwrap();

        assert!(
            elapsed < Duration::from_millis(200),
            "continuous updates starved the menu refresh for {elapsed:?}"
        );
    }
}
