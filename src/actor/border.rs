use std::sync::Arc;

use objc2::MainThreadMarker;
use tracing::warn;

use crate::actor;
use crate::common::config::{BorderSettings, Config};
use crate::model::projection::{DesktopSnapshot, StateRevision};
use crate::ui::border::{BorderStyle, FocusBorderWindow};

pub enum Event {
    Snapshot(Arc<DesktopSnapshot>),
    ConfigUpdated(Config),
}

pub struct Border {
    settings: BorderSettings,
    rx: Receiver,
    _mtm: MainThreadMarker,
    surface: Option<FocusBorderWindow>,
    last_snapshot: Option<Arc<DesktopSnapshot>>,
    last_applied_revision: Option<StateRevision>,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

impl Border {
    pub fn new(config: Config, rx: Receiver, mtm: MainThreadMarker) -> Self {
        Self {
            settings: config.settings.ui.border,
            rx,
            _mtm: mtm,
            surface: None,
            last_snapshot: None,
            last_applied_revision: None,
        }
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.rx.recv().await {
            let _guard = span.enter();
            match event {
                Event::Snapshot(snapshot) => self.handle_snapshot(snapshot),
                Event::ConfigUpdated(config) => self.handle_config_updated(config),
            }
        }
    }

    fn handle_snapshot(&mut self, snapshot: Arc<DesktopSnapshot>) {
        if self.last_applied_revision.is_some_and(|revision| revision >= snapshot.revision) {
            return;
        }
        self.last_applied_revision = Some(snapshot.revision);
        self.last_snapshot = Some(snapshot);
        self.sync_surface();
    }

    fn handle_config_updated(&mut self, config: Config) {
        self.settings = config.settings.ui.border;
        self.sync_surface();
    }

    fn sync_surface(&mut self) {
        let target = self.last_snapshot.as_ref().and_then(|snapshot| {
            (!snapshot.state.mission_control_active)
                .then_some(snapshot.state.border_target)
                .flatten()
        });
        let Some(target) = target.filter(|_| self.settings.enabled) else {
            self.surface = None;
            return;
        };
        let style = BorderStyle::from(self.settings);

        if let Some(surface) = &mut self.surface {
            if let Err(error) = surface.update(target, style) {
                warn!(?error, "failed to update focused-window border");
                self.surface = None;
            }
            return;
        }

        match FocusBorderWindow::new(target, style) {
            Ok(surface) => self.surface = Some(surface),
            Err(error) => warn!(?error, "failed to create focused-window border"),
        }
    }
}
