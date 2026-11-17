//! Find in output (Ctrl+F): searches the command blocks of the active tab, highlights
//! every match and steps through them.
//!
//! Matching is case-insensitive for ASCII letters (other characters must match
//! exactly) and runs over each block's plain text, the same text the block draws, so
//! the match count and the highlights agree.

use std::ops::Range;
use uuid::Uuid;

/// The find bar's state, owned by the app.
#[derive(Debug, Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    /// Index of the current match among all matches of the tab.
    pub current: usize,
    /// Move keyboard focus to the query box on the next frame.
    pub focus_requested: bool,
    /// Scroll the current match into view on the next frame.
    pub scroll_pending: bool,
    /// Frames left in which to keep scrolling to the current match. The request is
    /// repeated for a few frames because the first one can run before the blocks
    /// have their final layout.
    pub scroll_frames: u8,
}

impl FindState {
    pub fn open(&mut self) {
        self.open = true;
        self.focus_requested = true;
        self.scroll_pending = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Steps to the next (or previous) of `total` matches, wrapping around.
    pub fn step(&mut self, forward: bool, total: usize) {
        if total == 0 {
            self.current = 0;
            return;
        }
        self.current = if forward {
            (self.current + 1) % total
        } else {
            (self.current + total - 1) % total
        };
        self.scroll_pending = true;
    }
}

/// What blocks need to draw highlights this frame (stored in egui's temporary data
/// under [`HIGHLIGHT_ID`] while the find bar is open).
#[derive(Debug, Clone)]
pub struct Highlight {
    pub query: String,
    /// The block holding the current match, and which of its matches it is.
    pub current: Option<(Uuid, usize)>,
    /// Scroll the current match into view this frame.
    pub scroll: bool,
}

/// egui temporary-data key for [`Highlight`].
pub const HIGHLIGHT_ID: &str = "mrt_find_highlight";

/// Byte ranges of the non-overlapping matches of `query` in `text`, left to right.
pub fn match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    if query.is_empty() || query.len() > text.len() {
        return ranges;
    }
    let hay = text.as_bytes();
    let needle = query.as_bytes();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if text.is_char_boundary(i) && hay[i..i + needle.len()].eq_ignore_ascii_case(needle) && text.is_char_boundary(i + needle.len()) {
            ranges.push(i..i + needle.len());
            i += needle.len();
        } else {
            i += 1;
        }
    }
    ranges
}

/// The match counts per block, in display order, and their total.
pub fn count_matches<'a>(blocks: impl IntoIterator<Item = (Uuid, &'a str)>, query: &str) -> (Vec<(Uuid, usize)>, usize) {
    let mut per_block = Vec::new();
    let mut total = 0;
    for (id, text) in blocks {
        let n = match_ranges(text, query).len();
        if n > 0 {
            per_block.push((id, n));
            total += n;
        }
    }
    (per_block, total)
}

/// The block and the match within it for the `index`-th match overall.
pub fn locate(per_block: &[(Uuid, usize)], mut index: usize) -> Option<(Uuid, usize)> {
    for (id, n) in per_block {
        if index < *n {
            return Some((*id, index));
        }
        index -= n;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_ascii_case_insensitive_and_non_overlapping() {
        assert_eq!(match_ranges("Error: error ERROR", "error"), vec![0..5, 7..12, 13..18]);
        assert_eq!(match_ranges("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(match_ranges("abc", "").is_empty());
        assert!(match_ranges("ab", "abc").is_empty());
        // Non-ASCII text is matched exactly and ranges stay on character boundaries.
        assert_eq!(match_ranges("acción — acción", "acción"), vec![0..7, 12..19]);
        assert_eq!(match_ranges("é", "e"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn counting_and_locating_across_blocks() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let (per_block, total) = count_matches([(a, "x foo foo"), (b, "nothing"), (c, "FOO")], "foo");
        assert_eq!(total, 3);
        assert_eq!(per_block, vec![(a, 2), (c, 1)]);
        assert_eq!(locate(&per_block, 0), Some((a, 0)));
        assert_eq!(locate(&per_block, 1), Some((a, 1)));
        assert_eq!(locate(&per_block, 2), Some((c, 0)));
        assert_eq!(locate(&per_block, 3), None);
    }

    #[test]
    fn stepping_wraps_around() {
        let mut state = FindState::default();
        state.step(true, 3);
        state.step(true, 3);
        state.step(true, 3);
        assert_eq!(state.current, 0);
        state.step(false, 3);
        assert_eq!(state.current, 2);
        state.step(true, 0);
        assert_eq!(state.current, 0);
    }
}
