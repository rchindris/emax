//! Pure operations on window trees: splitting, deleting, balancing and
//! resizing windows, the rectangles they occupy, and finding the window next
//! to another one on screen. Nothing here knows about buffers or cursors, so
//! it is all testable on plain trees.
//!
//! Sizes are in cells (columns for side-by-side splits, lines for stacked
//! ones). A window's height includes its mode line, as in Emacs.

use super::{Node, Rect, SplitDir, WindowId};

/// Smallest window height in lines, mode line included (`window-min-height`).
pub const MIN_ROWS: f32 = 4.0;
/// Smallest window width in columns (`window-min-width`).
pub const MIN_COLS: f32 = 10.0;

/// Slack for comparing positions that went through float arithmetic.
const EPS: f32 = 0.01;

impl SplitDir {
    /// The extent of `r` this direction divides: width for side-by-side
    /// splits, height for stacked ones.
    pub fn extent(self, r: Rect) -> f32 {
        match self {
            SplitDir::Horizontal => r.w,
            SplitDir::Vertical => r.h,
        }
    }

    /// The smallest a single window may be along this direction.
    pub fn min_extent(self) -> f32 {
        match self {
            SplitDir::Horizontal => MIN_COLS,
            SplitDir::Vertical => MIN_ROWS,
        }
    }
}

/// A direction to look for a neighbouring window in (`windmove-*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// Why a resize did nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeError {
    /// The window is not in the tree.
    NoWindow,
    /// No split along that direction contains the window (e.g. the sole window).
    NoSplit,
    /// Every other window is already as small as allowed.
    CannotEnlarge,
    /// The window is already as small as allowed.
    CannotShrink,
}

impl Node {
    /// Whether window `id` is in this tree.
    pub fn contains(&self, id: WindowId) -> bool {
        self.path_to(id).is_some()
    }

    /// Child indices leading from this node to the leaf `id`.
    pub fn path_to(&self, id: WindowId) -> Option<Vec<usize>> {
        match self {
            Node::Leaf(w) => (*w == id).then(Vec::new),
            Node::Split { children, .. } => children.iter().enumerate().find_map(|(i, c)| {
                c.path_to(id).map(|mut p| {
                    p.insert(0, i);
                    p
                })
            }),
        }
    }

    fn at(&self, path: &[usize]) -> &Node {
        match (self, path.split_first()) {
            (Node::Split { children, .. }, Some((&i, rest))) => children[i].at(rest),
            _ => self,
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut Node {
        match path.split_first() {
            None => self,
            Some((&i, rest)) => match self {
                Node::Split { children, .. } => children[i].at_mut(rest),
                Node::Leaf(_) => self,
            },
        }
    }

    /// The first window in tree order (top left).
    pub fn first_window(&self) -> WindowId {
        match self {
            Node::Leaf(w) => *w,
            Node::Split { children, .. } => children[0].first_window(),
        }
    }

    /// The last window in tree order (bottom right).
    pub fn last_window(&self) -> WindowId {
        match self {
            Node::Leaf(w) => *w,
            Node::Split { children, .. } => children[children.len() - 1].last_window(),
        }
    }

    /// The smallest this subtree can be along `dir` with every window at its
    /// minimum size.
    pub fn min_extent(&self, dir: SplitDir) -> f32 {
        match self {
            Node::Leaf(_) => dir.min_extent(),
            Node::Split { dir: d, children, .. } => {
                let mins = children.iter().map(|c| c.min_extent(dir));
                if *d == dir { mins.sum() } else { mins.fold(0.0, f32::max) }
            }
        }
    }

    /// How many windows this subtree has side by side along `dir` (what
    /// balancing shares space by).
    pub fn weight(&self, dir: SplitDir) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { dir: d, children, .. } => {
                let ws = children.iter().map(|c| c.weight(dir));
                if *d == dir { ws.sum() } else { ws.max().unwrap_or(1) }
            }
        }
    }

    /// Split window `target` in two along `dir`: `new` goes after it (below
    /// or to the right) and each gets half of `target`'s space. Splitting
    /// inside a split of the same direction adds a sibling rather than
    /// nesting. Returns false if `target` isn't in the tree.
    pub fn split_window(&mut self, target: WindowId, new: WindowId, dir: SplitDir) -> bool {
        let Some(path) = self.path_to(target) else { return false };
        if let Some((&i, parent)) = path.split_last() {
            if let Node::Split { dir: d, children, ratios } = self.at_mut(parent) {
                if *d == dir {
                    let half = ratios[i] / 2.0;
                    ratios[i] = half;
                    ratios.insert(i + 1, half);
                    children.insert(i + 1, Node::Leaf(new));
                    return true;
                }
            }
        }
        *self.at_mut(&path) = Node::Split {
            dir,
            children: vec![Node::Leaf(target), Node::Leaf(new)],
            ratios: vec![0.5, 0.5],
        };
        true
    }

    /// Remove window `target`. Its space goes to the sibling before it (or
    /// after it, if it was first), like Emacs; a split left with one child is
    /// replaced by that child. Returns the window next to where `target` was
    /// (the one to select instead), or `None` if `target` is the whole tree
    /// or isn't in it.
    pub fn remove_window(&mut self, target: WindowId) -> Option<WindowId> {
        let path = self.path_to(target)?;
        let (&i, parent_path) = path.split_last()?;
        let parent = self.at_mut(parent_path);
        let Node::Split { children, ratios, .. } = parent else { return None };
        children.remove(i);
        let share = ratios.remove(i);
        let (heir, next) = if i > 0 { (i - 1, children[i - 1].last_window()) } else { (0, children[0].first_window()) };
        ratios[heir] += share;
        normalize(ratios);
        if children.len() == 1 {
            let only = children.pop().expect("one child");
            *parent = only;
        }
        self.flatten();
        Some(next)
    }

    /// Merge splits nested in a split of the same direction into it (their
    /// children become its children, with their shares scaled).
    pub fn flatten(&mut self) {
        let Node::Split { dir, children, ratios } = self else { return };
        for c in children.iter_mut() {
            c.flatten();
        }
        if !children.iter().any(|c| matches!(c, Node::Split { dir: d, .. } if d == dir)) {
            return;
        }
        let mut new_children = Vec::new();
        let mut new_ratios = Vec::new();
        for (c, r) in std::mem::take(children).into_iter().zip(std::mem::take(ratios)) {
            match c {
                Node::Split { dir: d, children: cc, ratios: cr } if d == *dir => {
                    new_children.extend(cc);
                    new_ratios.extend(cr.into_iter().map(|x| x * r));
                }
                c => {
                    new_children.push(c);
                    new_ratios.push(r);
                }
            }
        }
        normalize(&mut new_ratios);
        *children = new_children;
        *ratios = new_ratios;
    }

    /// Give every window the same size: each child of a split gets space in
    /// proportion to how many windows it has along the split.
    pub fn balance(&mut self) {
        if let Node::Split { dir, children, ratios } = self {
            let weights: Vec<f32> = children.iter().map(|c| c.weight(*dir) as f32).collect();
            let total: f32 = weights.iter().sum();
            for (r, w) in ratios.iter_mut().zip(&weights) {
                *r = w / total;
            }
            children.iter_mut().for_each(Node::balance);
        }
    }

    /// Check the structural invariants: splits have ≥2 children, one ratio
    /// each, every ratio > 0, summing to 1; no window appears twice.
    pub fn check(&self) -> Result<(), String> {
        let ws = self.windows();
        let mut sorted = ws.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != ws.len() {
            return Err(format!("a window appears twice in {ws:?}"));
        }
        self.check_splits()
    }

    fn check_splits(&self) -> Result<(), String> {
        let Node::Split { children, ratios, .. } = self else { return Ok(()) };
        if children.len() < 2 {
            return Err(format!("split with {} children", children.len()));
        }
        if ratios.len() != children.len() {
            return Err(format!("{} ratios for {} children", ratios.len(), children.len()));
        }
        if ratios.iter().any(|r| !(*r > 0.0)) {
            return Err(format!("non-positive ratio in {ratios:?}"));
        }
        let sum: f32 = ratios.iter().sum();
        if (sum - 1.0).abs() > 1e-3 {
            return Err(format!("ratios {ratios:?} sum to {sum}"));
        }
        children.iter().try_for_each(Node::check_splits)
    }
}

/// Scale `ratios` to sum to 1.
fn normalize(ratios: &mut [f32]) {
    let sum: f32 = ratios.iter().sum();
    if sum > 0.0 {
        ratios.iter_mut().for_each(|r| *r /= sum);
    }
}

/// The rectangles of a split's children in `area`. Boundaries are rounded to
/// whole units (cells or pixels) from `area`'s origin, and the last child
/// ends exactly at `area`'s edge, so the children tile it exactly.
fn child_rects(dir: SplitDir, ratios: &[f32], area: Rect) -> Vec<Rect> {
    let total = dir.extent(area);
    let n = ratios.len();
    let mut out = Vec::with_capacity(n);
    let mut cum = 0.0;
    let mut start = 0.0;
    for (i, r) in ratios.iter().enumerate() {
        cum += r;
        let end = if i + 1 == n { total } else { (cum * total).round().clamp(start, total) };
        out.push(match dir {
            SplitDir::Horizontal => Rect { x: area.x + start, w: end - start, ..area },
            SplitDir::Vertical => Rect { y: area.y + start, h: end - start, ..area },
        });
        start = end;
    }
    out
}

/// The window rectangles of `root` laid out in `area`, in tree order. They
/// tile `area` exactly: no gaps, no overlaps.
pub fn compute_rects(root: &Node, area: Rect) -> Vec<(WindowId, Rect)> {
    let mut out = Vec::new();
    collect_rects(root, area, &mut out);
    out
}

fn collect_rects(node: &Node, area: Rect, out: &mut Vec<(WindowId, Rect)>) {
    match node {
        Node::Leaf(w) => out.push((*w, area)),
        Node::Split { dir, children, ratios } => {
            for (c, r) in children.iter().zip(child_rects(*dir, ratios, area)) {
                collect_rects(c, r, out);
            }
        }
    }
}

/// The rectangle of the node at `path` when `root` fills `area`.
fn rect_at(root: &Node, area: Rect, path: &[usize]) -> Rect {
    match (root, path.split_first()) {
        (Node::Split { dir, children, ratios }, Some((&i, rest))) => {
            rect_at(&children[i], child_rects(*dir, ratios, area)[i], rest)
        }
        _ => area,
    }
}

/// The rectangle of window `id` when `root` fills `area`.
pub fn window_rect(root: &Node, area: Rect, id: WindowId) -> Option<Rect> {
    root.path_to(id).map(|p| rect_at(root, area, &p))
}

/// Grow window `target` by `delta` cells along `dir` (shrink it if
/// negative), within the nearest enclosing split of that direction. Space is
/// taken from the following siblings first, nearest first, then the
/// preceding ones; shrinking gives the space to the next sibling (or the
/// previous one if it is last). The change is clamped so that every window
/// keeps its minimum size; returns the signed number of cells actually moved.
pub fn resize_window(root: &mut Node, area: Rect, target: WindowId, dir: SplitDir, delta: i64) -> Result<i64, ResizeError> {
    let path = root.path_to(target).ok_or(ResizeError::NoWindow)?;
    let depth = (0..path.len())
        .rev()
        .find(|&k| matches!(root.at(&path[..k]), Node::Split { dir: d, .. } if *d == dir))
        .ok_or(ResizeError::NoSplit)?;
    let (split_path, idx) = (&path[..depth], path[depth]);
    let split_area = rect_at(root, area, split_path);
    let Node::Split { children, ratios, .. } = root.at_mut(split_path) else { unreachable!("found a split") };

    let mut sizes: Vec<i64> = child_rects(dir, ratios, split_area).iter().map(|r| dir.extent(*r).round() as i64).collect();
    let mins: Vec<i64> = children.iter().map(|c| c.min_extent(dir).round() as i64).collect();
    let slack = |sizes: &[i64], j: usize| (sizes[j] - mins[j]).max(0);
    let moved = if delta > 0 {
        let mut left = delta;
        for j in (idx + 1..sizes.len()).chain((0..idx).rev()) {
            let take = left.min(slack(&sizes, j));
            sizes[j] -= take;
            sizes[idx] += take;
            left -= take;
        }
        let moved = delta - left;
        if moved == 0 {
            return Err(ResizeError::CannotEnlarge);
        }
        moved
    } else if delta < 0 {
        let moved = (-delta).min(slack(&sizes, idx));
        if moved == 0 {
            return Err(ResizeError::CannotShrink);
        }
        let receiver = if idx + 1 < sizes.len() { idx + 1 } else { idx - 1 };
        sizes[idx] -= moved;
        sizes[receiver] += moved;
        -moved
    } else {
        return Ok(0);
    };
    set_ratios(ratios, &sizes);
    fit(root, area);
    Ok(moved)
}

/// Ratios proportional to `sizes` (all > 0).
fn set_ratios(ratios: &mut [f32], sizes: &[i64]) {
    let total: i64 = sizes.iter().sum();
    if total <= 0 {
        return;
    }
    for (r, s) in ratios.iter_mut().zip(sizes) {
        *r = (*s).max(1) as f32 / total as f32;
    }
    normalize(ratios);
}

/// Make every window in `node` at least its minimum size where the space
/// allows, taking the shortfall from the siblings with the most room. Used
/// after a resize, which scales nested windows proportionally and so could
/// leave a small one below the minimum.
pub fn fit(node: &mut Node, area: Rect) {
    let Node::Split { dir, children, ratios } = node else { return };
    let rects = child_rects(*dir, ratios, area);
    let mut sizes: Vec<i64> = rects.iter().map(|r| dir.extent(*r).round() as i64).collect();
    let mins: Vec<i64> = children.iter().map(|c| c.min_extent(*dir).round() as i64).collect();
    let fits = mins.iter().sum::<i64>() <= sizes.iter().sum::<i64>();
    let mut changed = false;
    while fits {
        let Some(short) = (0..sizes.len()).find(|&j| sizes[j] < mins[j]) else { break };
        let Some(donor) = (0..sizes.len()).filter(|&j| sizes[j] > mins[j]).max_by_key(|&j| sizes[j] - mins[j]) else { break };
        let n = (mins[short] - sizes[short]).min(sizes[donor] - mins[donor]);
        sizes[short] += n;
        sizes[donor] -= n;
        changed = true;
    }
    if changed {
        set_ratios(ratios, &sizes);
    }
    let rects = child_rects(*dir, ratios, area);
    for (c, r) in children.iter_mut().zip(rects) {
        fit(c, r);
    }
}

/// The window next to `from` in direction `towards` when `root` fills
/// `area`: of the windows beyond that edge whose span overlaps `from`'s, the
/// nearest; among equally near ones, the one at `reference` (a coordinate
/// across the direction of travel, e.g. the cursor's row for left/right),
/// else the one whose span is closest to it.
pub fn neighbour(root: &Node, area: Rect, from: WindowId, towards: Direction, reference: f32) -> Option<WindowId> {
    let rects = compute_rects(root, area);
    let s = rects.iter().find(|(w, _)| *w == from)?.1;
    // For each candidate: (distance from `from`'s edge, its span across the travel direction).
    let candidates = rects.iter().filter(|(w, _)| *w != from).filter_map(|(w, r)| {
        let (gap, lo, hi, slo, shi) = match towards {
            Direction::Left => (s.x - (r.x + r.w), r.y, r.y + r.h, s.y, s.y + s.h),
            Direction::Right => (r.x - (s.x + s.w), r.y, r.y + r.h, s.y, s.y + s.h),
            Direction::Up => (s.y - (r.y + r.h), r.x, r.x + r.w, s.x, s.x + s.w),
            Direction::Down => (r.y - (s.y + s.h), r.x, r.x + r.w, s.x, s.x + s.w),
        };
        let overlaps = lo < shi - EPS && hi > slo + EPS;
        (gap > -EPS && overlaps).then_some((*w, gap, lo, hi))
    });
    let candidates: Vec<_> = candidates.collect();
    let nearest = candidates.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
    let off_by = |lo: f32, hi: f32| if reference < lo { lo - reference } else if reference >= hi { reference - hi + EPS } else { 0.0 };
    candidates
        .iter()
        .filter(|c| c.1 <= nearest + EPS)
        .min_by(|a, b| off_by(a.2, a.3).total_cmp(&off_by(b.2, b.3)))
        .map(|c| c.0)
}
