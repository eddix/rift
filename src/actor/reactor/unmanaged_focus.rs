use crate::actor::app::{WindowId, pid_t};
use crate::common::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Toggle,
    Cycle,
}

#[derive(Debug, Default)]
pub(crate) struct History {
    by_display: HashMap<String, Vec<WindowId>>,
}

impl History {
    pub(crate) fn record(&mut self, display: &str, window: WindowId) {
        for windows in self.by_display.values_mut() {
            windows.retain(|candidate| *candidate != window);
        }
        self.by_display.entry(display.to_owned()).or_default().insert(0, window);
    }

    pub(crate) fn remove_window(&mut self, window: WindowId) {
        for windows in self.by_display.values_mut() {
            windows.retain(|candidate| *candidate != window);
        }
        self.by_display.retain(|_, windows| !windows.is_empty());
    }

    pub(crate) fn remove_app(&mut self, pid: pid_t) {
        for windows in self.by_display.values_mut() {
            windows.retain(|candidate| candidate.pid != pid);
        }
        self.by_display.retain(|_, windows| !windows.is_empty());
    }

    pub(crate) fn target(
        &mut self,
        display: &str,
        candidates: &[WindowId],
        current: Option<WindowId>,
        last_managed: Option<WindowId>,
        action: Action,
    ) -> Option<WindowId> {
        let history = self.by_display.entry(display.to_owned()).or_default();
        history.retain(|window| candidates.contains(window));
        for &candidate in candidates {
            if !history.contains(&candidate) {
                history.push(candidate);
            }
        }

        match action {
            Action::Toggle if current.is_some_and(|window| history.contains(&window)) => {
                last_managed
            }
            Action::Toggle => history.first().copied(),
            Action::Cycle => {
                let next = current
                    .and_then(|window| history.iter().position(|candidate| *candidate == window))
                    .map_or(0, |index| (index + 1) % history.len().max(1));
                history.rotate_left(next);
                history.first().copied()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(index: u32) -> WindowId { WindowId::new(index as i32, index) }

    #[test]
    fn toggle_switches_between_mru_unmanaged_and_last_managed() {
        let mut history = History::default();
        let managed = window(1);
        let unmanaged = window(2);
        history.record("display", unmanaged);

        let from_managed = history.target(
            "display",
            &[unmanaged],
            Some(managed),
            Some(managed),
            Action::Toggle,
        );
        let from_unmanaged = history.target(
            "display",
            &[unmanaged],
            Some(unmanaged),
            Some(managed),
            Action::Toggle,
        );

        assert_eq!((from_managed, from_unmanaged), (Some(unmanaged), Some(managed)));
    }

    #[test]
    fn cycle_visits_each_unmanaged_window_before_wrapping() {
        let mut history = History::default();
        let first = window(1);
        let second = window(2);
        let third = window(3);
        let candidates = [first, second, third];

        let next = history.target("display", &candidates, Some(first), None, Action::Cycle);
        let after_next = history.target("display", &candidates, next, None, Action::Cycle);
        let wrapped = history.target("display", &candidates, after_next, None, Action::Cycle);

        assert_eq!(
            (next, after_next, wrapped),
            (Some(second), Some(third), Some(first))
        );
    }

    #[test]
    fn closed_windows_are_removed_when_candidates_are_reconciled() {
        let mut history = History::default();
        let closed = window(1);
        let remaining = window(2);
        history.record("display", closed);
        history.record("display", remaining);

        let target = history.target("display", &[remaining], None, None, Action::Toggle);

        assert_eq!(target, Some(remaining));
    }
}
