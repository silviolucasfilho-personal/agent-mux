use std::time::{Duration, Instant};

/// Output within this window => Working. Tunable here only.
pub const ACTIVITY_WINDOW: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Working,
    Idle,
    NeedsAttention,
    /// None = exit code unknown (e.g. degraded after a write failure).
    Exited(Option<u32>),
}

#[derive(Debug, Default)]
pub struct StatusTracker {
    last_output: Option<Instant>,
    needs_attention: bool,
    exited: Option<Option<u32>>,
    last_bell_count: usize,
}

impl StatusTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_output(&mut self, now: Instant, bell_count: usize, focused: bool) {
        self.last_output = Some(now);
        if bell_count > self.last_bell_count {
            self.last_bell_count = bell_count;
            if !focused {
                self.needs_attention = true;
            }
        }
    }

    pub fn on_attach(&mut self) {
        self.needs_attention = false;
    }

    pub fn on_exit(&mut self, code: Option<u32>) {
        self.exited = Some(code);
    }

    pub fn status(&self, now: Instant) -> Status {
        if let Some(code) = self.exited {
            return Status::Exited(code);
        }
        if self.needs_attention {
            return Status::NeedsAttention;
        }
        match self.last_output {
            Some(t) if now.duration_since(t) < ACTIVITY_WINDOW => Status::Working,
            _ => Status::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn fresh_session_with_no_output_is_idle() {
        let tracker = StatusTracker::new();
        assert_eq!(tracker.status(t0()), Status::Idle);
    }

    #[test]
    fn recent_output_means_working() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 0, false);
        assert_eq!(tracker.status(now + Duration::from_millis(500)), Status::Working);
    }

    #[test]
    fn quiet_beyond_activity_window_means_idle() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 0, false);
        assert_eq!(tracker.status(now + ACTIVITY_WINDOW + Duration::from_millis(1)), Status::Idle);
    }

    #[test]
    fn bell_on_unfocused_session_sets_needs_attention_and_wins_over_working() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 1, false); // bell count went 0 -> 1
        assert_eq!(tracker.status(now), Status::NeedsAttention);
    }

    #[test]
    fn bell_while_focused_is_ignored() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 1, true);
        assert_eq!(tracker.status(now), Status::Working);
    }

    #[test]
    fn unchanged_bell_count_does_not_retrigger() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 1, true); // consumed while focused
        tracker.on_output(now, 1, false); // same count, now unfocused
        assert_eq!(tracker.status(now), Status::Working);
    }

    #[test]
    fn attach_clears_needs_attention() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 1, false);
        tracker.on_attach();
        assert_eq!(tracker.status(now), Status::Working);
    }

    #[test]
    fn exited_wins_over_everything() {
        let now = t0();
        let mut tracker = StatusTracker::new();
        tracker.on_output(now, 1, false);
        tracker.on_exit(Some(0));
        assert_eq!(tracker.status(now), Status::Exited(Some(0)));
    }

    #[test]
    fn vt100_counts_real_bell_but_not_osc_terminator() {
        // Verifies the design assumption behind using audible_bell_count().
        // In vt100 0.16.2, bells are tracked via Callbacks trait.
        #[derive(Default)]
        struct BellCounter {
            count: usize,
        }
        impl vt100::Callbacks for BellCounter {
            fn audible_bell(&mut self, _: &mut vt100::Screen) {
                self.count += 1;
            }
        }

        let counter = BellCounter::default();
        let mut parser = vt100::Parser::new_with_callbacks(24, 80, 0, counter);
        parser.process(b"\x1b]0;window title\x07"); // OSC set-title, BEL-terminated
        assert_eq!(parser.callbacks().count, 0);
        parser.process(b"hello\x07");
        assert_eq!(parser.callbacks().count, 1);
    }
}
