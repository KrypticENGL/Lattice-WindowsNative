//! The dock layout: a tree of splits whose leaves are tab groups of panes.
//!
//! Pure data and geometry; no drawing, no input. Splits keep their
//! children's sizes as fractions, so a layout survives any window size.

use crate::geom::{PointF, RectF};

/// How a split arranges its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side; the dividers are vertical.
    Horizontal,
    /// Stacked; the dividers are horizontal.
    Vertical,
}

/// Divider thickness between split children, in DIPs.
pub const DIVIDER: f32 = 1.0;
/// Grab zone on each side of a divider.
pub const GRAB: f32 = 4.0;
/// Height of a group's tab strip.
pub const STRIP_H: f32 = 28.0;
/// Smallest a group gets along a split, unless the area is too small.
pub const MIN_GROUP: f32 = 80.0;
/// Share of the whole area a pane docked at an outer edge takes.
const ROOT_EDGE: f32 = 0.25;

/// Every dockable panel. Each appears at most once across all trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PaneId {
    Explorer,
    Editor,
    Output,
    Terminal,
    Visualizer,
    Source,
    CallStack,
    Memory,
}

impl PaneId {
    pub const ALL: [Self; 8] = [
        Self::Explorer,
        Self::Editor,
        Self::Output,
        Self::Terminal,
        Self::Visualizer,
        Self::Source,
        Self::CallStack,
        Self::Memory,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Explorer => "Explorer",
            Self::Editor => "Editor",
            Self::Output => "Output",
            Self::Terminal => "Terminal",
            Self::Visualizer => "Visualizer",
            Self::Source => "Source",
            Self::CallStack => "Call stack",
            Self::Memory => "Memory",
        }
    }

    /// The editor and the Visualizer canvas are the centres of their
    /// workspaces: they can move but not be closed.
    pub fn closable(self) -> bool {
        !matches!(self, Self::Editor | Self::Visualizer)
    }
}

/// Where a pane goes relative to a group: one of its sides, or into its
/// tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

impl Zone {
    /// The split axis and whether the new pane comes first.
    fn split(self) -> Option<(Axis, bool)> {
        match self {
            Self::Left => Some((Axis::Horizontal, true)),
            Self::Right => Some((Axis::Horizontal, false)),
            Self::Top => Some((Axis::Vertical, true)),
            Self::Bottom => Some((Axis::Vertical, false)),
            Self::Center => None,
        }
    }

    /// The part of `r` a pane dropped in this zone would take.
    pub fn preview(self, r: RectF, share: f32) -> RectF {
        match self {
            Self::Left => RectF::new(r.x, r.y, r.w * share, r.h),
            Self::Right => RectF::new(r.right() - r.w * share, r.y, r.w * share, r.h),
            Self::Top => RectF::new(r.x, r.y, r.w, r.h * share),
            Self::Bottom => RectF::new(r.x, r.bottom() - r.h * share, r.w, r.h * share),
            Self::Center => r,
        }
    }
}

/// Where a closed pane was, so showing it again puts it back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    /// A pane it was next to (or tabbed with); `None` for the whole area.
    pub anchor: Option<PaneId>,
    pub zone: Zone,
    /// Its share of the space it splits with the anchor's group (or of the
    /// whole area at an edge).
    pub share: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Children side by side (`Horizontal`) or stacked, each with its
    /// share of the space; shares sum to 1.
    Split {
        axis: Axis,
        children: Vec<(Node, f32)>,
    },
    /// A tab group. Only the root may be empty.
    Tabs { panes: Vec<PaneId>, active: usize },
}

/// One tab group placed in the area.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub panes: Vec<PaneId>,
    pub active: usize,
    pub rect: RectF,
}

impl Group {
    pub fn active_pane(&self) -> PaneId {
        self.panes[self.active.min(self.panes.len() - 1)]
    }

    pub fn strip(&self) -> RectF {
        self.rect.split_top(STRIP_H.min(self.rect.h)).0
    }

    pub fn body(&self) -> RectF {
        self.rect.split_top(STRIP_H.min(self.rect.h)).1
    }
}

/// A divider between two children of a split.
#[derive(Debug, Clone, PartialEq)]
pub struct Handle {
    /// Path from the root to the split.
    path: Vec<usize>,
    /// The divider follows child `index`.
    index: usize,
    pub axis: Axis,
    /// The divider line.
    pub rect: RectF,
    /// The whole split, for turning a pointer position into shares.
    span: RectF,
}

impl Handle {
    pub fn grab_zone(&self) -> RectF {
        match self.axis {
            Axis::Horizontal => RectF::new(
                self.rect.x - GRAB,
                self.rect.y,
                self.rect.w + 2.0 * GRAB,
                self.rect.h,
            ),
            Axis::Vertical => RectF::new(
                self.rect.x,
                self.rect.y - GRAB,
                self.rect.w,
                self.rect.h + 2.0 * GRAB,
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub rect: RectF,
    pub groups: Vec<Group>,
    pub handles: Vec<Handle>,
}

impl Layout {
    pub fn group_at(&self, p: PointF) -> Option<usize> {
        self.groups.iter().position(|g| g.rect.contains(p))
    }

    #[cfg(test)]
    pub fn group_of(&self, pane: PaneId) -> Option<usize> {
        self.groups.iter().position(|g| g.panes.contains(&pane))
    }

    pub fn handle_at(&self, p: PointF) -> Option<usize> {
        self.handles.iter().position(|h| h.grab_zone().contains(p))
    }
}

fn along(axis: Axis, r: RectF) -> (f32, f32) {
    match axis {
        Axis::Horizontal => (r.x, r.w),
        Axis::Vertical => (r.y, r.h),
    }
}

impl Node {
    pub fn tabs(panes: &[PaneId]) -> Self {
        Self::Tabs {
            panes: panes.to_vec(),
            active: 0,
        }
    }

    pub fn split(axis: Axis, children: Vec<(Node, f32)>) -> Self {
        Self::Split { axis, children }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Tabs { panes, .. } if panes.is_empty())
    }

    /// Panes in layout order.
    #[cfg(test)]
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    #[cfg(test)]
    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Tabs { panes, .. } => out.extend(panes),
            Self::Split { children, .. } => children.iter().for_each(|(c, _)| c.collect(out)),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.path_to(pane).is_some()
    }

    fn first_pane(&self) -> Option<PaneId> {
        match self {
            Self::Tabs { panes, .. } => panes.first().copied(),
            Self::Split { children, .. } => children.iter().find_map(|(c, _)| c.first_pane()),
        }
    }

    /// Path to the tab group holding `pane`.
    fn path_to(&self, pane: PaneId) -> Option<Vec<usize>> {
        match self {
            Self::Tabs { panes, .. } => panes.contains(&pane).then(Vec::new),
            Self::Split { children, .. } => children.iter().enumerate().find_map(|(i, (c, _))| {
                let mut p = c.path_to(pane)?;
                p.insert(0, i);
                Some(p)
            }),
        }
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut Node {
        let mut n = self;
        for &i in path {
            n = match n {
                Self::Split { children, .. } => &mut children[i].0,
                Self::Tabs { .. } => unreachable!("path runs through a tab group"),
            };
        }
        n
    }

    /// Makes `pane` the visible tab of its group. Returns whether it is in
    /// this tree.
    pub fn activate(&mut self, pane: PaneId) -> bool {
        let Some(path) = self.path_to(pane) else {
            return false;
        };
        if let Self::Tabs { panes, active } = self.at_mut(&path) {
            if let Some(i) = panes.iter().position(|&p| p == pane) {
                *active = i;
            }
        }
        true
    }

    /// Takes `pane` out, collapsing what it leaves behind. Returns where it
    /// was, or `None` if it is not in this tree.
    pub fn remove(&mut self, pane: PaneId) -> Option<Spot> {
        let path = self.path_to(pane)?;
        let Self::Tabs { panes, active } = self.at_mut(&path) else {
            unreachable!("path_to ends at a tab group");
        };
        let i = panes.iter().position(|&p| p == pane)?;
        panes.remove(i);
        if !panes.is_empty() {
            if *active >= panes.len() || *active > i {
                *active = active.saturating_sub(1).min(panes.len() - 1);
            }
            return Some(Spot {
                anchor: Some(panes[*active]),
                zone: Zone::Center,
                share: 1.0,
            });
        }
        let Some((&child, parent)) = path.split_last() else {
            // The root group is now empty.
            return Some(Spot {
                anchor: None,
                zone: Zone::Center,
                share: 1.0,
            });
        };
        let Self::Split { axis, children } = self.at_mut(parent) else {
            unreachable!("a group's parent is a split");
        };
        let axis = *axis;
        // Its neighbour (the one before, else after) inherits its space.
        let (neighbor, zone) = if child > 0 {
            let zone = match axis {
                Axis::Horizontal => Zone::Right,
                Axis::Vertical => Zone::Bottom,
            };
            (child - 1, zone)
        } else {
            let zone = match axis {
                Axis::Horizontal => Zone::Left,
                Axis::Vertical => Zone::Top,
            };
            (child + 1, zone)
        };
        let share = children[child].1;
        let edge = parent.is_empty() && (child == 0 || child + 1 == children.len());
        let spot = if edge {
            // At an end of the whole area: back to that edge.
            Spot {
                anchor: None,
                zone,
                share,
            }
        } else {
            Spot {
                anchor: children[neighbor].0.first_pane(),
                zone,
                share: share / (share + children[neighbor].1),
            }
        };
        children[neighbor].1 += share;
        children.remove(child);
        self.collapse(parent);
        Some(spot)
    }

    /// Replaces a split left with one child by that child, merging it into
    /// its own parent when they split the same way.
    fn collapse(&mut self, path: &[usize]) {
        let node = self.at_mut(path);
        let Self::Split { children, .. } = node else {
            return;
        };
        if children.len() != 1 {
            return;
        }
        let (only, _) = children.pop().expect("one child");
        *node = only;
        let Some((&i, parent)) = path.split_last() else {
            return;
        };
        let Self::Split {
            axis: outer,
            children: siblings,
        } = self.at_mut(parent)
        else {
            return;
        };
        let outer = *outer;
        if let (Self::Split { axis, children }, share) = &mut siblings[i] {
            if *axis == outer {
                let share = *share;
                let inner: Vec<(Node, f32)> = std::mem::take(children)
                    .into_iter()
                    .map(|(n, s)| (n, s * share))
                    .collect();
                siblings.splice(i..=i, inner);
            }
        }
    }

    /// Docks `pane` (not already in the tree) relative to `anchor`'s group,
    /// or at the edge of the whole area when the anchor is `None` or
    /// absent.
    pub fn insert(&mut self, pane: PaneId, anchor: Option<PaneId>, zone: Zone) {
        let share = if anchor.is_some() { 0.5 } else { ROOT_EDGE };
        self.restore(
            pane,
            Spot {
                anchor,
                zone,
                share,
            },
        );
    }

    /// Docks `pane` at a spot [`remove`](Self::remove) returned.
    pub fn restore(&mut self, pane: PaneId, spot: Spot) {
        let Spot {
            anchor,
            zone,
            share,
        } = spot;
        let share = share.clamp(0.05, 0.95);
        debug_assert!(!self.contains(pane), "{pane:?} is already docked");
        if self.is_empty() {
            *self = Self::tabs(&[pane]);
            return;
        }
        let Some(path) = anchor.and_then(|a| self.path_to(a)) else {
            self.insert_at_edge(pane, zone, share);
            return;
        };
        let Some((axis, first)) = zone.split() else {
            if let Self::Tabs { panes, active } = self.at_mut(&path) {
                panes.push(pane);
                *active = panes.len() - 1;
            }
            return;
        };
        let new = Self::tabs(&[pane]);
        if let Some((&i, parent)) = path.split_last() {
            if let Self::Split { axis: a, children } = self.at_mut(parent) {
                if *a == axis {
                    // Same direction as the parent: become a sibling, taking
                    // part of the target's share.
                    let taken = children[i].1 * share;
                    children[i].1 -= taken;
                    children.insert(if first { i } else { i + 1 }, (new, taken));
                    return;
                }
            }
        }
        let target = self.at_mut(&path);
        let old = std::mem::replace(target, Self::tabs(&[]));
        let pair = if first {
            vec![(new, share), (old, 1.0 - share)]
        } else {
            vec![(old, 1.0 - share), (new, share)]
        };
        *target = Self::split(axis, pair);
    }

    fn insert_at_edge(&mut self, pane: PaneId, zone: Zone, share: f32) {
        let Some((axis, first)) = zone.split() else {
            // Into the first group.
            let mut n = &mut *self;
            loop {
                match n {
                    Self::Split { children, .. } => n = &mut children[0].0,
                    Self::Tabs { panes, active } => {
                        panes.push(pane);
                        *active = panes.len() - 1;
                        return;
                    }
                }
            }
        };
        let new = (Self::tabs(&[pane]), share);
        if let Self::Split { axis: a, children } = self {
            if *a == axis {
                for (_, s) in children.iter_mut() {
                    *s *= 1.0 - share;
                }
                if first {
                    children.insert(0, new);
                } else {
                    children.push(new);
                }
                return;
            }
        }
        let old = (std::mem::replace(self, Self::tabs(&[])), 1.0 - share);
        *self = Self::split(
            axis,
            if first {
                vec![new, old]
            } else {
                vec![old, new]
            },
        );
    }

    /// Smallest size along `axis` this subtree can be squeezed to.
    fn min_along(&self, axis: Axis) -> f32 {
        match self {
            Self::Tabs { .. } => MIN_GROUP,
            Self::Split { axis: a, children } => {
                let mins = children.iter().map(|(c, _)| c.min_along(axis));
                if *a == axis {
                    mins.sum::<f32>() + DIVIDER * (children.len() - 1) as f32
                } else {
                    mins.fold(0.0, f32::max)
                }
            }
        }
    }

    /// Places every group in `rect`.
    pub fn layout(&self, rect: RectF) -> Layout {
        let mut out = Layout {
            rect,
            ..Layout::default()
        };
        self.place(rect, &mut Vec::new(), &mut out);
        out
    }

    fn place(&self, rect: RectF, path: &mut Vec<usize>, out: &mut Layout) {
        match self {
            Self::Tabs { panes, active } => {
                if !panes.is_empty() {
                    out.groups.push(Group {
                        panes: panes.clone(),
                        active: *active,
                        rect,
                    });
                }
            }
            Self::Split { axis, children } => {
                let (start, extent) = along(*axis, rect);
                let n = children.len();
                let available = (extent - DIVIDER * (n - 1) as f32).max(0.0);
                let mins: Vec<f32> = children.iter().map(|(c, _)| c.min_along(*axis)).collect();
                let shares: Vec<f32> = children.iter().map(|(_, s)| *s).collect();
                let sizes = fit_sizes(&shares, &mins, available);
                let mut pos = start;
                for (i, ((child, _), size)) in children.iter().zip(&sizes).enumerate() {
                    let end = if i + 1 == n {
                        start + extent
                    } else {
                        (pos + size).round()
                    };
                    let r = match axis {
                        Axis::Horizontal => RectF::new(pos, rect.y, end - pos, rect.h),
                        Axis::Vertical => RectF::new(rect.x, pos, rect.w, end - pos),
                    };
                    path.push(i);
                    child.place(r, path, out);
                    path.pop();
                    if i + 1 < n {
                        let divider = match axis {
                            Axis::Horizontal => RectF::new(end, rect.y, DIVIDER, rect.h),
                            Axis::Vertical => RectF::new(rect.x, end, rect.w, DIVIDER),
                        };
                        out.handles.push(Handle {
                            path: path.clone(),
                            index: i,
                            axis: *axis,
                            rect: divider,
                            span: rect,
                        });
                    }
                    pos = end + DIVIDER;
                }
            }
        }
    }

    /// Moves a divider so it sits at `at` (x for side-by-side, y for
    /// stacked), keeping both neighbours at their minimum size or more.
    pub fn drag_handle(&mut self, h: &Handle, at: f32) {
        let Self::Split { axis, children } = self.at_mut(&h.path) else {
            return;
        };
        let axis = *axis;
        let i = h.index;
        if i + 1 >= children.len() {
            return;
        }
        let (start, extent) = along(axis, h.span);
        let available = extent - DIVIDER * (children.len() - 1) as f32;
        if available <= 0.0 {
            return;
        }
        let before: f32 = children[..i].iter().map(|(_, s)| s).sum();
        let pair = children[i].1 + children[i + 1].1;
        let pos = (at - start - i as f32 * DIVIDER) / available;
        let lo = children[i].0.min_along(axis) / available;
        let hi = pair - children[i + 1].0.min_along(axis) / available;
        let share = if lo <= hi {
            (pos - before).clamp(lo, hi)
        } else {
            pair / 2.0
        };
        children[i].1 = share;
        children[i + 1].1 = pair - share;
    }
}

/// Sizes for children with these shares and minimums in `available`
/// space: minimums first, the rest taken from those above their minimum.
fn fit_sizes(shares: &[f32], mins: &[f32], available: f32) -> Vec<f32> {
    let total: f32 = shares.iter().sum::<f32>().max(f32::EPSILON);
    let mut sizes: Vec<f32> = shares
        .iter()
        .zip(mins)
        .map(|(s, m)| (s / total * available).max(*m))
        .collect();
    let over = sizes.iter().sum::<f32>() - available;
    if over > 0.0 {
        let slack: f32 = sizes.iter().zip(mins).map(|(s, m)| s - m).sum();
        if slack >= over {
            for (s, m) in sizes.iter_mut().zip(mins) {
                *s -= over * (*s - m) / slack;
            }
        } else {
            // Too small for every minimum: shrink proportionally.
            let k = available / sizes.iter().sum::<f32>();
            sizes.iter_mut().for_each(|s| *s *= k);
        }
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::*;
    use PaneId::*;

    fn code() -> Node {
        Node::split(
            Axis::Horizontal,
            vec![
                (Node::tabs(&[Explorer]), 0.2),
                (
                    Node::split(
                        Axis::Vertical,
                        vec![
                            (Node::tabs(&[Editor]), 0.7),
                            (Node::tabs(&[Output, Terminal]), 0.3),
                        ],
                    ),
                    0.8,
                ),
            ],
        )
    }

    fn area() -> RectF {
        RectF::new(0.0, 0.0, 1001.0, 801.0)
    }

    fn rect_of(n: &Node, p: PaneId) -> RectF {
        let l = n.layout(area());
        l.groups[l.group_of(p).unwrap()].rect
    }

    fn sums_to_one(n: &Node) -> bool {
        match n {
            Node::Tabs { .. } => true,
            Node::Split { children, .. } => {
                (children.iter().map(|(_, s)| s).sum::<f32>() - 1.0).abs() < 1e-4
                    && children.len() >= 2
                    && children.iter().all(|(c, _)| sums_to_one(c))
            }
        }
    }

    #[test]
    fn layout_tiles_the_area() {
        let l = code().layout(area());
        assert_eq!(l.groups.len(), 3);
        assert_eq!(l.handles.len(), 2);
        let explorer = l.groups[0].rect;
        assert_eq!((explorer.x, explorer.w), (0.0, 200.0));
        let editor = l.groups[1].rect;
        assert_eq!(editor.x, 201.0, "one divider after the explorer");
        assert_eq!(editor.right(), 1001.0);
        let bottom = l.groups[2].rect;
        assert_eq!(bottom.bottom(), 801.0);
        assert_eq!(bottom.y, editor.bottom() + DIVIDER);
        let area: f32 = l.groups.iter().map(|g| g.rect.w * g.rect.h).sum();
        let dividers = 801.0 + 800.0;
        assert!((area + dividers - 1001.0 * 801.0).abs() < 1.0);
    }

    #[test]
    fn minimums_win_over_shares() {
        let mut n = code();
        if let Node::Split { children, .. } = &mut n {
            children[0].1 = 0.01;
            children[1].1 = 0.99;
        }
        assert_eq!(rect_of(&n, Explorer).w, MIN_GROUP);
    }

    #[test]
    fn removing_collapses_and_returns_the_spot() {
        let mut n = code();
        // Leaving a tab group: its neighbour tab is the anchor.
        let spot = n.remove(Terminal).unwrap();
        assert_eq!((spot.anchor, spot.zone), (Some(Output), Zone::Center));
        // Emptying a group: the split collapses into its parent.
        let spot = n.remove(Output).unwrap();
        assert_eq!((spot.anchor, spot.zone), (Some(Editor), Zone::Bottom));
        assert!((spot.share - 0.3).abs() < 1e-5);
        assert_eq!(n.panes(), [Explorer, Editor]);
        assert!(sums_to_one(&n));
        let spot = n.remove(Explorer).unwrap();
        assert_eq!(
            (spot.anchor, spot.zone),
            (None, Zone::Left),
            "an outer edge"
        );
        assert_eq!(n, Node::tabs(&[Editor]));
        assert_eq!(n.remove(Explorer), None, "not in the tree");
        n.remove(Editor);
        assert!(n.is_empty());
    }

    #[test]
    fn a_removed_pane_goes_back_where_it_was() {
        for pane in [Explorer, Output, Terminal] {
            let mut n = code();
            let before = n.layout(area());
            let spot = n.remove(pane).unwrap();
            n.restore(pane, spot);
            assert!(sums_to_one(&n));
            let after = n.layout(area());
            assert_eq!(
                after.groups.iter().map(|g| g.rect).collect::<Vec<_>>(),
                before.groups.iter().map(|g| g.rect).collect::<Vec<_>>(),
                "{pane:?}"
            );
        }
    }

    #[test]
    fn inserting_beside_and_into_groups() {
        let mut n = code();
        n.remove(Terminal);
        // Right of the editor: a new split inside the vertical one.
        n.insert(Terminal, Some(Editor), Zone::Right);
        let editor = rect_of(&n, Editor);
        let term = rect_of(&n, Terminal);
        assert_eq!(term.y, editor.y);
        assert_eq!(term.x, editor.right() + DIVIDER);
        assert!((term.w - editor.w).abs() <= 1.0);
        // Below the output: the vertical split gains a third child.
        n.remove(Terminal);
        n.insert(Terminal, Some(Output), Zone::Bottom);
        let Node::Split { children, .. } = &n else {
            panic!()
        };
        let Node::Split { children: col, .. } = &children[1].0 else {
            panic!()
        };
        assert_eq!(col.len(), 3);
        assert!(sums_to_one(&n));
        // Into a group as a tab: it becomes the active one.
        n.remove(Terminal);
        n.insert(Terminal, Some(Explorer), Zone::Center);
        let l = n.layout(area());
        let g = &l.groups[l.group_of(Explorer).unwrap()];
        assert_eq!(
            (g.panes.clone(), g.active_pane()),
            (vec![Explorer, Terminal], Terminal)
        );
    }

    #[test]
    fn edge_docking_takes_a_quarter() {
        let mut n = code();
        n.remove(Output);
        n.insert(Output, None, Zone::Right);
        let r = rect_of(&n, Output);
        assert!((r.w - 1000.0 * 0.25).abs() < 2.0, "{r:?}");
        assert_eq!(r.h, 801.0);
        n.remove(Explorer);
        n.insert(Explorer, None, Zone::Top);
        assert_eq!(rect_of(&n, Explorer).w, 1001.0);
        assert!(sums_to_one(&n));
        // Into an empty area.
        let mut e = Node::tabs(&[]);
        e.insert(Source, Some(Editor), Zone::Left);
        assert_eq!(e, Node::tabs(&[Source]));
    }

    #[test]
    fn dragging_a_divider_respects_minimums() {
        let mut n = code();
        let l = n.layout(area());
        let h = l
            .handles
            .iter()
            .find(|h| h.axis == Axis::Horizontal)
            .unwrap()
            .clone();
        n.drag_handle(&h, 400.0);
        assert_eq!(rect_of(&n, Explorer).w, 400.0);
        n.drag_handle(&h, 5.0);
        assert_eq!(rect_of(&n, Explorer).w, MIN_GROUP);
        n.drag_handle(&h, 2000.0);
        assert_eq!(rect_of(&n, Editor).w, MIN_GROUP);
        assert!(sums_to_one(&n));
    }

    #[test]
    fn activate_and_hit_tests() {
        let mut n = code();
        assert!(n.activate(Terminal));
        assert!(!n.activate(Source));
        let l = n.layout(area());
        assert_eq!(l.groups[2].active_pane(), Terminal);
        assert_eq!(l.group_at(PointF::new(10.0, 10.0)), Some(0));
        assert_eq!(
            l.handle_at(PointF::new(202.0, 300.0)),
            Some(0),
            "within the grab zone"
        );
        assert_eq!(l.handle_at(PointF::new(220.0, 300.0)), None);
    }
}
