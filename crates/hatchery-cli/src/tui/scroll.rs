//! Scroll state: an offset into the wrapped transcript, or "follow the tail".
//!
//! The transcript renders a window of its own wrapped lines (the terminal's scrollback is not
//! involved: the TUI owns the alternate screen), so nothing is ever truncated away — scrolling
//! up reaches the first line of the session. `None` means follow: pinned to the tail, which is
//! where a streaming conversation wants to be. Any upward scroll breaks follow; `End` restores
//! it, and scrolling down to the tail restores it by itself.

/// Where the transcript window starts, in wrapped lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollState {
    offset: Option<usize>,
}

impl ScrollState {
    /// True while the window rides the tail.
    #[must_use]
    pub const fn following(&self) -> bool {
        self.offset.is_none()
    }

    /// Pins the window back to the tail.
    pub fn follow(&mut self) {
        self.offset = None;
    }

    /// Pins the window to the very first line.
    pub fn top(&mut self) {
        self.offset = Some(0);
    }

    /// The effective top line: a stored offset clamped to what exists, or the tail.
    #[must_use]
    pub fn resolve(&self, total: usize, height: usize) -> usize {
        self.offset
            .unwrap_or(total.saturating_sub(height))
            .min(max_offset(total, height))
    }

    /// Moves by `delta` lines (negative up). Reaching the tail resumes follow.
    pub fn scroll_by(&mut self, delta: isize, total: usize, height: usize) {
        let max = max_offset(total, height);
        let next = (self.resolve(total, height) as isize + delta).clamp(0, max as isize) as usize;
        self.offset = if next >= max { None } else { Some(next) };
    }

    /// Puts `line` at the top of the window (clamped); unlike `scroll_by` this stays pinned even
    /// at the tail, because a jump is an explicit "show me this message" — follow would undo it
    /// whenever the last message is shorter than the viewport.
    pub fn jump_to(&mut self, line: usize, total: usize, height: usize) {
        self.offset = Some(line.min(max_offset(total, height)));
    }
}

fn max_offset(total: usize, height: usize) -> usize {
    total.saturating_sub(height.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_rides_the_tail_and_breaks_on_any_upward_scroll() {
        let mut scroll = ScrollState::default();
        assert!(scroll.following());
        assert_eq!(scroll.resolve(100, 20), 80);
        scroll.scroll_by(-5, 100, 20);
        assert!(!scroll.following());
        assert_eq!(scroll.resolve(100, 20), 75);
    }

    #[test]
    fn scrolling_back_to_the_tail_resumes_follow() {
        let mut scroll = ScrollState::default();
        scroll.scroll_by(-30, 100, 20);
        scroll.scroll_by(30, 100, 20);
        assert!(
            scroll.following(),
            "the tail is follow, not a stored offset"
        );
    }

    #[test]
    fn offsets_clamp_to_what_exists() {
        let mut scroll = ScrollState::default();
        scroll.scroll_by(-1000, 100, 20);
        assert_eq!(scroll.resolve(100, 20), 0);
        scroll.scroll_by(1000, 100, 20);
        assert!(scroll.following());
        // A transcript shorter than the viewport has exactly one window.
        assert_eq!(scroll.resolve(5, 20), 0);
        scroll.scroll_by(-3, 5, 20);
        assert_eq!(scroll.resolve(5, 20), 0);
    }

    #[test]
    fn jumps_pin_even_at_the_tail() {
        let mut scroll = ScrollState::default();
        scroll.jump_to(40, 100, 20);
        assert_eq!(scroll.resolve(100, 20), 40);
        scroll.jump_to(95, 100, 20);
        assert_eq!(
            scroll.resolve(100, 20),
            80,
            "clamped to the last full window"
        );
        assert!(
            !scroll.following(),
            "a jump stays put so the message stays on top"
        );
        scroll.top();
        assert_eq!(scroll.resolve(100, 20), 0);
        scroll.follow();
        assert_eq!(scroll.resolve(100, 20), 80);
    }
}
