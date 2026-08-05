/// How many render ticks a size must stay unchanged before it is applied to
/// the grid and PTY. At the AppKit timer's 120 Hz this is ~150 ms. This turns
/// a continuous live drag into a single resize once the drag settles.
const RESIZE_STABLE_TICKS: u32 = 18;

/// Coalesces window-size changes so the grid and PTY are resized together,
/// only after the size has been stable for a short while.
///
/// Resizing eagerly on every intermediate size during a live drag makes the
/// shell redraw repeatedly. Worse, if the grid moves to a smaller width before
/// the shell's redraw for the previous width is processed, the redraw can wrap
/// and leave duplicate lines on screen. Deferring the grid and PTY resize
/// together, after the drag settles, keeps both sides at the same size.
#[derive(Debug, Default)]
pub(crate) struct ResizeDebouncer {
    pending: Option<(usize, usize)>,
    last_recorded: Option<(usize, usize)>,
    stable_ticks: u32,
    last_sent: Option<(usize, usize)>,
}

impl ResizeDebouncer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record a size change from the window. Returns true when the size
    /// actually differs from the previously recorded one.
    pub(crate) fn record(&mut self, cols: usize, rows: usize) -> bool {
        if self.last_recorded == Some((cols, rows)) {
            return false;
        }
        self.last_recorded = Some((cols, rows));
        self.pending = Some((cols, rows));
        self.stable_ticks = 0;
        true
    }

    /// Whether any size has been handed to the PTY yet.
    pub(crate) fn has_sent(&self) -> bool {
        self.last_sent.is_some()
    }

    /// Return the pending size immediately without waiting for stability.
    /// Used for the very first resize so the shell gets a sane size at startup.
    pub(crate) fn take_now(&mut self) -> Option<(usize, usize)> {
        let size = self.pending.take();
        self.stable_ticks = 0;
        size
    }

    /// Advance one render tick. Returns the size to apply once it has stayed
    /// unchanged for enough ticks; the pending change is cleared in that case.
    pub(crate) fn take_stable(&mut self) -> Option<(usize, usize)> {
        let Some(size) = self.pending else {
            self.stable_ticks = 0;
            return None;
        };
        self.stable_ticks += 1;
        if self.stable_ticks < RESIZE_STABLE_TICKS {
            return None;
        }
        self.pending = None;
        self.stable_ticks = 0;
        Some(size)
    }

    /// Remember that `size` was handed to the PTY. Returns true when it had
    /// not been sent before, so callers can skip redundant resize events.
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
    fn resize_is_applied_after_enough_stable_ticks() {
        let mut debouncer = ResizeDebouncer::new();
        assert!(debouncer.record(80, 24));
        for _ in 0..RESIZE_STABLE_TICKS - 1 {
            assert_eq!(debouncer.take_stable(), None);
        }
        assert_eq!(debouncer.take_stable(), Some((80, 24)));
    }

    #[test]
    fn recording_the_same_size_keeps_stability() {
        let mut debouncer = ResizeDebouncer::new();
        debouncer.record(80, 24);
        // Repeated identical records must not reset the stability timer.
        let mut ticks = 0;
        while ticks < RESIZE_STABLE_TICKS / 2 {
            assert!(!debouncer.record(80, 24));
            assert_eq!(debouncer.take_stable(), None);
            ticks += 1;
        }
        while ticks < RESIZE_STABLE_TICKS - 1 {
            assert_eq!(debouncer.take_stable(), None);
            ticks += 1;
        }
        assert_eq!(debouncer.take_stable(), Some((80, 24)));
    }

    #[test]
    fn a_new_size_restarts_the_stability_window() {
        let mut debouncer = ResizeDebouncer::new();
        debouncer.record(80, 24);
        for _ in 0..RESIZE_STABLE_TICKS / 2 {
            assert_eq!(debouncer.take_stable(), None);
        }
        // The user keeps dragging; the pending size changes.
        assert!(debouncer.record(70, 18));
        for _ in 0..RESIZE_STABLE_TICKS - 1 {
            assert_eq!(debouncer.take_stable(), None);
        }
        assert_eq!(debouncer.take_stable(), Some((70, 18)));
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
        debouncer.record(100, 30);
        assert_eq!(debouncer.take_now(), Some((100, 30)));
        debouncer.mark_sent((100, 30));
        assert!(debouncer.has_sent());
    }
}
