//! Tests for windows and tabs: the pure tree operations and geometry, then
//! the editor side driven through commands and real key sequences.

use std::cell::RefCell;
use std::rc::Rc;

use super::tree::{Direction, ResizeError, fit, neighbour, resize_window};
use super::*;
use crate::engine::editor::{Args, RawArg};
use crate::engine::{Buffer, Dispatcher, Editor, EventKind, Key, Outcome};

fn w(n: u64) -> WindowId {
    WindowId(n)
}

fn leaf(n: u64) -> Node {
    Node::Leaf(w(n))
}

fn split(dir: SplitDir, children: Vec<Node>, ratios: &[f32]) -> Node {
    Node::Split { dir, children, ratios: ratios.to_vec() }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

const H: SplitDir = SplitDir::Horizontal;
const V: SplitDir = SplitDir::Vertical;

fn ratios_of(n: &Node) -> Vec<f32> {
    match n {
        Node::Split { ratios, .. } => ratios.clone(),
        Node::Leaf(_) => vec![],
    }
}

fn approx(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

/// The rects tile `area` exactly: inside it, no overlaps, areas add up.
fn assert_tiles(rects: &[(WindowId, Rect)], area: Rect) {
    let mut total = 0.0;
    for (i, (_, a)) in rects.iter().enumerate() {
        assert!(a.w >= 0.0 && a.h >= 0.0, "{a:?}");
        assert!(a.x >= area.x - 1e-3 && a.y >= area.y - 1e-3, "{a:?} outside {area:?}");
        assert!(a.x + a.w <= area.x + area.w + 1e-3 && a.y + a.h <= area.y + area.h + 1e-3, "{a:?} outside {area:?}");
        total += a.w * a.h;
        for (_, b) in &rects[i + 1..] {
            let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
            let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
            assert!(ox <= 1e-3 || oy <= 1e-3, "{a:?} overlaps {b:?}");
        }
    }
    assert!((total - area.w * area.h).abs() < 1e-2 * area.w.max(1.0), "areas sum to {total}, not {}", area.w * area.h);
}

/// A tiny deterministic random generator (no external crates).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

// ---------------------------------------------------------------- geometry

#[test]
fn a_single_window_fills_the_area() {
    let a = rect(3.0, 4.0, 50.0, 20.0);
    assert_eq!(compute_rects(&leaf(1), a), [(w(1), a)]);
}

#[test]
fn splits_divide_by_ratio_and_the_last_child_absorbs_rounding() {
    let n = split(H, vec![leaf(1), leaf(2), leaf(3)], &[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]);
    let r = compute_rects(&n, rect(0.0, 0.0, 10.0, 5.0));
    assert_eq!(r[0], (w(1), rect(0.0, 0.0, 3.0, 5.0)));
    assert_eq!(r[1], (w(2), rect(3.0, 0.0, 4.0, 5.0)));
    assert_eq!(r[2], (w(3), rect(7.0, 0.0, 3.0, 5.0)));

    // A fractional area: inner boundaries are whole units from the origin, the edge is exact.
    let r = compute_rects(&n, rect(0.5, 0.0, 10.25, 1.0));
    assert_eq!(r[0].1.x, 0.5);
    assert_eq!(r[1].1.x, 3.5);
    assert_eq!(r[2].1.x + r[2].1.w, 10.75);
    assert_tiles(&r, rect(0.5, 0.0, 10.25, 1.0));
}

#[test]
fn nested_splits_lay_out_in_tree_order() {
    // | 1 | 2 |
    // |   |---|
    // |   | 3 |
    let n = split(H, vec![leaf(1), split(V, vec![leaf(2), leaf(3)], &[0.5, 0.5])], &[0.5, 0.5]);
    let r = compute_rects(&n, rect(0.0, 0.0, 80.0, 24.0));
    assert_eq!(
        r,
        [(w(1), rect(0.0, 0.0, 40.0, 24.0)), (w(2), rect(40.0, 0.0, 40.0, 12.0)), (w(3), rect(40.0, 12.0, 40.0, 12.0))]
    );
    assert_eq!(window_rect(&n, rect(0.0, 0.0, 80.0, 24.0), w(3)), Some(rect(40.0, 12.0, 40.0, 12.0)));
    assert_eq!(window_rect(&n, rect(0.0, 0.0, 80.0, 24.0), w(9)), None);
}

#[test]
fn random_trees_always_tile_exactly() {
    let mut rng = Lcg(7);
    for round in 0..300 {
        let mut root = leaf(1);
        for next in (2..).take(rng.below(12) as usize) {
            let ws = root.windows();
            let target = ws[rng.below(ws.len() as u64) as usize];
            let dir = if rng.below(2) == 0 { H } else { V };
            assert!(root.split_window(target, w(next), dir));
        }
        // Random unequal ratios.
        perturb(&mut root, &mut rng);
        root.check().unwrap_or_else(|e| panic!("round {round}: {e}: {root:?}"));
        let area =
            rect(rng.below(5) as f32, rng.below(5) as f32, 1.0 + rng.below(300) as f32, 1.0 + rng.below(100) as f32);
        let rects = compute_rects(&root, area);
        assert_eq!(rects.iter().map(|r| r.0).collect::<Vec<_>>(), root.windows());
        assert_tiles(&rects, area);
    }
}

fn perturb(n: &mut Node, rng: &mut Lcg) {
    if let Node::Split { children, ratios, .. } = n {
        for r in ratios.iter_mut() {
            *r = 1.0 + rng.below(9) as f32;
        }
        let s: f32 = ratios.iter().sum();
        ratios.iter_mut().for_each(|r| *r /= s);
        children.iter_mut().for_each(|c| perturb(c, rng));
    }
}

#[test]
fn splitting_inside_a_same_direction_split_adds_a_sibling() {
    let mut n = leaf(1);
    assert!(n.split_window(w(1), w(2), V));
    assert_eq!(n, split(V, vec![leaf(1), leaf(2)], &[0.5, 0.5]));
    assert!(n.split_window(w(1), w(3), V));
    assert_eq!(n, split(V, vec![leaf(1), leaf(3), leaf(2)], &[0.25, 0.25, 0.5]));
    // The other direction nests.
    assert!(n.split_window(w(2), w(4), H));
    assert_eq!(n, split(V, vec![leaf(1), leaf(3), split(H, vec![leaf(2), leaf(4)], &[0.5, 0.5])], &[0.25, 0.25, 0.5]));
    assert!(!n.split_window(w(99), w(5), H));
    n.check().unwrap();
}

#[test]
fn removing_gives_the_space_to_the_previous_sibling_and_collapses() {
    let mut n = split(V, vec![leaf(1), leaf(2), leaf(3)], &[0.2, 0.3, 0.5]);
    assert_eq!(n.remove_window(w(2)), Some(w(1)));
    assert!(approx(&ratios_of(&n), &[0.5, 0.5]));
    // The first child gives its space to the next one.
    assert_eq!(n.remove_window(w(1)), Some(w(3)));
    assert_eq!(n, leaf(3));
    // The sole window can't go; an absent one isn't found.
    assert_eq!(n.remove_window(w(3)), None);
    assert_eq!(n.remove_window(w(7)), None);
}

#[test]
fn removing_flattens_a_lone_child_into_its_grandparent() {
    // V[1, H[2, V[3, 4]]]: removing 2 leaves V[1, V[3,4]] -> V[1, 3, 4].
    let inner = split(V, vec![leaf(3), leaf(4)], &[0.5, 0.5]);
    let mut n = split(V, vec![leaf(1), split(H, vec![leaf(2), inner], &[0.5, 0.5])], &[0.5, 0.5]);
    assert_eq!(n.remove_window(w(2)), Some(w(3)));
    assert_eq!(n.windows(), [w(1), w(3), w(4)]);
    assert!(approx(&ratios_of(&n), &[0.5, 0.25, 0.25]));
    n.check().unwrap();
    // Removing the last child of a split selects the end of the previous sibling.
    let mut n = split(H, vec![split(V, vec![leaf(1), leaf(2)], &[0.5, 0.5]), leaf(3)], &[0.5, 0.5]);
    assert_eq!(n.remove_window(w(3)), Some(w(2)));
    assert_eq!(n, split(V, vec![leaf(1), leaf(2)], &[0.5, 0.5]));
}

#[test]
fn balance_shares_by_windows_across() {
    // H[1, V[2, 3], H-flattened...]: widths by columns, heights by rows.
    let mut n = split(H, vec![leaf(1), split(V, vec![leaf(2), leaf(3), leaf(4)], &[0.8, 0.1, 0.1])], &[0.9, 0.1]);
    n.balance();
    assert!(approx(&ratios_of(&n), &[0.5, 0.5]));
    if let Node::Split { children, .. } = &n {
        assert!(approx(&ratios_of(&children[1]), &[1.0 / 3.0; 3]));
    }
    // A child with two columns gets twice the width.
    let mut n = split(V, vec![leaf(1), split(H, vec![leaf(2), leaf(3)], &[0.5, 0.5])], &[0.3, 0.7]);
    n.balance();
    assert!(approx(&ratios_of(&n), &[0.5, 0.5]));
    let mut n = split(
        H,
        vec![leaf(1), split(V, vec![leaf(2), split(H, vec![leaf(3), leaf(4)], &[0.1, 0.9])], &[0.5, 0.5])],
        &[0.5, 0.5],
    );
    n.balance();
    assert!(approx(&ratios_of(&n), &[1.0 / 3.0, 2.0 / 3.0]));
    n.check().unwrap();
}

#[test]
fn min_extent_and_weight() {
    let n = split(H, vec![leaf(1), split(V, vec![leaf(2), leaf(3)], &[0.5, 0.5])], &[0.5, 0.5]);
    assert_eq!(n.min_extent(H), 2.0 * MIN_COLS);
    assert_eq!(n.min_extent(V), 2.0 * MIN_ROWS);
    assert_eq!(n.weight(H), 2);
    assert_eq!(n.weight(V), 2);
    assert_eq!(leaf(1).weight(V), 1);
    assert_eq!(n.first_window(), w(1));
    assert_eq!(n.last_window(), w(3));
    assert_eq!(n.path_to(w(3)), Some(vec![1, 1]));
    assert!(n.contains(w(2)) && !n.contains(w(4)));
}

#[test]
fn check_catches_broken_trees() {
    assert!(split(V, vec![leaf(1)], &[1.0]).check().is_err());
    assert!(split(V, vec![leaf(1), leaf(2)], &[0.5]).check().is_err());
    assert!(split(V, vec![leaf(1), leaf(2)], &[0.0, 1.0]).check().is_err());
    assert!(split(V, vec![leaf(1), leaf(2)], &[0.5, 0.6]).check().is_err());
    assert!(split(V, vec![leaf(1), leaf(1)], &[0.5, 0.5]).check().is_err());
    assert!(split(V, vec![leaf(1), leaf(2)], &[0.5, 0.5]).check().is_ok());
}

#[test]
fn resizing_moves_lines_between_siblings_and_clamps() {
    let area = rect(0.0, 0.0, 80.0, 24.0);
    let mut n = split(V, vec![leaf(1), leaf(2)], &[0.5, 0.5]);
    assert_eq!(resize_window(&mut n, area, w(1), V, 3), Ok(3));
    assert_eq!(window_rect(&n, area, w(1)).unwrap().h, 15.0);
    assert_eq!(resize_window(&mut n, area, w(1), V, -5), Ok(-5));
    assert_eq!(window_rect(&n, area, w(1)).unwrap().h, 10.0);
    // Clamped: the other window keeps MIN_ROWS lines.
    assert_eq!(resize_window(&mut n, area, w(1), V, 100), Ok(10));
    assert_eq!(window_rect(&n, area, w(2)).unwrap().h, MIN_ROWS);
    assert_eq!(resize_window(&mut n, area, w(1), V, 1), Err(ResizeError::CannotEnlarge));
    assert_eq!(resize_window(&mut n, area, w(2), V, -1), Err(ResizeError::CannotShrink));
    assert_eq!(resize_window(&mut n, area, w(1), V, -100), Ok(-16));
    assert_eq!(window_rect(&n, area, w(1)).unwrap().h, MIN_ROWS);
    // No split across: nothing to resize against.
    assert_eq!(resize_window(&mut n, area, w(1), H, 1), Err(ResizeError::NoSplit));
    assert_eq!(resize_window(&mut n, area, w(9), V, 1), Err(ResizeError::NoWindow));
    assert_eq!(resize_window(&mut n, area, w(1), V, 0), Ok(0));
    n.check().unwrap();
}

#[test]
fn enlarging_takes_from_following_siblings_first() {
    let area = rect(0.0, 0.0, 90.0, 10.0);
    let mut n = split(H, vec![leaf(1), leaf(2), leaf(3)], &[1.0 / 3.0; 3]);
    assert_eq!(resize_window(&mut n, area, w(2), H, 25), Ok(25));
    let widths: Vec<f32> = compute_rects(&n, area).iter().map(|r| r.1.w).collect();
    // 3 gives its 20 spare columns first, then 1 gives 5.
    assert_eq!(widths, [25.0, 55.0, 10.0]);
    // Shrinking the last window gives its space to the previous one.
    assert_eq!(resize_window(&mut n, area, w(3), H, 5), Ok(5));
    assert_eq!(resize_window(&mut n, area, w(3), H, -5), Ok(-5));
    let widths: Vec<f32> = compute_rects(&n, area).iter().map(|r| r.1.w).collect();
    assert_eq!(widths, [25.0, 55.0, 10.0]);
}

#[test]
fn resizing_a_nested_window_uses_the_nearest_split_of_that_direction() {
    let area = rect(0.0, 0.0, 80.0, 24.0);
    // V[1, H[2, 3]]: making 2 taller grows the whole bottom row.
    let mut n = split(V, vec![leaf(1), split(H, vec![leaf(2), leaf(3)], &[0.5, 0.5])], &[0.5, 0.5]);
    assert_eq!(resize_window(&mut n, area, w(2), V, 4), Ok(4));
    assert_eq!(window_rect(&n, area, w(3)).unwrap().h, 16.0);
    assert_eq!(window_rect(&n, area, w(1)).unwrap().h, 8.0);
    // ...and making it wider moves the column boundary in the bottom row only.
    assert_eq!(resize_window(&mut n, area, w(2), H, 10), Ok(10));
    assert_eq!(window_rect(&n, area, w(2)).unwrap().w, 50.0);
    assert_eq!(window_rect(&n, area, w(1)).unwrap().w, 80.0);
}

#[test]
fn shrinking_a_sibling_subtree_keeps_its_windows_at_their_minimum() {
    let area = rect(0.0, 0.0, 80.0, 40.0);
    // H[1, V[2 (90%), 3 (10%)]] inside V[top, that]: growing `top` squeezes 3.
    let inner = split(V, vec![leaf(2), leaf(3)], &[0.9, 0.1]);
    let mut n = split(V, vec![leaf(1), split(H, vec![leaf(4), inner], &[0.5, 0.5])], &[0.5, 0.5]);
    fit(&mut n, area);
    resize_window(&mut n, area, w(1), V, 100).unwrap();
    for (id, r) in compute_rects(&n, area) {
        assert!(r.h >= MIN_ROWS, "{id:?} is {} lines", r.h);
        assert!(r.w >= MIN_COLS, "{id:?} is {} columns", r.w);
    }
    // The bottom row is exactly as tall as its tallest minimum (two stacked windows).
    assert_eq!(window_rect(&n, area, w(4)).unwrap().h, 2.0 * MIN_ROWS);
    n.check().unwrap();
}

#[test]
fn random_resizes_never_break_minimums() {
    let area = rect(0.0, 0.0, 120.0, 50.0);
    let mut rng = Lcg(42);
    for _ in 0..200 {
        let mut root = leaf(1);
        let mut next = 2;
        for _ in 0..rng.below(6) {
            let ws = root.windows();
            let t = ws[rng.below(ws.len() as u64) as usize];
            let r = window_rect(&root, area, t).unwrap();
            let dir = if rng.below(2) == 0 { H } else { V };
            if dir.extent(r) >= 2.0 * dir.min_extent() {
                root.split_window(t, w(next), dir);
                next += 1;
            }
        }
        for _ in 0..10 {
            let ws = root.windows();
            let t = ws[rng.below(ws.len() as u64) as usize];
            let dir = if rng.below(2) == 0 { H } else { V };
            let delta = rng.below(60) as i64 - 30;
            let _ = resize_window(&mut root, area, t, dir, delta);
            root.check().unwrap();
            let rects = compute_rects(&root, area);
            assert_tiles(&rects, area);
            for (id, r) in rects {
                assert!(r.h >= MIN_ROWS && r.w >= MIN_COLS, "{id:?} too small: {r:?} in {root:?}");
            }
        }
    }
}

#[test]
fn neighbours_in_an_irregular_layout() {
    // +----+----+----+
    // |    | 2  |    |
    // | 1  +----+ 4  |
    // |    | 3  |    |
    // +----+----+----+
    // |      5       |
    // +--------------+
    let area = rect(0.0, 0.0, 90.0, 30.0);
    let mid = split(V, vec![leaf(2), leaf(3)], &[0.5, 0.5]);
    let top = split(H, vec![leaf(1), mid, leaf(4)], &[1.0 / 3.0; 3]);
    let root = split(V, vec![top, leaf(5)], &[2.0 / 3.0, 1.0 / 3.0]);
    let nb = |from, dir, reference| neighbour(&root, area, w(from), dir, reference);
    // From 1, rightwards: 2 or 3, by the reference row.
    assert_eq!(nb(1, Direction::Right, 2.5), Some(w(2)));
    assert_eq!(nb(1, Direction::Right, 15.5), Some(w(3)));
    assert_eq!(nb(4, Direction::Left, 15.5), Some(w(3)));
    assert_eq!(nb(2, Direction::Right, 1.0), Some(w(4)));
    assert_eq!(nb(2, Direction::Down, 40.0), Some(w(3)));
    assert_eq!(nb(3, Direction::Up, 40.0), Some(w(2)));
    assert_eq!(nb(3, Direction::Down, 40.0), Some(w(5)));
    // From the bottom, upwards: by the reference column.
    assert_eq!(nb(5, Direction::Up, 5.0), Some(w(1)));
    assert_eq!(nb(5, Direction::Up, 45.0), Some(w(3)));
    assert_eq!(nb(5, Direction::Up, 85.0), Some(w(4)));
    // Nothing beyond the frame's edges.
    assert_eq!(nb(1, Direction::Left, 1.0), None);
    assert_eq!(nb(4, Direction::Right, 1.0), None);
    assert_eq!(nb(2, Direction::Up, 40.0), None);
    assert_eq!(nb(5, Direction::Down, 1.0), None);
    // A reference outside every candidate's span picks the closest.
    assert_eq!(nb(1, Direction::Right, 1000.0), Some(w(3)));
    assert_eq!(neighbour(&root, area, w(9), Direction::Up, 0.0), None);
}

#[test]
fn neighbours_must_overlap_the_selected_span() {
    // V[H[1, 2], H[3, 4, 5]] with unaligned columns: from 2 down, the windows under it.
    let area = rect(0.0, 0.0, 60.0, 20.0);
    let root = split(
        V,
        vec![split(H, vec![leaf(1), leaf(2)], &[0.5, 0.5]), split(H, vec![leaf(3), leaf(4), leaf(5)], &[1.0 / 3.0; 3])],
        &[0.5, 0.5],
    );
    assert_eq!(neighbour(&root, area, w(2), Direction::Down, 35.0), Some(w(4)));
    assert_eq!(neighbour(&root, area, w(2), Direction::Down, 55.0), Some(w(5)));
    // 3 (0..20) is under 1 only; 4 (20..40) is under both.
    assert_eq!(neighbour(&root, area, w(3), Direction::Up, 5.0), Some(w(1)));
    assert_eq!(neighbour(&root, area, w(4), Direction::Up, 35.0), Some(w(2)));
    assert_eq!(neighbour(&root, area, w(1), Direction::Right, 1.0), Some(w(2)));
}

// ------------------------------------------------------------------ layout

#[test]
fn new_layout_and_ids() {
    let mut l = Layout::new(7);
    assert_eq!(l.selected(), WindowId(1));
    assert_eq!(l.tab().root.windows(), [WindowId(1)]);
    assert_eq!(l.windows_showing(7), [WindowId(1)]);
    assert_eq!(l.alloc_window_id(), WindowId(2));
    l.reserve_window_id(WindowId(10));
    assert_eq!(l.alloc_window_id(), WindowId(11));
    assert_eq!(l.frame_size(), DEFAULT_FRAME);
    l.check().unwrap();
}

#[test]
fn frame_size_is_whole_and_positive() {
    let mut l = Layout::new(1);
    l.set_frame_size(100.7, 30.2);
    assert_eq!(l.frame_size(), (100.0, 30.0));
    l.set_frame_size(0.0, -3.0);
    assert_eq!(l.frame_size(), (1.0, 1.0));
    l.set_frame_size(f32::NAN, f32::INFINITY);
    assert_eq!(l.frame_size(), (1.0, 1.0));
    assert_eq!(l.window_rects(), [(w(1), rect(0.0, 0.0, 1.0, 1.0))]);
}

#[test]
fn layout_check_catches_broken_layouts() {
    let mut l = Layout::new(1);
    l.tab_mut().selected = w(5);
    assert!(l.check().is_err());
    let mut l = Layout::new(1);
    l.windows.insert(w(5), Window::new(w(5), 1));
    assert!(l.check().is_err(), "a window in no tab");
    let mut l = Layout::new(1);
    l.current_tab = 3;
    assert!(l.check().is_err());
    let mut l = Layout::new(1);
    l.tabs.push(Tab { name: None, root: leaf(1), selected: w(1) });
    assert!(l.check().is_err(), "a window in two tabs");
}

#[test]
fn unselected_windows_follow_edits_and_kills() {
    let mut l = Layout::new(1);
    let other = l.alloc_window_id();
    let mut win = Window::new(other, 1);
    win.cursors[0].point = 10;
    l.windows.insert(other, win);
    l.adjust_for_edit(1, 2, 4, 5); // replace 2 chars by 5 before point
    assert_eq!(l.window(other).unwrap().cursors[0].point, 13);
    l.adjust_for_edit(2, 0, 0, 100); // another buffer: untouched
    assert_eq!(l.window(other).unwrap().cursors[0].point, 13);
    assert!(l.replace_buffer(1, 5));
    assert_eq!(l.window(other).unwrap().buffer, 5);
    assert_eq!(l.window(other).unwrap().cursors[0].point, 0);
    assert_eq!(l.window(WindowId(1)).unwrap().buffer, 1); // selected: the editor handles it
    assert!(!l.replace_buffer(9, 5));
}

#[test]
fn node_serializes_as_plain_data() {
    let n = Node::Split {
        dir: SplitDir::Horizontal,
        children: vec![Node::Leaf(WindowId(1)), Node::Leaf(WindowId(2))],
        ratios: vec![0.5, 0.5],
    };
    let j = serde_json::to_value(&n).unwrap();
    assert_eq!(j["split"]["dir"], "horizontal");
    assert_eq!(serde_json::from_value::<Node>(j).unwrap(), n);
    assert_eq!(n.windows(), [WindowId(1), WindowId(2)]);
}

// ------------------------------------------------------------------ editor

/// An editor plus a key dispatcher, recording window-configuration events.
struct E {
    ed: Editor,
    d: Dispatcher,
    changes: Rc<RefCell<usize>>,
}

fn editor(text: &str) -> E {
    let mut ed = Editor::new(Buffer::from_str("a", text));
    let changes = Rc::new(RefCell::new(0));
    let c = changes.clone();
    ed.hooks.add(
        EventKind::WindowConfigurationChanged,
        "count",
        Rc::new(move |_, _| {
            *c.borrow_mut() += 1;
            Ok(())
        }),
    );
    ed.run_hooks();
    E { ed, d: Dispatcher::new(), changes }
}

impl E {
    fn keys(&mut self, keys: &str) -> &mut Self {
        for k in keys.split_whitespace() {
            if let Outcome::Run(cmd, args) = self.d.dispatch(&Key::parse(k), &self.ed.active_keymaps()) {
                self.ed.execute(cmd, args);
            }
        }
        self.check();
        self
    }

    fn run(&mut self, cmd: &str) -> &mut Self {
        self.run_with(cmd, Args::default())
    }

    fn run_n(&mut self, cmd: &str, n: i64) -> &mut Self {
        self.run_with(cmd, Args::with_raw(Some(RawArg::Number(n))))
    }

    fn run_with(&mut self, cmd: &str, args: Args) -> &mut Self {
        self.ed.execute(cmd, args);
        self.check();
        self
    }

    /// Layout invariants, plus: the selected window shows the current buffer.
    fn check(&self) {
        self.ed.layout.check().unwrap();
        let sel = self.ed.layout.selected();
        assert_eq!(
            self.ed.layout.window(sel).unwrap().buffer,
            self.ed.buf_id,
            "selected window shows the current buffer"
        );
        for win in self.ed.layout.windows.values() {
            assert!(self.ed.find_buffer(win.buffer).is_some(), "{:?} shows a killed buffer", win.id);
        }
    }

    /// Window-configuration events since the last call.
    fn changes(&mut self) -> usize {
        std::mem::take(&mut *self.changes.borrow_mut())
    }

    fn msg(&self) -> String {
        self.ed.message.clone().unwrap_or_default()
    }

    fn sel(&self) -> WindowId {
        self.ed.layout.selected()
    }

    fn point(&self) -> usize {
        self.ed.primary_cursor().point
    }

    fn goto(&mut self, p: usize) {
        self.ed.cursors = vec![crate::engine::cursor::Cursor::new(self.ed.primary, p)];
    }

    fn heights(&self) -> Vec<f32> {
        self.ed.layout.window_rects().iter().map(|r| r.1.h).collect()
    }

    fn widths(&self) -> Vec<f32> {
        self.ed.layout.window_rects().iter().map(|r| r.1.w).collect()
    }
}

#[test]
fn editor_keeps_the_selected_window_in_sync() {
    let mut ed = Editor::new(Buffer::from_str("a", "0123456789"));
    let b = ed.add_buffer(Buffer::from_str("b", ""));
    let sel = ed.layout.selected();
    assert_eq!(ed.layout.window(sel).unwrap().buffer, 1);
    ed.switch_to_buffer(b);
    assert_eq!(ed.layout.window(sel).unwrap().buffer, b);
    ed.switch_to_buffer(1);

    // An unselected window on the same buffer keeps its point across edits.
    let other = ed.layout.alloc_window_id();
    let mut win = Window::new(other, 1);
    win.cursors[0].point = 8;
    ed.layout.windows.insert(other, win);
    ed.replace(0..0, "abc");
    assert_eq!(ed.layout.window(other).unwrap().cursors[0].point, 11);
    // Killing its buffer shows the replacement there too.
    ed.kill_buffer(1);
    assert_eq!(ed.buf_id, b);
    assert_eq!(ed.layout.window(other).unwrap().buffer, b);
    assert_eq!(ed.layout.window(sel).unwrap().buffer, b);
}

#[test]
fn split_below_keeps_the_original_selected_with_a_copy_of_the_point() {
    let mut e = editor("0123456789");
    e.goto(4);
    e.keys("C-x 2");
    assert_eq!(e.changes(), 1);
    let order = e.ed.layout.window_order();
    assert_eq!(order.len(), 2);
    assert_eq!(e.sel(), order[0]);
    assert_eq!(e.ed.window_point(order[1]), Some(4));
    assert_eq!(e.ed.layout.window(order[1]).unwrap().buffer, e.ed.buf_id);
    assert_eq!(e.heights(), [12.0, 12.0]);
    e.keys("C-x 3");
    assert_eq!(
        e.ed.layout.window_rects().iter().map(|r| r.1).collect::<Vec<_>>(),
        [rect(0.0, 0.0, 40.0, 12.0), rect(40.0, 0.0, 40.0, 12.0), rect(0.0, 12.0, 80.0, 12.0)]
    );
}

#[test]
fn the_same_buffer_in_two_windows_has_independent_points() {
    let mut e = editor("hello world\nsecond line\nthird");
    e.keys("C-x 2");
    e.keys("M-f"); // top window: after "hello"
    assert_eq!(e.point(), 5);
    e.keys("C-x o");
    assert_eq!(e.point(), 0, "the new window kept the point it was split with");
    e.keys("C-n C-e");
    assert_eq!(e.point(), 23);
    e.keys("C-x o");
    assert_eq!(e.point(), 5);
    e.keys("C-x o");
    assert_eq!(e.point(), 23);
}

#[test]
fn editing_in_one_window_moves_the_other_windows_point() {
    let mut e = editor("abc\ndef\nghi");
    e.goto(8); // "ghi"
    e.keys("C-x 2");
    let other = e.ed.layout.window_order()[1];
    e.goto(0);
    e.ed.execute("self-insert-command", Args { text: Some("XYZ".into()), ..Default::default() });
    assert_eq!(e.ed.window_point(other), Some(11), "inserted before it: shifted");
    e.goto(14);
    e.ed.execute("self-insert-command", Args { text: Some("!".into()), ..Default::default() });
    assert_eq!(e.ed.window_point(other), Some(11), "inserted after it: unchanged");
    // Deleting text around the other window's point pulls it back.
    e.goto(0);
    e.keys("C-k C-k C-k C-k");
    assert_eq!(e.ed.rope().to_string(), "ghi!");
    assert_eq!(e.ed.window_point(other), Some(0));
    e.keys("C-x o");
    assert_eq!(e.point(), 0);
}

#[test]
fn undo_and_redo_move_the_other_windows_point_too() {
    let mut e = editor("abc\ndef\nghi");
    e.goto(8); // "ghi"
    e.keys("C-x 2");
    let other = e.ed.layout.window_order()[1];
    e.goto(0);
    e.ed.execute("self-insert-command", Args { text: Some("XYZ".into()), ..Default::default() });
    assert_eq!(e.ed.window_point(other), Some(11));
    e.keys("C-/");
    assert_eq!(e.ed.rope().to_string(), "abc\ndef\nghi");
    assert_eq!(e.ed.window_point(other), Some(8), "undo took the insertion back: still on ghi");
    e.keys("C-?");
    assert_eq!(e.ed.window_point(other), Some(11), "redo put it back");
    // A deletion around the other point, undone, leaves it where the text went.
    e.goto(7); // "def"
    e.keys("C-k C-k");
    assert_eq!(e.ed.rope().to_string(), "XYZabc\nghi");
    assert_eq!(e.ed.window_point(other), Some(7));
    // Undo re-inserts "def\n" at the other point, which stays before it
    // (like an Emacs marker). Each C-k is its own undo group.
    e.keys("C-/ C-/");
    assert_eq!(e.ed.rope().to_string(), "XYZabc\ndef\nghi");
    assert_eq!(e.ed.window_point(other), Some(7));
    e.keys("C-x o");
    assert_eq!(e.point(), 7);
}

#[test]
fn refused_edits_in_a_read_only_buffer_leave_other_windows_points_alone() {
    let mut e = editor("abcdef\nghij");
    e.goto(11); // the end
    e.keys("C-x 2");
    let other = e.ed.layout.window_order()[1];
    assert_eq!(e.ed.window_point(other), Some(11));
    e.keys("M-<");
    e.run("read-only-mode");
    e.ed.execute("self-insert-command", Args { text: Some("X".into()), ..Default::default() });
    assert!(e.msg().contains("read-only"), "{}", e.msg());
    e.keys("C-k");
    e.run_n("delete-char", 3);
    assert_eq!(e.ed.rope().to_string(), "abcdef\nghij");
    assert_eq!(e.ed.window_point(other), Some(11), "the refused deletions must not pull it back");
    assert_eq!(e.point(), 0);
    e.keys("C-x o");
    assert_eq!(e.point(), 11);
}

#[test]
fn windows_on_different_buffers_switch_the_current_buffer() {
    let mut e = editor("first");
    let b = e.ed.add_buffer(Buffer::from_str("b", "second buffer"));
    e.keys("C-x 3");
    e.changes();
    e.keys("C-x o");
    assert_eq!(e.changes(), 1);
    e.ed.switch_to_buffer(b);
    e.check();
    e.ed.run_hooks();
    assert_eq!(e.changes(), 1, "showing another buffer is a configuration change");
    e.keys("M->");
    assert_eq!(e.point(), 13);
    e.keys("C-x o");
    assert_eq!(e.ed.buf_id, 1);
    assert_eq!(e.ed.buf.name, "a");
    e.keys("C-x o");
    assert_eq!(e.ed.buf_id, b);
    assert_eq!(e.point(), 13);
    // window_view shows each window's own buffer and cursors.
    let [left, right] = e.ed.layout.window_order()[..] else { panic!() };
    assert_eq!(e.ed.window_view(left).unwrap().buf.name, "a");
    assert_eq!(e.ed.window_view(right).unwrap().buf.name, "b");
    assert_eq!(e.ed.window_view(right).unwrap().cursors[0].point, 13);
    assert!(e.ed.window_view(WindowId(99)).is_none());
}

#[test]
fn switching_to_the_same_buffer_is_not_a_change() {
    let mut e = editor("x");
    e.ed.switch_to_buffer(1);
    e.ed.run_hooks();
    assert_eq!(e.changes(), 0);
}

#[test]
fn deleting_the_sole_window_is_refused() {
    let mut e = editor("x");
    e.keys("C-x 0");
    assert_eq!(e.msg(), "Attempt to delete minibuffer or sole ordinary window");
    assert_eq!(e.changes(), 0);
    e.keys("C-x 1");
    assert_eq!(e.changes(), 0, "delete-other-windows with one window does nothing");
}

#[test]
fn deleting_the_selected_window_selects_its_neighbour() {
    let mut e = editor("one two three");
    e.keys("C-x 2 C-x 2");
    let order = e.ed.layout.window_order();
    assert_eq!(order.len(), 3);
    e.keys("C-x o"); // the middle one
    e.keys("M-f");
    assert_eq!(e.sel(), order[1]);
    e.changes();
    e.keys("C-x 0");
    assert_eq!(e.changes(), 1);
    assert_eq!(e.sel(), order[0], "the window above got the space and is selected");
    assert_eq!(e.ed.layout.window_order(), [order[0], order[2]]);
    assert!(e.ed.layout.window(order[1]).is_none());
    assert_eq!(e.heights(), [12.0, 12.0]);
    // The topmost gives its space downwards.
    e.keys("C-x 0");
    assert_eq!(e.sel(), order[2]);
    assert_eq!(e.ed.layout.tab().root, Node::Leaf(order[2]));
}

#[test]
fn delete_other_windows_keeps_only_the_selected_one() {
    let mut e = editor("x");
    e.keys("C-x 2 C-x 3 C-x o C-x o");
    let sel = e.sel();
    e.changes();
    e.keys("C-x 1");
    assert_eq!(e.changes(), 1);
    assert_eq!(e.ed.layout.tab().root, Node::Leaf(sel));
    assert_eq!(e.ed.layout.windows.len(), 1);
}

#[test]
fn other_window_takes_a_count_and_wraps() {
    let mut e = editor("x");
    e.keys("C-x 3 C-x 3 C-x 3");
    let order = e.ed.layout.window_order();
    assert_eq!(order.len(), 4);
    assert_eq!(e.sel(), order[0]);
    e.run_n("other-window", 2);
    assert_eq!(e.sel(), order[2]);
    e.run_n("other-window", -3);
    assert_eq!(e.sel(), order[3]);
    e.keys("C-u 5 C-x o");
    assert_eq!(e.sel(), order[0]);
    e.keys("M-- C-x o");
    assert_eq!(e.sel(), order[3]);
    e.changes();
    e.run_n("other-window", 4);
    assert_eq!(e.sel(), order[3]);
    assert_eq!(e.changes(), 0, "back to the same window: no change");
    // With one window it does nothing.
    e.keys("C-x 1");
    e.changes();
    e.keys("C-x o");
    assert_eq!(e.changes(), 0);
}

#[test]
fn split_refuses_windows_too_small_to_halve() {
    let mut e = editor("x");
    e.ed.layout.set_frame_size(19.0, 7.0);
    e.keys("C-x 3");
    assert_eq!(e.msg(), "Window too small for splitting");
    e.keys("C-x 2");
    assert_eq!(e.msg(), "Window too small for splitting");
    assert_eq!(e.ed.layout.windows.len(), 1);
    assert_eq!(e.changes(), 0);
    e.ed.layout.set_frame_size(20.0, 8.0);
    e.keys("C-x 3 C-x 2");
    assert_eq!(e.ed.layout.windows.len(), 3);
    e.keys("C-x 2");
    assert_eq!(e.msg(), "Window too small for splitting");
}

#[test]
fn enlarge_and_shrink_with_prefix_args() {
    let mut e = editor("x");
    e.keys("C-x 2");
    e.changes();
    e.keys("C-x ^");
    assert_eq!(e.heights(), [13.0, 11.0]);
    e.keys("C-u 3 C-x ^");
    assert_eq!(e.heights(), [16.0, 8.0]);
    e.run_n("shrink-window", 6);
    assert_eq!(e.heights(), [10.0, 14.0]);
    e.run_n("enlarge-window", -2);
    assert_eq!(e.heights(), [8.0, 16.0]);
    assert_eq!(e.changes(), 4);
    // Clamped to the minimum, then refused.
    e.keys("C-u 1 0 0 C-x ^");
    assert_eq!(e.heights(), [20.0, 4.0]);
    e.keys("C-x ^");
    assert_eq!(e.msg(), "Cannot enlarge selected window");
    e.run_n("shrink-window", 100);
    assert_eq!(e.heights(), [4.0, 20.0]);
    e.run("shrink-window");
    assert_eq!(e.msg(), "Cannot shrink selected window");
    // Horizontally there is no split to resize against.
    e.keys("C-x }");
    assert_eq!(e.msg(), "Cannot enlarge selected window");
    e.keys("C-x {");
    assert_eq!(e.msg(), "Cannot shrink selected window");
}

#[test]
fn horizontal_resizing_and_balance() {
    let mut e = editor("x");
    e.keys("C-x 3 C-x 3");
    assert_eq!(e.widths(), [20.0, 20.0, 40.0]);
    e.keys("C-u 5 C-x {");
    assert_eq!(e.widths(), [15.0, 25.0, 40.0]);
    e.keys("C-x o C-u 8 C-x }");
    assert_eq!(e.widths(), [15.0, 33.0, 32.0]);
    e.changes();
    e.keys("C-x +");
    assert_eq!(e.changes(), 1);
    // 80 / 3: boundaries rounded, the last absorbs the rest.
    assert_eq!(e.widths(), [27.0, 26.0, 27.0]);
}

#[test]
fn resizing_uses_the_reported_frame_size() {
    let mut e = editor("x");
    e.ed.layout.set_frame_size(200.0, 60.0);
    e.keys("C-x 2 C-u 1 0 C-x ^");
    assert_eq!(e.heights(), [40.0, 20.0]);
    // The ratios stay when the frame changes size.
    e.ed.layout.set_frame_size(200.0, 30.0);
    assert_eq!(e.heights(), [20.0, 10.0]);
}

#[test]
fn windmove_by_keys() {
    // +---+---+
    // | 1 | 3 |
    // +---+---+
    // |   2   |
    // +-------+
    let mut e = editor("x");
    e.keys("C-x 2 C-x 3");
    let [w1, w3, w2] = e.ed.layout.window_order()[..] else { panic!("{:?}", e.ed.layout.window_order()) };
    assert_eq!(e.sel(), w1);
    e.keys("C-x w <right>");
    assert_eq!(e.sel(), w3);
    e.keys("C-x w <down>");
    assert_eq!(e.sel(), w2);
    e.keys("C-x w <up>");
    assert_eq!(e.sel(), w1, "the window above at the cursor's column (column 0)");
    e.keys("C-x w <left>");
    assert_eq!(e.msg(), "No window left from selected window");
    assert_eq!(e.sel(), w1);
    e.keys("C-x w <up>");
    assert_eq!(e.msg(), "No window up from selected window");
    e.keys("C-x w <down> C-x w <down>");
    assert_eq!(e.msg(), "No window down from selected window");
    e.keys("C-x w <right>");
    assert_eq!(e.msg(), "No window right from selected window");
}

#[test]
fn windmove_picks_the_window_at_the_cursor() {
    // | 1 | 2 |   cursor in 3 at column 50 -> up goes to 2.
    // |   3   |
    let mut e = editor(&"x".repeat(60));
    e.keys("C-x 2 C-x 3");
    let [_w1, w2, w3] = e.ed.layout.window_order()[..] else { panic!() };
    e.ed.select_window(w3);
    e.goto(50);
    e.keys("C-x w <up>");
    assert_eq!(e.sel(), w2);
}

#[test]
fn select_window_saves_and_restores_cursors_including_multiple() {
    let mut e = editor("aaa bbb ccc");
    e.keys("C-x 3");
    let [left, right] = e.ed.layout.window_order()[..] else { panic!() };
    e.ed.add_cursor(crate::engine::cursor::Cursor::new(0, 8));
    e.ed.normalize_cursors();
    assert!(e.ed.select_window(right));
    assert_eq!(e.ed.cursors.len(), 1);
    assert!(e.ed.select_window(left));
    assert_eq!(e.ed.cursors.iter().map(|c| c.point).collect::<Vec<_>>(), [0, 8]);
    assert!(!e.ed.select_window(WindowId(77)));
    e.ed.run_hooks();
    assert_eq!(e.changes(), 3, "the split and two selections");
    assert!(e.ed.select_window(left));
    e.ed.run_hooks();
    assert_eq!(e.changes(), 0, "selecting the selected window is not a change");
}

#[test]
fn stale_points_are_clamped_when_selected() {
    let mut e = editor("0123456789");
    e.keys("C-x 2");
    let other = e.ed.layout.window_order()[1];
    e.ed.layout.windows.get_mut(&other).unwrap().cursors[0].point = 500;
    e.ed.layout.windows.get_mut(&other).unwrap().cursors[0].mark = Some(600);
    e.ed.select_window(other);
    assert_eq!(e.point(), 10);
    assert_eq!(e.ed.cursors[0].mark, Some(10));
}

#[test]
fn killing_a_buffer_shown_in_several_windows_and_tabs() {
    let mut e = editor("main");
    let b = e.ed.add_buffer(Buffer::from_str("doomed", "bye"));
    // Tab 1: two windows, the lower shows `doomed`. Tab 2: shows `doomed`.
    e.keys("C-x 2 C-x o");
    e.ed.switch_to_buffer(b);
    e.keys("C-x o");
    e.keys("C-x t 2");
    e.ed.switch_to_buffer(b);
    e.check();
    assert_eq!(e.ed.layout.windows_showing(b).len(), 2);
    e.ed.run_hooks();
    e.changes();
    e.ed.kill_buffer(b);
    e.ed.run_hooks();
    e.check();
    assert!(e.ed.layout.windows_showing(b).is_empty());
    assert_eq!(e.ed.buf.name, "a");
    assert_eq!(e.changes(), 1, "exactly one WindowConfigurationChanged per change");
    // Back in tab 1, the lower window shows the replacement.
    e.keys("C-x t o");
    for id in e.ed.layout.window_order() {
        assert_eq!(e.ed.layout.window(id).unwrap().buffer, 1);
    }
    // Killing the last buffer leaves *scratch* everywhere.
    e.changes();
    e.ed.kill_buffer(1);
    e.ed.run_hooks();
    e.check();
    assert_eq!(e.changes(), 1);
    assert_eq!(e.ed.buf.name, "*scratch*");
    assert_eq!(e.ed.layout.windows_showing(e.ed.buf_id).len(), e.ed.layout.windows.len());
}

#[test]
fn tab_lifecycle() {
    let mut e = editor("hello");
    assert_eq!(e.ed.tab_names(), ["a"]);
    e.goto(3);
    e.keys("C-x t 2");
    assert_eq!(e.changes(), 1);
    assert_eq!(e.ed.layout.tabs.len(), 2);
    assert_eq!(e.ed.layout.current_tab, 1);
    assert_eq!(e.point(), 3, "the new tab starts at the same point");
    assert_eq!(e.ed.tab_names(), ["a", "a"]);
    e.run_with("tab-rename", Args { text: Some("work".into()), ..Default::default() });
    assert_eq!(e.ed.tab_names(), ["a", "work"]);
    e.keys("C-x 3");
    e.goto(5);
    // Back to the first tab: its single window and point.
    e.keys("C-x t o");
    assert_eq!(e.ed.layout.current_tab, 0);
    assert_eq!(e.ed.layout.window_order().len(), 1);
    assert_eq!(e.point(), 3);
    // ...and forward again: the split and its point are restored.
    e.keys("C-x t S-o");
    assert_eq!(e.ed.layout.current_tab, 1);
    assert_eq!(e.ed.layout.window_order().len(), 2);
    assert_eq!(e.point(), 5);
    // Renaming without a name goes back to the buffer's name.
    e.run("tab-rename");
    assert_eq!(e.ed.tab_names(), ["a", "a"]);
    e.run_with("tab-rename", Args { text: Some("   ".into()), ..Default::default() });
    assert_eq!(e.ed.layout.tab().name, None);
    // Closing: the tab's windows go, the buffer stays.
    e.changes();
    e.keys("C-x t 0");
    assert_eq!(e.changes(), 1);
    assert_eq!(e.ed.layout.tabs.len(), 1);
    assert_eq!(e.ed.layout.windows.len(), 1);
    assert_eq!(e.ed.buffer_count(), 1);
    assert_eq!(e.point(), 3);
    e.keys("C-x t 0");
    assert_eq!(e.msg(), "Attempt to delete the sole tab");
    assert_eq!(e.changes(), 0);
}

#[test]
fn tabs_cycle_with_counts_and_wrap() {
    let mut e = editor("x");
    e.keys("C-x t 2 C-x t 2 C-x t 2");
    assert_eq!(e.ed.layout.current_tab, 3);
    e.keys("C-x t o");
    assert_eq!(e.ed.layout.current_tab, 0);
    e.keys("C-TAB C-TAB");
    assert_eq!(e.ed.layout.current_tab, 2);
    e.keys("C-S-TAB");
    assert_eq!(e.ed.layout.current_tab, 1);
    e.run_n("tab-next", 6);
    assert_eq!(e.ed.layout.current_tab, 3);
    e.run_n("tab-previous", 5);
    assert_eq!(e.ed.layout.current_tab, 2);
    e.run_n("tab-next", -1);
    assert_eq!(e.ed.layout.current_tab, 1);
    e.changes();
    e.run_n("tab-next", 4);
    assert_eq!(e.changes(), 0, "a full cycle lands on the same tab");
}

#[test]
fn new_tabs_go_after_the_current_one_and_closing_selects_the_next() {
    let mut e = editor("x");
    let b = e.ed.add_buffer(Buffer::from_str("b", ""));
    let c = e.ed.add_buffer(Buffer::from_str("c", ""));
    e.keys("C-x t 2");
    e.ed.switch_to_buffer(b);
    e.keys("C-x t o C-x t 2"); // inserted between the first and `b`
    e.ed.switch_to_buffer(c);
    e.check();
    assert_eq!(e.ed.tab_names(), ["a", "c", "b"]);
    assert_eq!(e.ed.layout.current_tab, 1);
    e.keys("C-x t 0");
    assert_eq!(e.ed.tab_names(), ["a", "b"]);
    assert_eq!(e.ed.layout.current_tab, 1);
    assert_eq!(e.ed.buf.name, "b");
    e.keys("C-x t 0");
    assert_eq!(e.ed.layout.current_tab, 0, "the last tab closed: the one before is selected");
    assert_eq!(e.ed.buf.name, "a");
    // Closing another tab keeps the current one current.
    e.keys("C-x t 2 C-x t 2");
    assert_eq!(e.ed.layout.current_tab, 2);
    assert!(e.ed.close_tab(0).is_ok());
    assert_eq!(e.ed.layout.current_tab, 1);
    assert!(e.ed.close_tab(5).is_err());
    e.check();
}

#[test]
fn selecting_a_window_in_another_tab_switches_tab() {
    let mut e = editor("x");
    let first = e.sel();
    e.keys("C-x t 2");
    assert!(e.ed.select_window(first));
    assert_eq!(e.ed.layout.current_tab, 0);
    e.check();
    assert!(e.ed.select_tab(1));
    assert!(!e.ed.select_tab(2));
    assert!(e.ed.rename_tab(0, Some("zero")));
    assert!(!e.ed.rename_tab(5, Some("nope")));
    assert_eq!(e.ed.tab_name(0), "zero");
    assert_eq!(e.ed.tab_name(9), "");
}

#[test]
fn deleting_a_window_in_another_tab() {
    let mut e = editor("x");
    e.keys("C-x 2");
    let doomed = e.ed.layout.window_order()[0];
    e.keys("C-x t 2");
    e.ed.delete_window(doomed).unwrap();
    e.check();
    assert_eq!(e.ed.layout.current_tab, 1);
    assert_eq!(e.ed.layout.tabs[0].root.windows().len(), 1);
    assert!(e.ed.delete_window(WindowId(99)).is_err());
}

#[test]
fn tab_names_follow_the_selected_windows_buffer() {
    let mut e = editor("x");
    let b = e.ed.add_buffer(Buffer::from_str("other", ""));
    e.keys("C-x t 2");
    e.ed.switch_to_buffer(b);
    assert_eq!(e.ed.tab_names(), ["a", "other"]);
    e.keys("C-x t o");
    assert_eq!(e.ed.tab_names(), ["a", "other"]);
}

#[test]
fn window_scroll_is_recorded_and_copied_on_split() {
    let mut e = editor("x");
    let sel = e.sel();
    e.ed.set_window_scroll(sel, 42);
    e.keys("C-x 2");
    let new = e.ed.layout.window_order()[1];
    assert_eq!(e.ed.layout.window(new).unwrap().scroll_line, 42);
    // Showing another buffer starts at its top.
    let b = e.ed.add_buffer(Buffer::from_str("b", ""));
    e.ed.switch_to_buffer(b);
    assert_eq!(e.ed.layout.window(sel).unwrap().scroll_line, 0);
    e.ed.set_window_scroll(WindowId(99), 1); // ignored
}

#[test]
fn every_window_command_reports_one_change() {
    let mut e = editor("x");
    for (cmd, expected) in [
        ("split-window-below", 1),
        ("split-window-right", 1),
        ("other-window", 1),
        ("balance-windows", 1),
        ("enlarge-window", 1),
        ("shrink-window", 1),
        ("enlarge-window-horizontally", 1),
        ("shrink-window-horizontally", 1),
        ("windmove-left", 1),
        ("delete-window", 1),
        ("delete-other-windows", 1),
        ("tab-new", 1),
        ("tab-rename", 1),
        ("tab-next", 1),
        ("tab-previous", 1),
        ("tab-close", 1),
    ] {
        e.run(cmd);
        assert_eq!(e.changes(), expected, "{cmd}: {}", e.msg());
    }
}

#[test]
fn window_commands_are_registered_and_bound() {
    use crate::engine::commands;
    let ed = Editor::new(Buffer::from_str("a", ""));
    let maps = ed.active_keymaps();
    for c in COMMANDS {
        assert!(commands::lookup(c.name).is_some(), "{} not registered", c.name);
        assert!(!c.doc.is_empty());
    }
    for (keys, cmd) in [
        ("C-x 2", "split-window-below"),
        ("C-x 3", "split-window-right"),
        ("C-x 0", "delete-window"),
        ("C-x 1", "delete-other-windows"),
        ("C-x o", "other-window"),
        ("C-x +", "balance-windows"),
        ("C-x ^", "enlarge-window"),
        ("C-x }", "enlarge-window-horizontally"),
        ("C-x {", "shrink-window-horizontally"),
        ("C-x w <left>", "windmove-left"),
        ("C-x t 2", "tab-new"),
        ("C-x t S-o", "tab-previous"),
        ("C-TAB", "tab-next"),
    ] {
        let seq: Vec<String> = keys.split(' ').map(String::from).collect();
        assert!(maps.iter().any(|m| m.lookup(&seq) == Some(cmd)), "{keys}");
    }
}

#[test]
fn random_command_sequences_keep_the_invariants() {
    let cmds = [
        "split-window-below",
        "split-window-right",
        "delete-window",
        "delete-other-windows",
        "other-window",
        "balance-windows",
        "enlarge-window",
        "shrink-window",
        "enlarge-window-horizontally",
        "shrink-window-horizontally",
        "windmove-left",
        "windmove-right",
        "windmove-up",
        "windmove-down",
        "tab-new",
        "tab-close",
        "tab-next",
        "tab-previous",
        "forward-char",
        "end-of-buffer",
        "newline",
        "kill-buffer",
        "switch-buffer",
    ];
    let mut rng = Lcg(2024);
    for _ in 0..20 {
        let mut e = editor("some text\nmore text\n");
        e.ed.add_buffer(Buffer::from_str("b", "bbbbbbbbbb"));
        e.ed.layout.set_frame_size(160.0, 50.0);
        for _ in 0..150 {
            let cmd = cmds[rng.below(cmds.len() as u64) as usize];
            let n = rng.below(7) as i64 - 3;
            match cmd {
                "kill-buffer" => {
                    let id = e.ed.buf_id;
                    e.ed.kill_buffer(id);
                }
                "switch-buffer" => {
                    if e.ed.buffer_count() < 4 {
                        e.ed.add_buffer(Buffer::from_str("new", "0123456789\n0123"));
                    }
                    let id = e.ed.others[rng.below(e.ed.others.len() as u64) as usize].id;
                    e.ed.switch_to_buffer(id);
                }
                "newline" => e.ed.execute(cmd, Args::default()),
                _ => e.ed.execute(cmd, Args::with_raw(Some(RawArg::Number(n)))),
            }
            e.check();
            // Unselected windows' points are inside their buffers.
            for id in e.ed.layout.windows.keys().copied().collect::<Vec<_>>() {
                let v = e.ed.window_view(id).unwrap();
                let len = v.buf.rope.len_chars();
                assert!(v.cursors.iter().all(|c| c.point <= len), "{id:?} point beyond its buffer");
            }
            for (_, r) in e.ed.layout.window_rects() {
                assert!(r.w >= 1.0 && r.h >= 1.0);
            }
        }
    }
}
