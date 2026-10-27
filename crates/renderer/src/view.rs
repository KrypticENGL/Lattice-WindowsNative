//! The interactive viewport: model + latest layout snapshot + camera, input
//! handling (pan, zoom, drag, hover, select) and frame painting with culling
//! and level of detail.

use std::collections::HashMap;

use engine::NodeId;

use crate::camera::Camera;
use crate::canvas::{Canvas, Rgba, Style};
use crate::geom::{Rect, Vec2};
use crate::layout::{self, LayoutWorker, LINK_DISTANCE};
use crate::model::{EdgeClass, GraphModel, ModelNode, NodeClass};
use crate::spatial::Grid;

/// Node radius in world units.
pub const NODE_RADIUS: f32 = 6.0;
/// Below this on-screen radius (DIPs) nodes become unantialiased squares.
const DOT_RADIUS_PX: f32 = 1.6;
/// At or above this zoom, labels and arrowheads are drawn.
const DETAIL_ZOOM: f32 = 0.8;
/// Labels are drawn for every visible node only when there are at most
/// this many; otherwise just for the selection and hover.
const MAX_LABELS: usize = 250;
const MAX_NEIGHBOR_LABELS: usize = 60;
/// Pointer travel (DIPs) before a press becomes a drag or pan.
const DRAG_SLOP: f32 = 3.0;
/// Hover/hit tolerance around a node, in DIPs.
const HIT_SLOP_PX: f32 = 4.0;
const WHEEL_STEP: f32 = 1.2;
const FIT_MARGIN: f32 = 40.0;
const FIT_MAX_ZOOM: f32 = 2.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    /// Over a node.
    Hand,
    /// Panning or dragging.
    Move,
}

/// What the host should do after an input call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Response {
    pub repaint: bool,
    pub cursor: Cursor,
    /// A node was double-clicked: open its source.
    pub open: Option<u32>,
    /// A node was clicked (pressed and released without dragging).
    pub click: Option<u32>,
}

enum Gesture {
    None,
    /// Button down, not moved past the slop yet.
    Press {
        at: Vec2,
        node: Option<u32>,
    },
    Pan {
        last: Vec2,
    },
    Drag {
        node: u32,
        /// World offset from the pointer to the node centre.
        grab: Vec2,
    },
}

struct Selection {
    node: u32,
    /// Selected node and its neighbours.
    lit: Vec<bool>,
}

/// Index of (class, state) draw buckets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Normal = 0,
    Dim = 1,
    Lit = 2,
}

#[derive(Default)]
struct Scratch {
    segments: Vec<Vec<[Vec2; 2]>>,
    nodes: Vec<Vec<Vec2>>,
    visible: Vec<u32>,
}

pub struct GraphView {
    model: GraphModel,
    generation: u64,
    positions: Vec<Vec2>,
    running: bool,
    worker: Option<LayoutWorker>,
    camera: Camera,
    grid: Grid,
    grid_dirty: bool,
    hover: Option<u32>,
    selection: Option<Selection>,
    gesture: Gesture,
    /// Keep framing the whole graph until the user moves the camera.
    auto_fit: bool,
    scratch: Scratch,
}

impl GraphView {
    /// `wake` is called from the layout thread when new positions are
    /// ready; the host then calls [`GraphView::poll`]. If the thread cannot
    /// start, the graph is shown in its initial arrangement and the error
    /// is returned alongside.
    pub fn new(wake: Box<dyn Fn() + Send>) -> (Self, Option<String>) {
        let (worker, error) = match LayoutWorker::spawn(wake) {
            Ok(w) => (Some(w), None),
            Err(e) => (None, Some(format!("layout thread failed to start: {e}"))),
        };
        let view = Self {
            model: GraphModel::default(),
            generation: 0,
            positions: Vec::new(),
            running: false,
            worker,
            camera: Camera::default(),
            grid: Grid::default(),
            grid_dirty: true,
            hover: None,
            selection: None,
            gesture: Gesture::None,
            auto_fit: true,
            scratch: Scratch::default(),
        };
        (view, error)
    }

    pub fn model(&self) -> &GraphModel {
        &self.model
    }

    /// Whether the layout is still moving (the host keeps repainting).
    pub fn is_animating(&self) -> bool {
        self.running
    }

    pub fn zoom(&self) -> f32 {
        self.camera.zoom()
    }

    pub fn selected(&self) -> Option<&ModelNode> {
        self.selection
            .as_ref()
            .and_then(|s| self.model.node(s.node))
    }

    /// Replaces the graph. Nodes that existed before keep their positions
    /// (matched by id), so a reload or filter change only nudges the layout.
    pub fn set_model(&mut self, model: GraphModel) {
        let old = self.positions_by_id();
        self.load(model, &old);
    }

    /// Where each node is now, by id (to restore a level later).
    pub fn positions_by_id(&self) -> HashMap<NodeId, Vec2> {
        self.model
            .nodes()
            .iter()
            .zip(&self.positions)
            .map(|(n, &p)| (n.id, p))
            .collect()
    }

    /// Shows a different graph (another level), starting from `seeds` for
    /// the nodes it has positions for, and frames it.
    pub fn switch_model(&mut self, model: GraphModel, seeds: &HashMap<NodeId, Vec2>) {
        self.selection = None;
        self.auto_fit = true;
        self.load(model, seeds);
    }

    fn load(&mut self, model: GraphModel, old: &HashMap<NodeId, Vec2>) {
        let selected = self.selected().map(|n| n.id);
        let initial: Vec<Option<Vec2>> = model
            .nodes()
            .iter()
            .map(|n| old.get(&n.id).copied())
            .collect();
        let known = initial.iter().filter(|p| p.is_some()).count();
        let edges: Vec<(u32, u32)> = model.edges().iter().map(|e| (e.from, e.to)).collect();
        self.positions = layout::place(&initial, &edges);
        let alpha = if known == 0 {
            1.0
        } else if known == initial.len() {
            0.3
        } else {
            0.6
        };
        self.generation += 1;
        if let Some(w) = &self.worker {
            w.load(
                self.generation,
                self.positions.iter().copied().map(Some).collect(),
                edges,
                alpha,
            );
            self.running = !model.is_empty();
        }
        self.model = model;
        if known == 0 {
            self.auto_fit = true;
        }
        self.grid_dirty = true;
        self.hover = None;
        self.gesture = Gesture::None;
        self.selection = None;
        if let Some(i) = selected.and_then(|id| self.model.index_of(id)) {
            self.select(Some(i));
        }
        if self.auto_fit {
            self.fit();
        }
    }

    /// Re-runs the layout from scratch.
    pub fn relayout(&mut self) {
        let model = std::mem::take(&mut self.model);
        self.positions.clear();
        self.set_model(model);
    }

    /// Takes the newest layout snapshot. Returns `true` if anything moved.
    pub fn poll(&mut self) -> bool {
        let Some(snap) = self.worker.as_ref().and_then(LayoutWorker::take) else {
            return false;
        };
        if snap.generation != self.generation || snap.positions.len() != self.positions.len() {
            return false;
        }
        let dragged = match self.gesture {
            Gesture::Drag { node, .. } => self.positions.get(node as usize).map(|&p| (node, p)),
            _ => None,
        };
        self.positions.copy_from_slice(&snap.positions);
        // The pointer owns the dragged node, even if the snapshot lags.
        if let Some((node, p)) = dragged {
            self.positions[node as usize] = p;
        }
        self.running = snap.running;
        self.grid_dirty = true;
        if self.auto_fit {
            self.fit();
        }
        true
    }

    pub fn set_viewport(&mut self, r: Rect) {
        if r != self.camera.viewport() {
            self.camera.set_viewport(r);
            if self.auto_fit {
                self.fit();
            }
        }
    }

    /// Frames the whole graph.
    pub fn fit(&mut self) {
        if let Some(b) = Rect::bounding(&self.positions) {
            self.camera
                .fit(b.inflate(NODE_RADIUS), FIT_MARGIN, FIT_MAX_ZOOM);
        }
    }

    /// Frames the whole graph and keeps doing so while the layout runs.
    pub fn fit_and_follow(&mut self) {
        self.auto_fit = true;
        self.fit();
    }

    pub fn clear_selection(&mut self) -> bool {
        self.selection.take().is_some()
    }

    fn select(&mut self, node: Option<u32>) {
        self.selection = node.map(|node| {
            let mut lit = vec![false; self.model.nodes().len()];
            if let Some(l) = lit.get_mut(node as usize) {
                *l = true;
            }
            for &j in self.model.neighbors(node) {
                lit[j as usize] = true;
            }
            Selection { node, lit }
        });
    }

    /// The node under `screen` and its on-screen centre.
    pub fn node_under(&mut self, screen: Vec2) -> Option<(u32, Vec2)> {
        let i = self.node_at(screen)?;
        Some((i, self.camera.to_screen(*self.positions.get(i as usize)?)))
    }

    fn node_at(&mut self, screen: Vec2) -> Option<u32> {
        self.refresh_grid();
        let world = self.camera.to_world(screen);
        let radius = NODE_RADIUS + HIT_SLOP_PX / self.camera.zoom();
        self.grid.nearest(&self.positions, world, radius)
    }

    fn refresh_grid(&mut self) {
        if self.grid_dirty {
            self.grid = Grid::build(&self.positions, LINK_DISTANCE * 2.0);
            self.grid_dirty = false;
        }
    }

    // ---- input ----------------------------------------------------------

    pub fn pointer_down(&mut self, pos: Vec2, clicks: u8) -> Response {
        let node = self.node_at(pos);
        if clicks >= 2 {
            self.gesture = Gesture::None;
            return Response {
                cursor: cursor_for(node),
                open: node,
                ..Response::default()
            };
        }
        self.gesture = Gesture::Press { at: pos, node };
        Response {
            cursor: cursor_for(node),
            ..Response::default()
        }
    }

    pub fn pointer_move(&mut self, pos: Vec2, button_down: bool) -> Response {
        if !button_down && !matches!(self.gesture, Gesture::None) {
            // Missed the release (e.g. focus loss): end the gesture.
            self.end_gesture();
        }
        match self.gesture {
            Gesture::Press { at, node } => {
                if (pos - at).length() < DRAG_SLOP {
                    return Response {
                        cursor: cursor_for(node),
                        ..Response::default()
                    };
                }
                self.auto_fit = false;
                self.gesture = match node {
                    Some(node) => {
                        let p = self.positions[node as usize];
                        Gesture::Drag {
                            node,
                            grab: p - self.camera.to_world(at),
                        }
                    }
                    None => Gesture::Pan { last: at },
                };
                self.pointer_move(pos, true)
            }
            Gesture::Pan { last } => {
                self.camera.pan(pos - last);
                self.gesture = Gesture::Pan { last: pos };
                Response {
                    repaint: true,
                    cursor: Cursor::Move,
                    ..Response::default()
                }
            }
            Gesture::Drag { node, grab } => {
                let p = self.camera.to_world(pos) + grab;
                if let Some(slot) = self.positions.get_mut(node as usize) {
                    *slot = p;
                }
                self.grid_dirty = true;
                if let Some(w) = &self.worker {
                    w.pin(node, p);
                    self.running = true;
                }
                Response {
                    repaint: true,
                    cursor: Cursor::Move,
                    ..Response::default()
                }
            }
            Gesture::None => {
                let hover = self.node_at(pos);
                let changed = hover != self.hover;
                self.hover = hover;
                Response {
                    repaint: changed,
                    cursor: cursor_for(hover),
                    ..Response::default()
                }
            }
        }
    }

    pub fn pointer_up(&mut self, pos: Vec2) -> Response {
        let (repaint, click) = match self.gesture {
            Gesture::Press { node, .. } => {
                // A click: select the node, or clear on empty space.
                let before = self.selection.as_ref().map(|s| s.node);
                self.select(node);
                (before != node, node)
            }
            _ => (true, None),
        };
        self.end_gesture();
        Response {
            repaint,
            cursor: cursor_for(self.node_at(pos)),
            click,
            ..Response::default()
        }
    }

    fn end_gesture(&mut self) {
        if let Gesture::Drag { node, .. } = self.gesture {
            if let Some(w) = &self.worker {
                w.release(node);
            }
        }
        self.gesture = Gesture::None;
    }

    pub fn pointer_leave(&mut self) -> Response {
        Response {
            repaint: self.hover.take().is_some(),
            ..Response::default()
        }
    }

    /// `notches` > 0 zooms in about `pos`.
    pub fn wheel(&mut self, pos: Vec2, notches: f32) -> Response {
        self.auto_fit = false;
        self.camera.zoom_at(pos, WHEEL_STEP.powf(notches));
        Response {
            repaint: true,
            cursor: cursor_for(self.hover),
            ..Response::default()
        }
    }

    /// Zooms about the viewport centre (keyboard).
    pub fn zoom_by(&mut self, factor: f32) {
        self.auto_fit = false;
        let c = self.camera.viewport().center();
        self.camera.zoom_at(c, factor);
    }

    /// Pans by a screen delta (keyboard, benchmarks).
    pub fn pan_by(&mut self, delta: Vec2) {
        self.auto_fit = false;
        self.camera.pan(delta);
    }

    // ---- painting -------------------------------------------------------

    /// Draws the graph into the viewport. The caller clears the background
    /// and clips to the viewport.
    pub fn paint(&mut self, c: &mut dyn Canvas, style: &Style) {
        if self.model.is_empty() {
            return;
        }
        self.refresh_grid();
        let transform = self.camera.transform();
        let zoom = transform.scale;
        let visible = self.camera.visible_world().inflate(NODE_RADIUS * 2.0);
        let dots = zoom * NODE_RADIUS < DOT_RADIUS_PX;
        let detail = zoom >= DETAIL_ZOOM;
        let lit = self.selection.as_ref().map(|s| (s.node, &s.lit));

        let s = &mut self.scratch;
        s.segments.resize_with(EdgeClass::COUNT * 3, Vec::new);
        s.nodes.resize_with(NodeClass::COUNT * 3, Vec::new);
        s.segments.iter_mut().for_each(Vec::clear);
        s.nodes.iter_mut().for_each(Vec::clear);
        s.visible.clear();

        // Edges: cull by segment bounds, bucket by class and tone.
        let arrow = 5.0f32;
        for e in self.model.edges() {
            let (a, b) = (
                self.positions[e.from as usize],
                self.positions[e.to as usize],
            );
            if !visible.overlaps_segment(a, b) {
                continue;
            }
            let tone = match lit {
                Some((sel, _)) if e.from == sel || e.to == sel => Tone::Lit,
                Some(_) => Tone::Dim,
                None => Tone::Normal,
            };
            let bucket = &mut s.segments[e.class.index() * 3 + tone as usize];
            bucket.push([a, b]);
            if detail && tone != Tone::Dim && e.class != EdgeClass::Structure {
                let d = b - a;
                let len = d.length();
                if len > 2.0 * NODE_RADIUS + arrow {
                    let dir = d * (1.0 / len);
                    let perp = Vec2::new(-dir.y, dir.x);
                    let tip = b - dir * (NODE_RADIUS + 1.0);
                    let back = tip - dir * arrow;
                    bucket.push([tip, back + perp * (arrow * 0.6)]);
                    bucket.push([tip, back - perp * (arrow * 0.6)]);
                }
            }
        }

        // Nodes: grid query, then an exact containment test.
        let positions = &self.positions;
        self.grid.query(visible, |i| {
            if positions
                .get(i as usize)
                .is_some_and(|&p| visible.contains(p))
            {
                s.visible.push(i);
            }
        });
        for &i in &s.visible {
            let n = &self.model.nodes()[i as usize];
            let tone = match lit {
                Some((_, lit)) if lit[i as usize] => Tone::Lit,
                Some(_) => Tone::Dim,
                None => Tone::Normal,
            };
            s.nodes[n.class.index() * 3 + tone as usize].push(positions[i as usize]);
        }

        c.set_transform(transform);
        c.set_antialias(!dots);
        let px = 1.0 / zoom;
        for tone in [Tone::Dim, Tone::Normal, Tone::Lit] {
            for class in [
                EdgeClass::Structure,
                EdgeClass::References,
                EdgeClass::Calls,
                EdgeClass::Includes,
            ] {
                let segs = &s.segments[class.index() * 3 + tone as usize];
                if segs.is_empty() {
                    continue;
                }
                let base = style.edge[class.index()];
                let (color, width) = match tone {
                    Tone::Dim => (base.faded(style.dim), 1.0),
                    Tone::Normal => (
                        base,
                        if matches!(class, EdgeClass::Calls | EdgeClass::Includes) {
                            1.2
                        } else {
                            1.0
                        },
                    ),
                    Tone::Lit => (style.highlight, 1.8),
                };
                c.stroke_segments(segs, color, width * px);
            }
        }
        for tone in [Tone::Dim, Tone::Normal, Tone::Lit] {
            for class in 0..NodeClass::COUNT {
                let pts = &s.nodes[class * 3 + tone as usize];
                if pts.is_empty() {
                    continue;
                }
                let base = style.node[class];
                let color = if tone == Tone::Dim {
                    base.faded(style.dim)
                } else {
                    base
                };
                if dots {
                    c.fill_squares(pts, 1.2 * px, color);
                } else {
                    c.fill_discs(pts, NODE_RADIUS, color);
                }
            }
        }
        c.set_antialias(true);
        let ring = |c: &mut dyn Canvas, i: u32, color: Rgba, gap: f32, width: f32| {
            if let Some(&p) = positions.get(i as usize) {
                let r = if dots { 1.2 * px } else { NODE_RADIUS };
                c.stroke_circle(p, r + gap * px, color, width * px);
            }
        };
        if let Some(hub) = self.model.hub {
            // The level's subject: everything else here belongs to it.
            ring(
                c,
                hub,
                style.node[self.model.nodes()[hub as usize].class.index()],
                4.0,
                1.5,
            );
        }
        if let Some((sel, _)) = lit {
            ring(c, sel, style.highlight, 3.0, 2.0);
        }
        if let Some(h) = self.hover {
            ring(c, h, style.hover, 2.0, 1.5);
        }
        c.reset_transform();

        self.paint_labels(c, style, detail);
    }

    /// Labels, placed greedily so none overlap: the hovered node first,
    /// then the selection and its neighbours, then (zoomed in, with few
    /// enough on screen) every other visible node.
    fn paint_labels(&self, c: &mut dyn Canvas, style: &Style, detail: bool) {
        let lh = c.line_height();
        // Average glyph width of the label font, close enough for spacing.
        let char_w = lh * 0.45;
        let zoom = self.camera.zoom();
        let view = self.camera.viewport();
        let selection = self.selection.as_ref();
        let on_screen = |i: u32| {
            self.positions
                .get(i as usize)
                .is_some_and(|&p| view.contains(self.camera.to_screen(p)))
        };
        let mut order: Vec<(u32, Rgba)> = Vec::new();
        order.extend(self.hover.map(|h| (h, style.label)));
        if let Some(s) = selection {
            order.push((s.node, style.label));
            order.extend(
                self.model
                    .neighbors(s.node)
                    .iter()
                    .filter(|&&i| on_screen(i))
                    .take(MAX_NEIGHBOR_LABELS)
                    .map(|&i| (i, style.label)),
            );
        }
        if detail && self.scratch.visible.len() <= MAX_LABELS {
            for &i in &self.scratch.visible {
                let dim = selection.is_some_and(|s| !s.lit[i as usize]);
                let color = if dim {
                    style.label_dim.faded(style.dim)
                } else {
                    style.label
                };
                order.push((i, color));
            }
        }
        let forced = |i: u32| self.hover == Some(i) || selection.is_some_and(|s| s.node == i);
        let mut placed: Vec<(u32, Rect)> = Vec::new();
        for (i, color) in order {
            if placed.iter().any(|&(j, _)| j == i) {
                continue;
            }
            let (Some(n), Some(&p)) = (self.model.node(i), self.positions.get(i as usize)) else {
                continue;
            };
            let s = self.camera.to_screen(p);
            let r = (NODE_RADIUS * zoom).max(2.0);
            let rect = Rect::new(
                s.x + r + 4.0,
                s.y - lh / 2.0,
                n.label.chars().count() as f32 * char_w,
                lh,
            );
            if !forced(i) && placed.iter().any(|(_, q)| q.intersects(&rect.inflate(1.0))) {
                continue;
            }
            placed.push((i, rect));
            c.text(&n.label, Vec2::new(rect.x, rect.y), color, false);
        }
    }
}

fn cursor_for(node: Option<u32>) -> Cursor {
    if node.is_some() {
        Cursor::Hand
    } else {
        Cursor::Arrow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts what a frame draws.
    #[derive(Default)]
    struct Recorder {
        segments: usize,
        discs: usize,
        squares: usize,
        texts: Vec<String>,
    }

    impl Canvas for Recorder {
        fn set_transform(&mut self, _t: crate::geom::Affine) {}
        fn reset_transform(&mut self) {}
        fn set_antialias(&mut self, _on: bool) {}
        fn stroke_segments(&mut self, s: &[[Vec2; 2]], _c: Rgba, _w: f32) {
            self.segments += s.len();
        }
        fn fill_discs(&mut self, c: &[Vec2], _r: f32, _color: Rgba) {
            self.discs += c.len();
        }
        fn fill_squares(&mut self, c: &[Vec2], _h: f32, _color: Rgba) {
            self.squares += c.len();
        }
        fn stroke_circle(&mut self, _c: Vec2, _r: f32, _color: Rgba, _w: f32) {}
        fn fill_rect(&mut self, _r: Rect, _radius: f32, _c: Rgba) {}
        fn stroke_rect(&mut self, _r: Rect, _radius: f32, _c: Rgba, _w: f32) {}
        fn text(&mut self, s: &str, _o: Vec2, _c: Rgba, _bold: bool) {
            self.texts.push(s.to_string());
        }
        fn measure_text(&self, s: &str, _bold: bool) -> f32 {
            s.len() as f32 * 7.0
        }
        fn line_height(&self) -> f32 {
            16.0
        }
    }

    fn style() -> Style {
        let c = Rgba::new(1.0, 1.0, 1.0, 1.0);
        Style {
            node: [c; NodeClass::COUNT],
            edge: [c; EdgeClass::COUNT],
            highlight: c,
            hover: c,
            label: c,
            label_dim: c,
            card_bg: c,
            card_border: c,
            dim: 0.2,
        }
    }

    /// A view of a small synthetic graph, framed in an 800×600 viewport.
    /// Positions stay at their seeds because the test never polls.
    fn view(nodes: usize) -> GraphView {
        let (mut v, err) = GraphView::new(Box::new(|| {}));
        assert!(err.is_none());
        v.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        v.set_model(GraphModel::synthetic(nodes, 1.5));
        v
    }

    fn screen_of(v: &GraphView, i: u32) -> Vec2 {
        v.camera.to_screen(v.positions[i as usize])
    }

    fn frame(v: &mut GraphView) -> Recorder {
        let mut r = Recorder::default();
        v.paint(&mut r, &style());
        r
    }

    #[test]
    fn click_selects_double_click_opens_empty_click_clears() {
        let mut v = view(30);
        let p = screen_of(&v, 3);
        assert_eq!(v.pointer_down(p, 1).cursor, Cursor::Hand);
        assert!(v.pointer_up(p).repaint);
        assert_eq!(v.selected().map(|n| n.id), Some(NodeId(3)));
        let lit = v
            .selection
            .as_ref()
            .map(|s| s.lit.clone())
            .unwrap_or_default();
        for &j in v.model.neighbors(3) {
            assert!(lit[j as usize], "neighbour {j} not lit");
        }
        assert_eq!(v.pointer_down(p, 2).open, Some(3));
        v.pointer_up(p);

        let empty = Vec2::new(2.0, 2.0);
        assert_eq!(v.node_at(empty), None);
        v.pointer_down(empty, 1);
        v.pointer_up(empty);
        assert!(v.selected().is_none());
    }

    #[test]
    fn dragging_moves_the_node_and_panning_moves_the_camera() {
        let mut v = view(30);
        let p = screen_of(&v, 5);
        v.pointer_down(p, 1);
        let r = v.pointer_move(p + Vec2::new(40.0, 0.0), true);
        assert_eq!(r.cursor, Cursor::Move);
        v.pointer_up(p + Vec2::new(40.0, 0.0));
        let moved = screen_of(&v, 5);
        assert!((moved - (p + Vec2::new(40.0, 0.0))).length() < 0.5);

        let before = screen_of(&v, 0);
        let empty = Vec2::new(2.0, 2.0);
        v.pointer_down(empty, 1);
        v.pointer_move(empty + Vec2::new(10.0, 25.0), true);
        v.pointer_up(empty + Vec2::new(10.0, 25.0));
        assert!((screen_of(&v, 0) - (before + Vec2::new(10.0, 25.0))).length() < 0.5);
    }

    #[test]
    fn level_of_detail_and_culling() {
        let mut v = view(200);
        // Fitted: whole graph visible.
        let fitted = frame(&mut v);
        assert_eq!(fitted.discs + fitted.squares, 200);
        assert!(fitted.segments > 0);

        // Far out: dots, no labels.
        v.zoom_by(0.02);
        let far = frame(&mut v);
        assert_eq!(far.discs, 0);
        assert_eq!(far.squares, 200);
        assert!(far.texts.is_empty());

        // Close in: discs and labels for the few nodes on screen.
        v.fit();
        v.zoom_by(8.0);
        let near = frame(&mut v);
        assert!(near.discs > 0 && near.discs < 200, "{}", near.discs);
        assert_eq!(near.squares, 0);
        assert!(!near.texts.is_empty());

        // Panned away from everything: nothing drawn.
        v.pan_by(Vec2::new(1e6, 1e6));
        let none = frame(&mut v);
        assert_eq!(none.discs + none.squares + none.segments, 0);
    }

    #[test]
    fn click_is_reported_and_reload_keeps_positions() {
        let mut v = view(30);
        let p = screen_of(&v, 7);
        v.pointer_down(p, 1);
        assert_eq!(v.pointer_up(p).click, Some(7));
        let labelled = frame(&mut v);
        assert!(labelled.texts.iter().any(|t| t == "m0::item_7"));

        // Same ids again: positions and selection survive.
        let before = v.positions.clone();
        v.set_model(GraphModel::synthetic(30, 1.5));
        assert_eq!(v.positions, before);
        assert_eq!(v.selected().map(|n| n.id), Some(NodeId(7)));

        // A drag is not a click.
        v.pointer_down(p, 1);
        v.pointer_move(p + Vec2::new(30.0, 0.0), true);
        assert_eq!(v.pointer_up(p + Vec2::new(30.0, 0.0)).click, None);
    }

    #[test]
    fn switching_levels_restores_seeded_positions() {
        let mut v = view(30);
        let saved = v.positions_by_id();
        v.switch_model(GraphModel::synthetic(10, 1.0), &HashMap::new());
        assert_eq!(v.model().nodes().len(), 10);
        v.switch_model(GraphModel::synthetic(30, 1.5), &saved);
        assert_eq!(v.positions_by_id(), saved);
    }
}
