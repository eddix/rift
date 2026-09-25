use std::sync::Arc;

use objc2::MainThreadMarker;
use objc2_core_foundation::CGRect;
use parking_lot::Mutex;
use tracing::{Span, warn};

use crate::actor;
use crate::common::collections::HashMap;
use crate::common::config::{BorderSettings, Config};
use crate::model::projection::{BorderTarget, DesktopSnapshot, StateRevision};
use crate::model::server::RuntimeDisplayData;
use crate::sys::cgs_window::{CgsWindowError, CgsWindowMotionHandle};
use crate::sys::geometry::CGRectExt;
use crate::sys::window_server::WindowServerId;
use crate::ui::border::{BorderStyle, FocusBorderWindow, border_frame, border_origin};

pub enum Event {
    Snapshot(Arc<DesktopSnapshot>),
    ConfigUpdated(Box<Config>),
    OrderInvalidated(WindowServerId),
    FrameChanged(WindowServerId, CGRect),
}

pub struct Border {
    settings: BorderSettings,
    rx: Receiver,
    motion: BorderMotionHandle,
    _mtm: MainThreadMarker,
    surface: Option<FocusBorderWindow>,
    last_snapshot: Option<Arc<DesktopSnapshot>>,
    last_applied_revision: Option<StateRevision>,
    live_frame: Option<(WindowServerId, CGRect)>,
}

pub type Sender = actor::Sender<Event>;
pub type Receiver = actor::Receiver<Event>;

const MAX_EVENTS_PER_BATCH: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct MotionTarget {
    target: WindowServerId,
    surface: CgsWindowMotionHandle,
    border_width: f64,
}

#[derive(Debug, Clone, Default)]
pub struct BorderMotionHandle {
    target: Arc<Mutex<Option<MotionTarget>>>,
}

impl BorderMotionHandle {
    pub(crate) fn move_target(
        &self,
        window: WindowServerId,
        frame: CGRect,
    ) -> Result<bool, CgsWindowError> {
        let target = self.target.lock();
        let Some(target) = *target else {
            return Ok(false);
        };
        if target.target != window {
            return Ok(false);
        }
        let origin = border_origin(frame, target.border_width);
        target.surface.move_with_group(origin)?;
        Ok(true)
    }

    fn register(&self, target: WindowServerId, surface: CgsWindowMotionHandle, border_width: f64) {
        *self.target.lock() = Some(MotionTarget { target, surface, border_width });
    }

    fn clear(&self) { *self.target.lock() = None; }
}

impl Border {
    pub fn new(
        config: Config,
        rx: Receiver,
        motion: BorderMotionHandle,
        mtm: MainThreadMarker,
    ) -> Self {
        Self {
            settings: config.settings.ui.border,
            rx,
            motion,
            _mtm: mtm,
            surface: None,
            last_snapshot: None,
            last_applied_revision: None,
            live_frame: None,
        }
    }

    pub async fn run(mut self) {
        while let Some(first) = self.rx.recv().await {
            let mut pending_snapshot = None;
            let mut pending_frames = HashMap::default();
            self.handle_batched_event(first, &mut pending_snapshot, &mut pending_frames);
            for _ in 1..MAX_EVENTS_PER_BATCH {
                let Ok(event) = self.rx.try_recv() else {
                    break;
                };
                self.handle_batched_event(event, &mut pending_snapshot, &mut pending_frames);
            }
            self.flush_pending_updates(&mut pending_snapshot, &mut pending_frames);
        }
    }

    fn handle_batched_event(
        &mut self,
        (span, event): (Span, Event),
        pending_snapshot: &mut Option<(Span, Arc<DesktopSnapshot>)>,
        pending_frames: &mut HashMap<WindowServerId, (Span, CGRect)>,
    ) {
        match event {
            Event::Snapshot(snapshot) => *pending_snapshot = Some((span, snapshot)),
            Event::FrameChanged(window, frame) => {
                _ = pending_frames.insert(window, (span, frame));
            }
            Event::ConfigUpdated(config) => {
                self.flush_pending_updates(pending_snapshot, pending_frames);
                let _guard = span.enter();
                self.handle_config_updated(*config);
            }
            Event::OrderInvalidated(window) => {
                self.flush_pending_updates(pending_snapshot, pending_frames);
                let _guard = span.enter();
                self.handle_order_invalidated(window);
            }
        }
    }

    fn flush_pending_updates(
        &mut self,
        pending_snapshot: &mut Option<(Span, Arc<DesktopSnapshot>)>,
        pending_frames: &mut HashMap<WindowServerId, (Span, CGRect)>,
    ) {
        self.flush_pending_snapshot(pending_snapshot);
        let target = self
            .last_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.state.border_target)
            .map(|target| target.window_server_id);
        if let Some(target) = target
            && let Some((span, frame)) = pending_frames.remove(&target)
        {
            let _guard = span.enter();
            self.handle_frame_changed(target, frame);
        }
        pending_frames.clear();
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
        let snapshot_target = snapshot.state.border_target.map(|target| target.window_server_id);
        if self.live_frame.map(|(window, _)| window) != snapshot_target {
            self.live_frame = None;
        }
        self.last_snapshot = Some(snapshot);
        self.sync_surface();
    }

    fn handle_frame_changed(&mut self, window: WindowServerId, frame: CGRect) {
        let targets_window = self
            .last_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.state.border_target)
            .is_some_and(|target| target.window_server_id == window);
        if !targets_window {
            return;
        }
        self.live_frame = Some((window, frame));
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
        self.motion.clear();
        if let Some(surface) = &self.surface
            && let Err(error) = surface.sync_order()
        {
            warn!(?error, "failed to restore focused-window border ordering");
            self.clear_surface();
            self.sync_surface();
            return;
        }
        self.sync_surface();
    }

    fn clear_surface(&mut self) {
        self.motion.clear();
        self.surface = None;
    }

    fn install_surface(
        &mut self,
        surface: FocusBorderWindow,
        target: BorderTarget,
        style: BorderStyle,
    ) {
        let motion_handle = surface.motion_handle();
        self.surface = Some(surface);
        self.motion.register(target.window_server_id, motion_handle, style.width);
    }

    fn sync_surface(&mut self) {
        let target = self.last_snapshot.as_ref().and_then(|snapshot| {
            (!snapshot.state.mission_control_active)
                .then_some(snapshot.state.border_target)
                .flatten()
        });
        let target = target.map(|target| target_with_live_frame(target, self.live_frame));
        let displays = self
            .last_snapshot
            .as_ref()
            .map(|snapshot| snapshot.state.displays.as_slice())
            .unwrap_or_default();
        let Some(target) = target.filter(|target| {
            self.settings.enabled && border_fits_any_display(*target, self.settings.width, displays)
        }) else {
            self.clear_surface();
            return;
        };
        let style = BorderStyle::for_target(self.settings, target);

        if self.surface.as_ref().is_some_and(|surface| !surface.targets(target)) {
            match FocusBorderWindow::new(target, style) {
                Ok(surface) => {
                    self.clear_surface();
                    self.install_surface(surface, target, style);
                }
                Err(error) => {
                    warn!(?error, "failed to replace focused-window border target");
                    self.clear_surface();
                }
            }
            return;
        }

        if let Some(surface) = &mut self.surface {
            self.motion.clear();
            if let Err(error) = surface.update(target, style) {
                warn!(?error, "failed to update focused-window border");
                self.clear_surface();
            } else {
                self.motion
                    .register(target.window_server_id, surface.motion_handle(), style.width);
            }
            return;
        }

        match FocusBorderWindow::new(target, style) {
            Ok(surface) => self.install_surface(surface, target, style),
            Err(error) => warn!(?error, "failed to create focused-window border"),
        }
    }
}

impl Drop for Border {
    fn drop(&mut self) { self.clear_surface(); }
}

fn target_with_live_frame(
    mut target: BorderTarget,
    live_frame: Option<(WindowServerId, CGRect)>,
) -> BorderTarget {
    if let Some((window, frame)) = live_frame
        && window == target.window_server_id
    {
        target.frame = frame;
    }
    target
}

fn border_fits_any_display(
    target: BorderTarget,
    width: f64,
    displays: &[RuntimeDisplayData],
) -> bool {
    border_fits_visible_frames(
        target.frame,
        width,
        displays.iter().map(|display| display.info.frame),
    )
}

fn border_fits_visible_frames(
    target: CGRect,
    width: f64,
    visible_frames: impl Iterator<Item = CGRect>,
) -> bool {
    let frame = border_frame(target, width);
    let mut visible_frames = visible_frames.peekable();
    visible_frames.peek().is_none()
        || visible_frames.any(|visible_frame| visible_frame.contains_rect(frame))
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;
    use crate::actor::app::WindowId;
    use crate::sys::screen::SpaceId;

    fn target(window_server_id: u32, frame: CGRect) -> BorderTarget {
        BorderTarget {
            window: WindowId::new(1, 1),
            window_server_id: WindowServerId::new(window_server_id),
            space: SpaceId::new(1),
            frame,
            corner_radius: None,
        }
    }

    #[test]
    fn live_frame_should_override_a_matching_snapshot_target() {
        let snapshot = CGRect::new(CGPoint::ZERO, CGSize::new(800.0, 600.0));
        let live = CGRect::new(CGPoint::new(120.0, 80.0), CGSize::new(800.0, 600.0));

        let result =
            target_with_live_frame(target(7, snapshot), Some((WindowServerId::new(7), live)));

        assert_eq!(result.frame, live);
    }

    #[test]
    fn live_frame_should_not_override_a_different_snapshot_target() {
        let snapshot = CGRect::new(CGPoint::ZERO, CGSize::new(800.0, 600.0));
        let live = CGRect::new(CGPoint::new(120.0, 80.0), CGSize::new(800.0, 600.0));

        let result =
            target_with_live_frame(target(7, snapshot), Some((WindowServerId::new(8), live)));

        assert_eq!(result.frame, snapshot);
    }

    #[test]
    fn empty_motion_handle_should_ignore_window_frames() {
        let frame = CGRect::new(CGPoint::ZERO, CGSize::new(800.0, 600.0));

        assert!(
            matches!(
                BorderMotionHandle::default().move_target(WindowServerId::new(7), frame),
                Ok(false)
            ),
            "an unregistered motion handle must not issue a WindowServer transaction"
        );
    }

    #[test]
    fn border_should_fit_when_outer_frame_stays_inside_visible_display() {
        let display = CGRect::new(CGPoint::ZERO, CGSize::new(1000.0, 800.0));
        let window = CGRect::new(CGPoint::new(8.0, 8.0), CGSize::new(984.0, 784.0));

        assert!(border_fits_visible_frames(window, 3.0, [display].into_iter()));
    }

    #[test]
    fn border_should_not_fit_when_window_touches_visible_display_edge() {
        let display = CGRect::new(CGPoint::ZERO, CGSize::new(1000.0, 800.0));
        let maximized_window = display;

        assert!(!border_fits_visible_frames(
            maximized_window,
            3.0,
            [display].into_iter()
        ));
    }
}
