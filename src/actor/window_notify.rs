use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use tracing::{debug, trace};

use super::reactor::{self, Event};
use super::{border, spaces};
use crate::common::collections::{HashMap, HashSet};
use crate::sys::screen::SpaceId;
use crate::sys::skylight::{CGSEventType, KnownCGSEvent};
use crate::sys::window_notify;
use crate::sys::window_server::{self, WindowServerId};

#[derive(Default)]
pub struct Ignored {
    by_event: HashMap<u32, Arc<HashSet<u32>>>,
}

impl Ignored {
    pub fn empty() -> Self { Self { by_event: HashMap::default() } }

    #[inline]
    pub fn is_ignored(&self, event: CGSEventType, wsid: u32) -> bool {
        self.by_event.get(&event.into()).map_or(false, |set| set.contains(&wsid))
    }

    pub fn with_added(&self, event: CGSEventType, wsid: u32) -> Arc<Ignored> {
        let code = event.into();
        if self.is_ignored(event, wsid) {
            return Arc::new(self.clone());
        }
        let mut next_map = self.by_event.clone();
        let mut next_set = next_map.get(&code).map(|s| (**s).clone()).unwrap_or_default();
        next_set.insert(wsid);
        next_map.insert(code, Arc::new(next_set));
        Arc::new(Ignored { by_event: next_map })
    }

    pub fn with_removed(&self, event: CGSEventType, wsid: u32) -> Arc<Ignored> {
        let code = event.into();
        let Some(set_arc) = self.by_event.get(&code) else {
            return Arc::new(self.clone());
        };
        if !set_arc.contains(&wsid) {
            return Arc::new(self.clone());
        }
        let mut next_map = self.by_event.clone();
        let mut next_set = (**set_arc).clone();
        next_set.remove(&wsid);
        if next_set.is_empty() {
            next_map.remove(&code);
        } else {
            next_map.insert(code, Arc::new(next_set));
        }
        Arc::new(Ignored { by_event: next_map })
    }
}

impl Clone for Ignored {
    fn clone(&self) -> Self {
        Self {
            by_event: self.by_event.clone(),
        }
    }
}

#[derive(Debug)]
pub enum Request {
    Subscribe(CGSEventType),
    UpdateWindowNotifications(Vec<u32>),
    Stop,
}

pub type Sender = crate::actor::Sender<Request>;
pub type Receiver = crate::actor::Receiver<Request>;

const FOCUS_WAKE_DEBOUNCE: Duration = Duration::from_millis(1);

fn is_border_order_event(event: CGSEventType) -> bool {
    matches!(
        event,
        CGSEventType::Known(KnownCGSEvent::WindowReordered)
            | CGSEventType::Known(KnownCGSEvent::WindowLevelChanged)
            | CGSEventType::Known(KnownCGSEvent::WindowManagerActivatingClickOrdering)
            | CGSEventType::Known(KnownCGSEvent::WindowOrderingGroupChanged)
            | CGSEventType::Known(KnownCGSEvent::WindowParentChanged)
    )
}

fn is_border_frame_event(event: CGSEventType) -> bool {
    matches!(
        event,
        CGSEventType::Known(KnownCGSEvent::WindowMoved)
            | CGSEventType::Known(KnownCGSEvent::WindowResized)
    )
}

#[derive(Clone)]
struct FocusWakeSender {
    wake: mpsc::SyncSender<()>,
}

impl FocusWakeSender {
    fn notify(&self) { let _ = self.wake.try_send(()); }
}

pub struct WindowNotify {
    events_tx: reactor::Sender,
    spaces_tx: spaces::Sender,
    requests_rx: Option<Receiver>,
    subscribed: HashSet<CGSEventType>,
    initial_events: Vec<CGSEventType>,
    focus_wake: FocusWakeSender,
    border_tx: border::Sender,
    border_motion: border::BorderMotionHandle,
}

impl WindowNotify {
    pub fn new(
        events_tx: reactor::Sender,
        spaces_tx: spaces::Sender,
        requests_rx: Receiver,
        initial_events: &[CGSEventType],
        border_tx: border::Sender,
        border_motion: border::BorderMotionHandle,
    ) -> Self {
        let (focus_wake_tx, focus_wake_rx) = mpsc::sync_channel(1);
        Self::spawn_focus_resolver(events_tx.clone(), focus_wake_rx);
        Self {
            events_tx,
            spaces_tx,
            requests_rx: Some(requests_rx),
            subscribed: HashSet::default(),
            initial_events: initial_events.iter().copied().collect(),
            focus_wake: FocusWakeSender { wake: focus_wake_tx },
            border_tx,
            border_motion,
        }
    }

    pub async fn run(mut self) {
        let mut requests_rx = match self.requests_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        for event in self.initial_events.drain(..) {
            match Self::subscribe(
                event,
                self.events_tx.clone(),
                self.spaces_tx.clone(),
                self.focus_wake.clone(),
                self.border_tx.clone(),
                self.border_motion.clone(),
            ) {
                Ok(()) => {
                    self.subscribed.insert(event);
                    debug!("initial subscription succeeded for event {}", event);
                }
                Err(code) => {
                    debug!("initial subscribe for {} failed (res={})", event, code);
                }
            }
        }

        while let Some((span, request)) = requests_rx.recv().await {
            let _g = span.enter();
            if let Request::Stop = request {
                debug!("received Stop request");
                break;
            }
            self.handle_request(request);
        }

        debug!("WindowNotify actor exiting");
    }

    fn handle_request(&mut self, request: Request) {
        match request {
            Request::Subscribe(event) => {
                if self.subscribed.contains(&event) {
                    debug!("already subscribed to event {}", event);
                    return;
                }
                match Self::subscribe(
                    event,
                    self.events_tx.clone(),
                    self.spaces_tx.clone(),
                    self.focus_wake.clone(),
                    self.border_tx.clone(),
                    self.border_motion.clone(),
                ) {
                    Ok(()) => {
                        self.subscribed.insert(event);
                        debug!("subscribed to event {}", event);
                    }
                    Err(code) => {
                        debug!("failed to register event {} (res={})", event, code);
                    }
                }
            }
            Request::UpdateWindowNotifications(window_ids) => {
                window_notify::update_window_notifications(&window_ids);
            }

            Request::Stop => {}
        }
    }

    fn subscribe(
        event: CGSEventType,
        events_tx: reactor::Sender,
        spaces_tx: spaces::Sender,
        focus_wake: FocusWakeSender,
        border_tx: border::Sender,
        border_motion: border::BorderMotionHandle,
    ) -> Result<(), i32> {
        let res = window_notify::init(event);
        if res != 0 {
            return Err(res);
        }

        let mut rx = window_notify::take_receiver(event);

        std::thread::spawn(move || {
            while let Some((_span, evt)) = rx.blocking_recv() {
                trace!(?event, ?evt, "got event");

                match event {
                    CGSEventType::Known(KnownCGSEvent::WindowClosed) => {
                        let Some(window_id) = evt.window_id else {
                            continue;
                        };
                        events_tx.send(Event::WindowClosed(WindowServerId::new(window_id)));
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceDestroyed) => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx.send(spaces::Event::SpaceDestroyed(SpaceId::new(space_id)));
                        }
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceCreated) => {
                        if let Some(space_id) = evt.space_id {
                            spaces_tx.send(spaces::Event::SpaceCreated(SpaceId::new(space_id)));
                        }
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceCurrentChanged) => {
                        spaces_tx.send(spaces::Event::ActiveSpaceChanged);
                    }
                    CGSEventType::Known(KnownCGSEvent::ManagedSpaceMembershipUpdated)
                    | CGSEventType::Known(
                        KnownCGSEvent::SpaceWindowManagementCapabilitiesChanged,
                    ) => {
                        spaces_tx.send(spaces::Event::SpaceInventoryChanged);
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceWindowDestroyed) => {
                        focus_wake.notify();
                        let (Some(window_id), Some(space_id)) = (evt.window_id, evt.space_id)
                        else {
                            continue;
                        };
                        // This is not just "window left the current active-space snapshot".
                        // CGS emits SpaceWindowDestroyed when the window's connection drops
                        // out of the WindowServer membership for that space.
                        spaces_tx.send(spaces::Event::WindowServerDestroyed(
                            WindowServerId::new(window_id),
                            SpaceId::new(space_id),
                        ))
                    }
                    CGSEventType::Known(KnownCGSEvent::SpaceWindowCreated) => {
                        focus_wake.notify();
                        let (Some(window_id), Some(space_id)) = (evt.window_id, evt.space_id)
                        else {
                            continue;
                        };
                        spaces_tx.send(spaces::Event::WindowServerAppeared(
                            WindowServerId::new(window_id),
                            SpaceId::new(space_id),
                        ));
                    }
                    CGSEventType::Known(KnownCGSEvent::WindowReordered)
                    | CGSEventType::Known(KnownCGSEvent::WindowLevelChanged)
                    | CGSEventType::Known(KnownCGSEvent::WindowManagerActivatingClickOrdering)
                    | CGSEventType::Known(KnownCGSEvent::WindowOrderingGroupChanged)
                    | CGSEventType::Known(KnownCGSEvent::WindowParentChanged)
                    | CGSEventType::Known(
                        KnownCGSEvent::WindowManagerSpaceFrontConnectionChanged,
                    )
                    | CGSEventType::Known(
                        KnownCGSEvent::WindowManagerGlobalFrontConnectionChanged,
                    ) => {
                        focus_wake.notify();
                        if is_border_order_event(event)
                            && let Some(window_id) = evt.window_id
                        {
                            border_tx.send(border::Event::OrderInvalidated(WindowServerId::new(
                                window_id,
                            )));
                        }
                    }
                    CGSEventType::Known(
                        KnownCGSEvent::WindowHidden | KnownCGSEvent::WindowUnhidden,
                    ) => {
                        focus_wake.notify();
                        if let Some(window_id) = evt.window_id {
                            let wsid = WindowServerId::new(window_id);
                            events_tx.send(
                                if matches!(
                                    evt.event_type,
                                    CGSEventType::Known(KnownCGSEvent::WindowUnhidden)
                                ) {
                                    Event::WindowServerUnhidden(wsid)
                                } else {
                                    Event::WindowServerHidden(wsid)
                                },
                            );
                        }
                    }
                    CGSEventType::Known(KnownCGSEvent::WindowMoved)
                    | CGSEventType::Known(KnownCGSEvent::WindowResized) => {
                        debug_assert!(is_border_frame_event(event));
                        let Some(window_id) = evt.window_id else {
                            continue;
                        };
                        let wsid = WindowServerId::new(window_id);
                        if let Some(bounds) = window_server::get_window_bounds(wsid) {
                            if let Err(error) = border_motion.move_target(wsid, bounds) {
                                trace!(?error, ?wsid, "focused-window border fast move failed");
                            }
                            border_tx.send(border::Event::FrameChanged(wsid, bounds));
                        };
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    fn spawn_focus_resolver(events_tx: reactor::Sender, focus_wake_rx: mpsc::Receiver<()>) {
        std::thread::Builder::new()
            .name("window-focus-resolver".to_string())
            .spawn(move || {
                while focus_wake_rx.recv().is_ok() {
                    let mut wake_count = 1;
                    loop {
                        // WindowServer commonly emits 808 and 815 back-to-back.
                        // The bounded channel absorbs both into this one wake.
                        std::thread::sleep(FOCUS_WAKE_DEBOUNCE);
                        while focus_wake_rx.try_recv().is_ok() {
                            wake_count += 1;
                        }

                        let query_started = Instant::now();
                        let focused = window_server::key_focused_window();
                        let query_elapsed = query_started.elapsed();

                        // A wake queued during the SPI means this result may already
                        // be stale. Resolve once more before publishing it.
                        if focus_wake_rx.try_recv().is_ok() {
                            wake_count += 1;
                            continue;
                        }

                        trace!(
                            wake_count,
                            ?query_elapsed,
                            ?focused,
                            "resolved coalesced WindowServer focus"
                        );
                        if let Some((window, space)) = focused {
                            events_tx.send(Event::WindowServerFocusChanged(window, space));
                        }
                        break;
                    }
                }
            })
            .expect("failed to spawn WindowServer focus resolver");
    }
}

#[cfg(test)]
mod tests {
    use super::{FocusWakeSender, is_border_frame_event, is_border_order_event};
    use crate::sys::skylight::{CGSEventType, KnownCGSEvent};

    #[test]
    fn focus_wakes_coalesce_to_one_signal() {
        let (wake, rx) = std::sync::mpsc::sync_channel(1);
        let sender = FocusWakeSender { wake };

        sender.notify();
        sender.notify();
        sender.notify();

        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn activating_click_and_order_group_events_resync_the_border() {
        for event in [
            KnownCGSEvent::WindowReordered,
            KnownCGSEvent::WindowLevelChanged,
            KnownCGSEvent::WindowManagerActivatingClickOrdering,
            KnownCGSEvent::WindowOrderingGroupChanged,
            KnownCGSEvent::WindowParentChanged,
        ] {
            assert!(is_border_order_event(CGSEventType::Known(event)));
        }
        assert!(!is_border_order_event(CGSEventType::Known(
            KnownCGSEvent::WindowMoved,
        )));
    }

    #[test]
    fn move_and_resize_events_use_the_border_fast_path() {
        for event in [KnownCGSEvent::WindowMoved, KnownCGSEvent::WindowResized] {
            assert!(is_border_frame_event(CGSEventType::Known(event)));
        }
        assert!(!is_border_frame_event(CGSEventType::Known(
            KnownCGSEvent::WindowReordered,
        )));
    }
}
