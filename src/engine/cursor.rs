//! A cursor is a point plus an optional mark (Emacs' region), a goal column for
//! vertical motion and its own mark ring. The editor holds one or more cursors.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub id: u64,
    pub point: usize,
    pub mark: Option<usize>,
    /// Transient-mark-mode: the region is active (highlighted, used by commands).
    pub active: bool,
    /// Region was started by shift+motion and ends on unshifted motion.
    pub shift_selected: bool,
    pub goal_col: Option<usize>,
    pub mark_ring: Vec<usize>,
}

pub const MARK_RING_MAX: usize = 16;

impl Cursor {
    pub fn new(id: u64, point: usize) -> Self {
        Self { id, point, mark: None, active: false, shift_selected: false, goal_col: None, mark_ring: Vec::new() }
    }

    /// The active region, if any.
    pub fn region(&self) -> Option<Range<usize>> {
        if !self.active {
            return None;
        }
        self.mark.map(|m| m.min(self.point)..m.max(self.point))
    }

    /// The region between point and mark whether or not it is active.
    pub fn mark_region(&self) -> Option<Range<usize>> {
        self.mark.map(|m| m.min(self.point)..m.max(self.point))
    }

    pub fn push_mark(&mut self, pos: usize, activate: bool) {
        if let Some(old) = self.mark {
            self.mark_ring.push(old);
            if self.mark_ring.len() > MARK_RING_MAX {
                self.mark_ring.remove(0);
            }
        }
        self.mark = Some(pos);
        self.active = activate;
        self.shift_selected = false;
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.shift_selected = false;
    }

    /// Shift every stored position to account for `start..end` being replaced
    /// by `inserted` chars.
    pub fn adjust(&mut self, start: usize, end: usize, inserted: usize) {
        let f = |p: usize| adjust_pos(p, start, end, inserted);
        self.point = f(self.point);
        self.mark = self.mark.map(f);
        for m in &mut self.mark_ring {
            *m = f(*m);
        }
    }
}

/// Positions before the change stay; positions after shift; positions inside a
/// same-size replacement stay put, otherwise they collapse into the new text.
pub fn adjust_pos(p: usize, start: usize, end: usize, inserted: usize) -> usize {
    if p <= start {
        p
    } else if p >= end {
        p - (end - start) + inserted
    } else {
        start + (p - start).min(inserted)
    }
}
