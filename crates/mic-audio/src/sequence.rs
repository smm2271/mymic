use mic_protocol::ahead;
use std::collections::{HashSet, VecDeque};
/// Sequence gaps are observed missing slots, not proof of UDP network loss.
#[derive(Default)]
pub struct SequenceTracker {
    expected: Option<u32>,
    highest: Option<u32>,
    pending: HashSet<u32>,
    recent: VecDeque<u32>,
    missing_since: Option<u64>,
    pub gaps: u64,
    pub reordered: u64,
    pub duplicates: u64,
}
impl SequenceTracker {
    /// Holes currently visible inside the bounded reorder window; not finalized loss.
    pub fn pending_holes(&self) -> u64 {
        match (self.expected, self.highest) {
            (Some(expected), Some(highest)) if !ahead(expected, highest) => {
                (highest.wrapping_sub(expected) as u64 + 1)
                    .saturating_sub(self.pending.len() as u64)
            }
            _ => 0,
        }
    }
    pub fn observe(&mut self, sequence: u32, now: u64) {
        if self.recent.contains(&sequence) {
            self.duplicates += 1;
            return;
        }
        self.recent.push_back(sequence);
        if self.recent.len() > 256 {
            self.recent.pop_front();
        }
        if let Some(highest) = self.highest {
            if ahead(highest, sequence) {
                self.reordered += 1
            } else if ahead(sequence, highest) {
                self.highest = Some(sequence)
            }
        } else {
            self.highest = Some(sequence);
            self.expected = Some(sequence);
        }
        if let Some(e) = self.expected {
            if ahead(e, sequence) {
                return;
            }
            if sequence.wrapping_sub(e) > 255 {
                self.pending.clear();
                self.expected = Some(sequence);
                self.missing_since = None;
            }
        }
        self.pending.insert(sequence);
        self.finalize(now);
    }
    pub fn finalize(&mut self, now: u64) {
        while let (Some(expected), Some(highest)) = (self.expected, self.highest) {
            if ahead(expected, highest) {
                break;
            }
            if self.pending.remove(&expected) {
                self.expected = Some(expected.wrapping_add(1));
                self.missing_since = None;
                continue;
            }
            let since = *self.missing_since.get_or_insert(now);
            if now.saturating_sub(since) < 40 {
                break;
            }
            self.gaps += 1;
            self.expected = Some(expected.wrapping_add(1));
            // All adjacent holes first became observable at the same time.
        }
    }
    pub fn reset(&mut self) {
        let (gaps, reordered, duplicates) = (self.gaps, self.reordered, self.duplicates);
        *self = Self::default();
        self.gaps = gaps;
        self.reordered = reordered;
        self.duplicates = duplicates;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reordering_window_does_not_permanently_count_loss() {
        let mut t = SequenceTracker::default();
        t.observe(0, 0);
        t.observe(2, 10);
        assert_eq!(t.pending_holes(), 1);
        t.observe(1, 30);
        assert_eq!(t.pending_holes(), 0);
        t.finalize(100);
        assert_eq!(t.gaps, 0);
        assert_eq!(t.reordered, 1);
    }
    #[test]
    fn gap_expires_and_duplicate_is_not_data() {
        let mut t = SequenceTracker::default();
        t.observe(0, 0);
        t.observe(2, 10);
        t.finalize(49);
        assert_eq!(t.gaps, 0);
        t.finalize(50);
        assert_eq!(t.gaps, 1);
        t.observe(2, 60);
        assert_eq!(t.duplicates, 1);
    }
    #[test]
    fn wraps_and_resets() {
        let mut t = SequenceTracker::default();
        t.observe(u32::MAX, 0);
        t.observe(0, 10);
        assert_eq!(t.gaps, 0);
        t.reset();
        t.observe(900, 20);
        assert_eq!(t.gaps, 0);
        assert!(t.pending.len() <= 256);
    }
}
