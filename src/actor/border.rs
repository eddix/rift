use std::sync::Arc;

use objc2::MainThreadMarker;
use tracing::{Span, warn};

use crate::actor;
use crate::common::config::{BorderSettings, Config};
use crate::model::projection::{DesktopSnapshot, StateRevision};
use crate::sys::window_server::WindowServerId;
use crate::ui::border::{BorderStyle, FocusBorderWindow};

pub enum Event {
    Snapshot(Arc<DesktopSnapshot>),
    ConfigUpdated(Box<Config>),
    OrderInvalidated(WindowServerId),
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

const MAX_EVENTS_PER_BATCH: usize = 4096;

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
        while let Some(first) = self.rx.recv().await {
            let mut pending_snapshot = None;
            self.handle_batched_event(first, &mut pending_snapshot);
            for _ in 1..MAX_EVENTS_PER_BATCH {
                let Ok(event) = self.rx.try_recv() else {
                    break;
                };
                self.handle_batched_event(event, &mut pending_snapshot);
            }
            self.flush_pending_snapshot(&mut pending_snapshot);
        }
    }

    fn handle_batched_event(
        &mut self,
        (span, event): (Span, Event),
        pending_snapshot: &mut Option<(Span, Arc<DesktopSnapshot>)>,
    ) {
        match event {
            Event::Snapshot(snapshot) => *pending_snapshot = Some((span, snapshot)),
            Event::ConfigUpdated(config) => {
                self.flush_pending_snapshot(pending_snapshot);
                let _guard = span.enter();
                self.handle_config_updated(*config);
            }
            Event::OrderInvalidated(window) => {
                self.flush_pending_snapshot(pending_snapshot);
                let _guard = span.enter();
                self.handle_order_invalidated(window);
            }
        }
    }

    fn flush_pending_snapshot(
        &mut self,
        pending_snapshot: &mut Option<(Span, Arc<DesktopSnapshot>)>,
    ) {
        let Some((span, snapshot)) = pending_snapshot.take() else {
            return;
        };
        let _guard = span.enter();
        self.handle_snapshot(snapshot);
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

    fn handle_order_invalidated(&mut self, window: WindowServerId) {
        if !self
            .surface
            .as_ref()
            .is_some_and(|surface| surface.should_resync_order_for(window))
        {
            return;
        }
        if let Some(surface) = &self.surface
            && let Err(error) = surface.sync_order()
        {
            warn!(?error, "failed to restore focused-window border ordering");
            self.surface = None;
            self.sync_surface();
        }
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

        if self.surface.as_ref().is_some_and(|surface| !surface.targets(target)) {
            self.surface = match FocusBorderWindow::new(target, style) {
                Ok(surface) => Some(surface),
                Err(error) => {
                    warn!(?error, "failed to replace focused-window border target");
                    None
                }
            };
            return;
        }

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
