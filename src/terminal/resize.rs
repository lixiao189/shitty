use std::time::{Duration, Instant};

/// How long a terminal size must stay unchanged before it is applied.
pub(crate) const RESIZE_DEBOUNCE_DELAY: Duration = Duration::from_millis(150);

/// Coalesces window-size changes so the grid and PTY are resized together,
/// only after the final size has been stable for a short while.
///
/// Resizing eagerly on every intermediate size during a live drag makes the
/// shell redraw repeatedly. Deferring the grid and PTY resize together keeps
/// shell output from being parsed at a width different from the one it was
/// generated for.
#[derive(Debug, Default)]
pub(crate) struct ResizeDebouncer {
    pending: Option<(usize, usize)>,
    last_recorded: Option<(usize, usize)>,
    deadline: Option<Instant>,
    last_sent: Option<(usize, usize)>,
}

impl ResizeDebouncer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a changed grid size and restart the trailing debounce window.
    pub(crate) fn record(&mut self, cols: usize, rows: usize, now: Instant) -> bool {
        if self.last_recorded == Some((cols, rows)) {
            return false;
        }
        self.last_recorded = Some((cols, rows));
        self.pending = Some((cols, rows));
        self.deadline = Some(now + RESIZE_DEBOUNCE_DELAY);
        true
    }

    /// Whether any size has been handed to the PTY yet.
    pub(crate) fn has_sent(&self) -> bool {
        self.last_sent.is_some()
    }

    /// Return the pending size immediately, used for the initial PTY size.
    pub(crate) fn take_now(&mut self) -> Option<(usize, usize)> {
        self.deadline = None;
        self.pending.take()
    }

    /// Return the pending size once its deadline has elapsed.
    pub(crate) fn take_ready(&mut self, now: Instant) -> Option<(usize, usize)> {
        if self.deadline.is_some_and(|deadline| deadline <= now) {
            return self.take_now();
        }
        None
    }

    /// Time remaining before the pending size may be applied.
    pub(crate) fn remaining_delay(&self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// Remember that `size` was handed to the PTY and deduplicate repeats.
    pub(crate) fn mark_sent(&mut self, size: (usize, usize)) -> bool {
        if self.last_sent == Some(size) {
            return false;
        }
        self.last_sent = Some(size);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_is_applied_after_debounce_deadline() {
        let start = Instant::now();
        let mut debouncer = ResizeDebouncer::new();
        assert!(debouncer.record(80, 24, start));
        assert_eq!(
            debouncer.take_ready(start + RESIZE_DEBOUNCE_DELAY - Duration::from_millis(1)),
            None
        );
        assert_eq!(
            debouncer.take_ready(start + RESIZE_DEBOUNCE_DELAY),
            Some((80, 24))
        );
    }

    #[test]
    fn recording_the_same_size_does_not_extend_deadline() {
        let start = Instant::now();
        let mut debouncer = ResizeDebouncer::new();
        debouncer.record(80, 24, start);
        assert!(!debouncer.record(80, 24, start + Duration::from_millis(100)));
        assert_eq!(
            debouncer.take_ready(start + RESIZE_DEBOUNCE_DELAY),
            Some((80, 24))
        );
    }

    #[test]
    fn a_new_size_restarts_the_debounce_window() {
        let start = Instant::now();
        let mut debouncer = ResizeDebouncer::new();
        debouncer.record(80, 24, start);
        debouncer.record(70, 18, start + Duration::from_millis(100));
        assert_eq!(debouncer.take_ready(start + RESIZE_DEBOUNCE_DELAY), None);
        assert_eq!(
            debouncer.take_ready(start + Duration::from_millis(250)),
            Some((70, 18))
        );
    }

    #[test]
    fn remaining_delay_tracks_latest_change() {
        let start = Instant::now();
        let mut debouncer = ResizeDebouncer::new();
        debouncer.record(80, 24, start);
        assert_eq!(
            debouncer.remaining_delay(start + Duration::from_millis(50)),
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn sent_sizes_are_deduplicated() {
        let mut debouncer = ResizeDebouncer::new();
        assert!(debouncer.mark_sent((80, 24)));
        assert!(!debouncer.mark_sent((80, 24)));
        assert!(debouncer.mark_sent((79, 24)));
    }

    #[test]
    fn first_resize_can_be_taken_immediately() {
        let mut debouncer = ResizeDebouncer::new();
        assert!(!debouncer.has_sent());
        debouncer.record(100, 30, Instant::now());
        assert_eq!(debouncer.take_now(), Some((100, 30)));
        debouncer.mark_sent((100, 30));
        assert!(debouncer.has_sent());
    }
}
