//! Dockable panels. A [`DockArea`] lays a [`tree::Node`] out in a rect,
//! draws each group's tab strip and the dividers, routes input to the
//! panes, and lets a tab be dragged to another group's side or tabs, to an
//! outer edge of the area, or (through the shell) to the other workspace.

pub mod tree;

pub use tree::{Axis, Node, PaneId, Spot, Zone};

use crate::event::{CursorShape, Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;
use tree::Layout;

const TAB_PAD: f32 = 12.0;
const CLOSE: f32 = 14.0;
/// Movement before a pressed tab starts dragging.
const DRAG_SLOP: f32 = 4.0;
/// Within this of the area's outer edge, a drop docks along that edge.
const EDGE_BAND: f32 = 18.0;
/// The top edge's band is thin: just below it are the top tab strips,
/// where a drop joins the tabs.
const TOP_BAND: f32 = 6.0;
/// Inside a group, the outer fraction on each side that docks beside it.
const SIDE_BAND: f32 = 0.25;

/// The panels a [`DockArea`] lays out and forwards input to.
pub trait PaneHost {
    fn pane(&mut self, id: PaneId) -> &mut dyn Panel;
}

/// A pressed or dragged tab. The shell keeps one for all areas, so a drag
/// can carry on in the other workspace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabDrag {
    pub pane: PaneId,
    start: PointF,
    /// Moved far enough to be a drag rather than a click.
    pub active: bool,
}

/// Where a dragged tab would land.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drop {
    /// Beside or into the group holding `anchor`.
    Group { anchor: PaneId, zone: Zone },
    /// Along an outer edge of the whole area.
    Edge(Zone),
}

/// Changes to the trees that the shell applies (they may involve both
/// workspaces).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request {
    Close(PaneId),
    Move { pane: PaneId, to: Drop },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Tab {
    pane: PaneId,
    rect: RectF,
    close: Option<RectF>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Pane(PaneId),
    Handle(usize),
}

pub struct DockArea {
    pub tree: Node,
    layout: Layout,
    /// Per group, its tabs.
    tabs: Vec<Vec<Tab>>,
    focus: Option<PaneId>,
    /// Pane under the pointer (it gets `MouseLeave` when this changes).
    hover: Option<PaneId>,
    hover_handle: Option<usize>,
    /// Tab under the pointer, and whether over its close button.
    hover_tab: Option<(PaneId, bool)>,
    owner: Option<Owner>,
    /// Drop target under a dragged tab, and the area it would take.
    preview: Option<(Drop, RectF)>,
}

impl DockArea {
    pub fn new(tree: Node) -> Self {
        Self {
            tree,
            layout: Layout::default(),
            tabs: Vec::new(),
            focus: None,
            hover: None,
            hover_handle: None,
            hover_tab: None,
            owner: None,
            preview: None,
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.tree.contains(pane)
    }

    /// Shows `pane`'s tab and gives it the keyboard. Returns whether it is
    /// in this area.
    pub fn reveal(&mut self, pane: PaneId, cx: &mut Cx) -> bool {
        if !self.tree.activate(pane) {
            return false;
        }
        self.focus = Some(pane);
        cx.repaint();
        true
    }

    /// Whether `pane` is the visible tab of its group.
    pub fn is_shown(&self, pane: PaneId) -> bool {
        self.layout.groups.iter().any(|g| g.active_pane() == pane)
    }

    /// Forgets a drop preview (the drag ended or moved elsewhere).
    pub fn clear_preview(&mut self) -> bool {
        self.preview.take().is_some()
    }

    /// The pointer left the area (or the workspace was switched).
    pub fn leave(&mut self, host: &mut dyn PaneHost, cx: &mut Cx) {
        if self.hover_handle.take().is_some() | self.hover_tab.take().is_some() {
            cx.repaint();
        }
        if let Some(old) = self.hover.take() {
            host.pane(old).on_event(&Event::MouseLeave, cx);
        }
        self.owner = None;
    }

    pub fn layout(&mut self, rect: RectF, text: &TextSystem, host: &mut dyn PaneHost) {
        self.layout = self.tree.layout(rect);
        self.tabs = self
            .layout
            .groups
            .iter()
            .map(|g| {
                let strip = g.strip();
                let mut x = strip.x + 4.0;
                g.panes
                    .iter()
                    .map(|&pane| {
                        let label = text.measure(TextStyle::UiSmall, pane.title());
                        let extra = if pane.closable() { CLOSE + 6.0 } else { 0.0 };
                        let rect = RectF::new(x, strip.y, label + 2.0 * TAB_PAD + extra, strip.h);
                        x = rect.right();
                        let close = pane.closable().then(|| {
                            RectF::new(
                                rect.right() - TAB_PAD + 2.0 - CLOSE,
                                rect.y + (rect.h - CLOSE) / 2.0,
                                CLOSE,
                                CLOSE,
                            )
                        });
                        Tab { pane, rect, close }
                    })
                    .collect()
            })
            .collect();
        // Every pane in a group is laid out, so a hidden tab (the
        // terminal behind Output) keeps its size.
        for g in &self.layout.groups {
            let body = g.body();
            for &p in &g.panes {
                host.pane(p).layout(body, text);
            }
        }
        if self.focus.is_none_or(|f| !self.tree.contains(f)) {
            // The workspace's centre (editor or canvas), else the first tab.
            self.focus = PaneId::ALL
                .into_iter()
                .find(|&p| !p.closable() && self.tree.contains(p))
                .or_else(|| self.layout.groups.first().map(|g| g.active_pane()));
        }
    }

    fn pane_at(&self, p: PointF) -> Option<PaneId> {
        let g = &self.layout.groups[self.layout.group_at(p)?];
        g.body().contains(p).then(|| g.active_pane())
    }

    fn tab_at(&self, p: PointF) -> Option<(PaneId, bool)> {
        let g = self.layout.group_at(p)?;
        if !self.layout.groups[g].strip().contains(p) {
            return None;
        }
        let t = self.tabs[g].iter().find(|t| t.rect.contains(p))?;
        Some((t.pane, t.close.is_some_and(|c| c.contains(p))))
    }

    /// Where `dragged` would land if dropped at `p`.
    fn drop_at(&self, p: PointF, dragged: PaneId) -> Option<(Drop, RectF)> {
        let area = self.layout.rect;
        if !area.contains(p) {
            return None;
        }
        let edge = [
            (p.x - area.x, Zone::Left),
            (area.right() - p.x, Zone::Right),
            (p.y - area.y + EDGE_BAND - TOP_BAND, Zone::Top),
            (area.bottom() - p.y, Zone::Bottom),
        ]
        .into_iter()
        .filter(|(d, _)| *d < EDGE_BAND)
        .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, zone)) = edge {
            return Some((Drop::Edge(zone), zone.preview(area, 0.25)));
        }
        let g = &self.layout.groups[self.layout.group_at(p)?];
        let anchor = *g.panes.iter().find(|&&q| q != dragged)?;
        let zone = if g.strip().contains(p) {
            Zone::Center
        } else {
            let r = g.body();
            let fx = (p.x - r.x) / r.w.max(1.0);
            let fy = (p.y - r.y) / r.h.max(1.0);
            [
                (fx, Zone::Left),
                (1.0 - fx, Zone::Right),
                (fy, Zone::Top),
                (1.0 - fy, Zone::Bottom),
            ]
            .into_iter()
            .filter(|(d, _)| *d < SIDE_BAND)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Zone::Center, |(_, z)| z)
        };
        if zone == Zone::Center && g.panes.contains(&dragged) {
            return None;
        }
        Some((Drop::Group { anchor, zone }, zone.preview(g.rect, 0.5)))
    }

    fn set_focus(&mut self, pane: PaneId, cx: &mut Cx) {
        if self.focus != Some(pane) {
            self.focus = Some(pane);
            cx.repaint();
        }
    }

    fn handle_cursor(&self, h: usize) -> CursorShape {
        match self.layout.handles[h].axis {
            Axis::Horizontal => CursorShape::SizeWE,
            Axis::Vertical => CursorShape::SizeNS,
        }
    }

    /// Routes an event over this area. `drag` is the shell's shared tab
    /// drag. Returns a change for the shell to make to the trees.
    pub fn on_event(
        &mut self,
        e: &Event,
        host: &mut dyn PaneHost,
        drag: &mut Option<TabDrag>,
        cx: &mut Cx,
    ) -> Option<Request> {
        if let Some(d) = drag.as_mut() {
            match *e {
                Event::MouseMove { pos, .. } => {
                    let (dx, dy) = (pos.x - d.start.x, pos.y - d.start.y);
                    if !d.active && dx.hypot(dy) >= DRAG_SLOP {
                        d.active = true;
                    }
                    if d.active {
                        let preview = self.drop_at(pos, d.pane);
                        if preview != self.preview {
                            self.preview = preview;
                            cx.repaint();
                        }
                        cx.cursor = CursorShape::Move;
                    }
                    return None;
                }
                Event::MouseUp {
                    button: MouseButton::Left,
                    ..
                } => {
                    let d = drag.take()?;
                    let preview = self.preview.take();
                    cx.repaint();
                    return match preview {
                        Some((to, _)) if d.active => Some(Request::Move { pane: d.pane, to }),
                        _ => None,
                    };
                }
                Event::MouseLeave => {
                    if self.clear_preview() {
                        cx.repaint();
                    }
                    return None;
                }
                Event::MouseDown { .. } | Event::MouseUp { .. } | Event::Wheel { .. } => {
                    return None
                }
                _ => {}
            }
        }
        match *e {
            Event::KeyDown { .. } | Event::Char(_) => {
                if let Some(f) = self.focus {
                    host.pane(f).on_event(e, cx);
                }
            }
            Event::MouseDown { pos, button, .. } => {
                if let Some(h) = self.layout.handle_at(pos) {
                    if button == MouseButton::Left {
                        self.owner = Some(Owner::Handle(h));
                        cx.cursor = self.handle_cursor(h);
                    }
                    return None;
                }
                if let Some((pane, close)) = self.tab_at(pos) {
                    let middle = button == MouseButton::Middle;
                    if pane.closable() && (close || middle) {
                        return Some(Request::Close(pane));
                    }
                    if button == MouseButton::Left {
                        self.tree.activate(pane);
                        self.set_focus(pane, cx);
                        cx.repaint();
                        *drag = Some(TabDrag {
                            pane,
                            start: pos,
                            active: false,
                        });
                    }
                    return None;
                }
                if let Some(p) = self.pane_at(pos) {
                    self.set_focus(p, cx);
                    self.owner = Some(Owner::Pane(p));
                    host.pane(p).on_event(e, cx);
                }
            }
            Event::MouseMove { pos, .. } => match self.owner {
                Some(Owner::Handle(h)) => {
                    let handle = self.layout.handles[h].clone();
                    let at = match handle.axis {
                        Axis::Horizontal => pos.x,
                        Axis::Vertical => pos.y,
                    };
                    self.tree.drag_handle(&handle, at);
                    cx.cursor = self.handle_cursor(h);
                    cx.repaint();
                }
                Some(Owner::Pane(p)) => {
                    host.pane(p).on_event(e, cx);
                }
                None => {
                    let handle = self.layout.handle_at(pos);
                    if handle != self.hover_handle {
                        self.hover_handle = handle;
                        cx.repaint();
                    }
                    let tab = if handle.is_none() {
                        self.tab_at(pos)
                    } else {
                        None
                    };
                    if tab != self.hover_tab {
                        self.hover_tab = tab;
                        cx.repaint();
                    }
                    let pane = if handle.is_none() && tab.is_none() {
                        self.pane_at(pos)
                    } else {
                        None
                    };
                    if pane != self.hover {
                        if let Some(old) = self.hover {
                            host.pane(old).on_event(&Event::MouseLeave, cx);
                        }
                        self.hover = pane;
                    }
                    if let Some(h) = handle {
                        cx.cursor = self.handle_cursor(h);
                    } else if let Some(p) = pane {
                        host.pane(p).on_event(e, cx);
                    }
                }
            },
            Event::MouseUp { .. } => match self.owner.take() {
                Some(Owner::Pane(p)) => {
                    host.pane(p).on_event(e, cx);
                }
                Some(Owner::Handle(_)) => cx.repaint(),
                None => {}
            },
            Event::Wheel { pos, .. } => {
                if let Some(p) = self.pane_at(pos) {
                    host.pane(p).on_event(e, cx);
                }
            }
            Event::MouseLeave => {
                if self.owner.is_none() {
                    self.leave(host, cx);
                }
            }
            Event::Focus(_) => cx.repaint(),
        }
        None
    }

    pub fn paint(
        &mut self,
        d: &mut DrawCtx,
        theme: &Theme,
        host: &mut dyn PaneHost,
        window_focused: bool,
    ) {
        if self.layout.groups.is_empty() {
            d.fill_rect(self.layout.rect, theme.panel_bg);
        }
        for (gi, g) in self.layout.groups.iter().enumerate() {
            let active = g.active_pane();
            let focused = window_focused && self.focus == Some(active);
            d.clip(g.body(), |d| host.pane(active).paint(d, theme, focused));
            let strip = g.strip();
            d.fill_rect(strip, theme.chrome_bg);
            d.fill_rect(
                RectF::new(strip.x, strip.bottom() - 1.0, strip.w, 1.0),
                theme.border,
            );
            let group_focused = window_focused && self.focus.is_some_and(|f| g.panes.contains(&f));
            d.clip(strip, |d| {
                for t in &self.tabs[gi] {
                    let is_active = t.pane == active;
                    let hover = self.hover_tab.map(|(p, _)| p) == Some(t.pane);
                    if is_active {
                        d.fill_rect(
                            RectF::new(t.rect.x, t.rect.y, t.rect.w, t.rect.h - 1.0),
                            theme.panel_bg,
                        );
                    } else if hover {
                        d.fill_rect(t.rect, theme.hover_bg);
                    }
                    let color = if is_active {
                        theme.text_bright
                    } else {
                        theme.text_dim
                    };
                    d.text(
                        TextStyle::UiSmall,
                        t.pane.title(),
                        RectF::new(t.rect.x + TAB_PAD, t.rect.y, t.rect.w - TAB_PAD, t.rect.h),
                        color,
                    );
                    if is_active {
                        let line = if group_focused {
                            theme.accent
                        } else {
                            theme.text_dim
                        };
                        d.fill_rect(
                            RectF::new(t.rect.x + 4.0, t.rect.bottom() - 2.0, t.rect.w - 8.0, 2.0),
                            line,
                        );
                    }
                    if let Some(c) = t.close.filter(|_| is_active || hover) {
                        let over = self.hover_tab == Some((t.pane, true));
                        if over {
                            d.fill_round_rect(c, 3.0, theme.hover_bg);
                        }
                        let x = if over { theme.text } else { theme.text_dim };
                        let m = c.inset(4.0);
                        d.line(
                            PointF::new(m.x, m.y),
                            PointF::new(m.right(), m.bottom()),
                            x,
                            1.2,
                        );
                        d.line(
                            PointF::new(m.right(), m.y),
                            PointF::new(m.x, m.bottom()),
                            x,
                            1.2,
                        );
                    }
                }
            });
        }
        let dragging = matches!(self.owner, Some(Owner::Handle(_)));
        for (i, h) in self.layout.handles.iter().enumerate() {
            let hot =
                self.hover_handle == Some(i) || (dragging && self.owner == Some(Owner::Handle(i)));
            if hot {
                d.fill_rect(
                    inset_axis(h.grab_zone(), h.axis, tree::GRAB - 1.0),
                    theme.accent,
                );
            } else {
                d.fill_rect(h.rect, theme.border);
            }
        }
        if let Some((_, r)) = self.preview {
            d.fill_rect(r, theme.dock_drop);
            d.stroke_rect(r.inset(0.5), theme.accent, 1.0);
        }
    }
}

/// Shrinks `r` across a divider's direction only.
fn inset_axis(r: RectF, axis: Axis, d: f32) -> RectF {
    match axis {
        Axis::Horizontal => RectF::new(r.x + d, r.y, (r.w - 2.0 * d).max(0.0), r.h),
        Axis::Vertical => RectF::new(r.x, r.y + d, r.w, (r.h - 2.0 * d).max(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree::STRIP_H;
    use PaneId::*;

    fn area() -> DockArea {
        let tree = Node::split(
            Axis::Horizontal,
            vec![
                (Node::tabs(&[Explorer]), 0.2),
                (Node::tabs(&[Editor, Output]), 0.8),
            ],
        );
        let mut a = DockArea::new(tree);
        a.layout = a.tree.layout(RectF::new(0.0, 0.0, 1001.0, 600.0));
        a
    }

    #[test]
    fn drop_zones() {
        let a = area();
        // Outer edge band.
        let (d, r) = a.drop_at(PointF::new(5.0, 300.0), Output).unwrap();
        assert_eq!(d, Drop::Edge(Zone::Left));
        assert!((r.w - 1001.0 * 0.25).abs() < 0.01);
        // The explorer's sides and middle (body starts below the strip).
        let mid_y = STRIP_H + (600.0 - STRIP_H) / 2.0;
        assert_eq!(
            a.drop_at(PointF::new(100.0, mid_y), Output).unwrap().0,
            Drop::Group {
                anchor: Explorer,
                zone: Zone::Center
            }
        );
        assert_eq!(
            a.drop_at(PointF::new(170.0, mid_y), Output).unwrap().0,
            Drop::Group {
                anchor: Explorer,
                zone: Zone::Right
            }
        );
        assert_eq!(
            a.drop_at(PointF::new(100.0, 560.0), Output).unwrap().0,
            Drop::Group {
                anchor: Explorer,
                zone: Zone::Bottom
            }
        );
        // The top edge band is thin.
        assert_eq!(
            a.drop_at(PointF::new(100.0, 3.0), Output).unwrap().0,
            Drop::Edge(Zone::Top)
        );
        // On a strip: join the tabs.
        assert_eq!(
            a.drop_at(PointF::new(100.0, 10.0), Output).unwrap().0,
            Drop::Group {
                anchor: Explorer,
                zone: Zone::Center
            }
        );
    }

    #[test]
    fn dropping_on_its_own_group() {
        let a = area();
        let mid = PointF::new(600.0, 300.0);
        // Into the group it is already in: nothing.
        assert_eq!(a.drop_at(mid, Output), None);
        // Beside it: split off, anchored on the other tab.
        assert_eq!(
            a.drop_at(PointF::new(950.0, 300.0), Output).unwrap().0,
            Drop::Group {
                anchor: Editor,
                zone: Zone::Right
            }
        );
        // A group holding only the dragged pane is no target.
        assert_eq!(a.drop_at(PointF::new(100.0, 300.0), Explorer), None);
    }
}
