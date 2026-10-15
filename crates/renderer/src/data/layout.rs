//! Shape-aware layout of a [`Snapshot`]: trees as trees, lists as chains,
//! arrays as rows of cells, matrices as grids.
//!
//! 1. Every frame and object becomes a box: frames and structs are rows of
//!    `name | value`, arrays are cells with their indices under them, arrays
//!    of scalar arrays (`int[3][4]`, `vector<vector<int>>`) one grid.
//!    Types are not drawn; the view shows them for the focused box only.
//! 2. A depth-first walk from the frames (outermost first, in slot order)
//!    picks one parent per box: the first slot that reaches it. The other
//!    pointers to it are cross edges (back pointers, `prev`, cycles,
//!    sharing) and are drawn as curves.
//! 3. Per struct type, the same-type pointer fields that are tree edges
//!    somewhere are its *child fields*. One child field (`next`) makes a
//!    chain: the child goes to the right, or downward when the chain hangs
//!    off an array cell (hash buckets, adjacency lists). Two or more
//!    (`left`/`right`, `children[]`) make a tree: children go below in field
//!    order, and a null child keeps an empty slot, so a right child stays
//!    right. Anything else a box owns hangs below it.
//! 4. Subtree extents are computed bottom-up, then boxes are placed
//!    top-down: frames in a column on the left, each structure to their
//!    right, level with the variable that first reaches it.

use std::collections::{HashMap, HashSet};

use debugger::{Cell, Object, ObjectKind, Snapshot};

use crate::canvas::Canvas;
use crate::geom::{Rect, Vec2};

/// Text metrics the layout needs; every [`Canvas`] provides them.
pub trait Measure {
    fn text_width(&self, s: &str, bold: bool) -> f32;
    fn line_height(&self) -> f32;
}

impl<C: Canvas + ?Sized> Measure for C {
    fn text_width(&self, s: &str, bold: bool) -> f32 {
        self.measure_text(s, bold)
    }

    fn line_height(&self) -> f32 {
        Canvas::line_height(self)
    }
}

const PAD_X: f32 = 8.0;
const ROW_PAD: f32 = 4.0;
const NAME_GAP: f32 = 14.0;
/// Minimum width of a value cell that holds a pointer dot.
const PORT_W: f32 = 18.0;
const CELL_MIN: f32 = 26.0;
const CELL_PAD: f32 = 7.0;
const MIN_BOX_W: f32 = 48.0;
/// Space under arrays for index labels.
const INDEX_GAP: f32 = 2.0;
/// Vertical gap between a box and the children below it.
pub const V_GAP: f32 = 40.0;
/// Horizontal gap between sibling subtrees.
const H_GAP: f32 = 20.0;
/// Gap between chain links.
const SPINE_GAP: f32 = 40.0;
/// Width kept for a missing child of a tree node.
const NULL_W: f32 = 22.0;
const FRAME_GAP: f32 = 16.0;
const BLOCK_GAP: f32 = 30.0;
const FRAMES_TO_BLOCKS: f32 = 120.0;
const MAX_VALUE_CHARS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxKind {
    Frame,
    Struct,
    Map,
    Array,
    Matrix,
    Cell,
}

/// What a value slot leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// Plain text.
    None,
    Pointer,
    /// A by-value member drawn as its own box.
    Embedded,
    Null,
    /// Unreadable address, or past the object cap.
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// Field name, index or key; unique within the box.
    pub name: String,
    pub ty: String,
    /// The value as drawn (empty for a pointer with a target: it is a dot).
    pub text: String,
    /// Raw value text (for pointers, the address).
    pub raw: String,
    /// Where the name is drawn, for row boxes.
    pub name_at: Option<Vec2>,
    pub cell: Rect,
    pub link: Link,
    /// The object id this slot points at (resolved into `target`).
    pub target_key: Option<String>,
    pub target: Option<usize>,
    /// Differs from the previous stop; set by the view.
    pub changed: bool,
}

impl Slot {
    /// Where an outgoing edge starts.
    pub fn port(&self) -> Vec2 {
        self.cell.center()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataBox {
    /// Object id, or `frame:N` for the N-th frame (outermost is 0).
    pub key: String,
    pub kind: BoxKind,
    /// Outline of the box (cells or rows), in world units.
    pub rect: Rect,
    pub slots: Vec<Slot>,
    /// Secondary text drawn dim: array indices, matrix row/column numbers.
    pub labels: Vec<(String, Vec2)>,
    /// Frames: function name and its `line N`, drawn as a header band.
    pub header: Option<(String, String)>,
    pub header_h: f32,
    pub ty: String,
    pub size: u64,
    pub addr: Option<String>,
    pub summary: Option<String>,
    /// Frames: source file and line.
    pub source: Option<(String, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Parent to child in the spanning tree.
    Tree,
    /// Parent to a by-value member.
    Embedded,
    /// Any other pointer: back pointers, cycles, sharing.
    Cross,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataEdge {
    pub from: usize,
    pub slot: usize,
    pub to: usize,
    pub kind: EdgeKind,
    /// Polyline from the slot's port to the target's outline.
    pub points: Vec<Vec2>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataLayout {
    pub boxes: Vec<DataBox>,
    pub edges: Vec<DataEdge>,
    pub bounds: Rect,
    /// Number of frame boxes (they come first in `boxes`).
    pub frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Right,
    Down,
}

#[derive(Debug, Clone, Copy)]
enum Item {
    Child(usize),
    Gap,
}

#[derive(Debug, Clone, Default)]
struct Arrange {
    /// The box's parent and the slot that reaches it.
    parent: Option<(usize, usize)>,
    spine: Option<usize>,
    dir: Option<Dir>,
    below: Vec<Item>,
    /// Footprint of the box itself (outline plus labels under it).
    own: Vec2,
    core: Vec2,
    row: Vec2,
    ext: Vec2,
}

struct Metrics {
    lh: f32,
    row_h: f32,
}

impl DataLayout {
    pub fn build<M: Measure + ?Sized>(snap: &Snapshot, m: &M) -> Self {
        let lh = m.line_height().max(1.0);
        let mt = Metrics {
            lh,
            row_h: lh + 2.0 * ROW_PAD,
        };
        let mut boxes = Vec::new();
        let mut index: HashMap<&str, usize> = HashMap::new();

        for (i, f) in snap.frames.iter().enumerate() {
            let slots = f
                .vars
                .iter()
                .map(|v| slot(&v.name, &v.ty, &v.value))
                .collect();
            boxes.push(rows_box(
                format!("frame:{i}"),
                BoxKind::Frame,
                slots,
                Some((f.func.clone(), format!("line {}", f.line))),
                m,
                &mt,
            ));
            let b = boxes.last_mut().expect("just pushed");
            b.source = Some((f.file.clone(), f.line));
        }
        let frames = boxes.len();

        let absorbed = matrix_rows(snap);
        for (key, o) in &snap.objects {
            if absorbed.contains(key.as_str()) {
                continue;
            }
            let b = object_box(key, o, snap, m, &mt);
            index.insert(key.as_str(), boxes.len());
            boxes.push(b);
        }
        // Resolve targets.
        for b in &mut boxes {
            for s in &mut b.slots {
                s.target = s.target_key.as_deref().and_then(|k| index.get(k).copied());
                if s.target.is_none() && matches!(s.link, Link::Pointer | Link::Embedded) {
                    // The target was absorbed into a grid or never sent.
                    s.link = Link::Invalid;
                    s.text = shorten(&s.raw);
                }
            }
        }

        let mut lay = Self {
            boxes,
            edges: Vec::new(),
            bounds: Rect::default(),
            frames,
        };
        let (order, arr) = lay.arrange();
        lay.place(&order, arr);
        lay
    }

    /// Spanning forest, chain/tree arrangement and extents.
    fn arrange(&self) -> (Vec<usize>, Vec<Arrange>) {
        let n = self.boxes.len();
        let mut arr = vec![Arrange::default(); n];
        let mut seen = vec![false; n];
        let mut order = Vec::with_capacity(n);
        // Depth-first from each frame (and then any unreached box), slots in
        // order. The explicit stack keeps long lists off the call stack.
        let roots: Vec<usize> = (0..n).collect();
        for &root in &roots {
            if seen[root] {
                continue;
            }
            seen[root] = true;
            order.push(root);
            let mut stack = vec![(root, 0usize)];
            while let Some(top) = stack.last_mut() {
                let (b, si) = *top;
                if si >= self.boxes[b].slots.len() {
                    stack.pop();
                    continue;
                }
                top.1 += 1;
                if let Some(t) = self.boxes[b].slots[si].target {
                    if !seen[t] && t >= self.frames {
                        seen[t] = true;
                        arr[t].parent = Some((b, si));
                        order.push(t);
                        stack.push((t, 0));
                    }
                }
            }
        }

        // Child fields per struct type.
        let mut self_fields: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut child_fields: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (bi, b) in self.boxes.iter().enumerate() {
            if b.kind != BoxKind::Struct {
                continue;
            }
            for (si, s) in b.slots.iter().enumerate() {
                if !matches!(s.link, Link::Pointer | Link::Null) || !same_type_ptr(&s.ty, &b.ty) {
                    continue;
                }
                let fields = self_fields.entry(&b.ty).or_default();
                if !fields.contains(&s.name.as_str()) {
                    fields.push(&s.name);
                }
                if let Some(t) = s.target {
                    if arr[t].parent == Some((bi, si)) {
                        child_fields.entry(&b.ty).or_default().insert(&s.name);
                    }
                }
            }
        }

        // Arrangement, top-down (parents come before children in `order`).
        for &b in &order {
            let bx = &self.boxes[b];
            let kids = child_fields.get(bx.ty.as_str());
            let chain_field = kids
                .filter(|k| k.len() == 1)
                .and_then(|k| k.iter().next().copied());
            let tree_fields = kids.filter(|k| k.len() >= 2);
            let mut below = Vec::new();
            let mut spine = None;
            for (si, s) in bx.slots.iter().enumerate() {
                let child = s.target.filter(|&t| arr[t].parent == Some((b, si)));
                if bx.kind == BoxKind::Struct && Some(s.name.as_str()) == chain_field {
                    if let Some(c) = child {
                        spine = Some(c);
                        continue;
                    }
                }
                match child {
                    Some(c) => below.push(Item::Child(c)),
                    None if tree_fields.is_some_and(|f| f.contains(s.name.as_str())) => {
                        below.push(Item::Gap)
                    }
                    None => {}
                }
            }
            if !below.iter().any(|i| matches!(i, Item::Child(_))) {
                below.clear();
            }
            if bx.kind == BoxKind::Frame {
                // Frames are a column of their own; what they reach is placed
                // as separate blocks.
                below.clear();
                spine = None;
            }
            let dir = match arr[b].parent {
                Some((p, _)) if arr[p].spine == Some(b) => arr[p].dir,
                Some((p, _)) if matches!(self.boxes[p].kind, BoxKind::Array | BoxKind::Matrix) => {
                    Some(Dir::Down)
                }
                _ => Some(Dir::Right),
            };
            let a = &mut arr[b];
            a.below = below;
            a.spine = spine;
            a.dir = dir;
            a.own = footprint(bx);
        }

        // Extents, bottom-up.
        for &b in order.iter().rev() {
            let mut row = Vec2::ZERO;
            let mut first = true;
            for item in &arr[b].below {
                let (w, h) = match *item {
                    Item::Child(c) => (arr[c].ext.x, arr[c].ext.y),
                    Item::Gap => (NULL_W, 0.0),
                };
                if !first {
                    row.x += H_GAP;
                }
                first = false;
                row.x += w;
                row.y = row.y.max(h);
            }
            let own = arr[b].own;
            let core = if arr[b].below.is_empty() {
                own
            } else {
                Vec2::new(own.x.max(row.x), own.y + V_GAP + row.y)
            };
            let ext = match (arr[b].spine, arr[b].dir) {
                (Some(s), Some(Dir::Down)) => Vec2::new(
                    core.x.max(arr[s].ext.x),
                    core.y + SPINE_GAP * 0.75 + arr[s].ext.y,
                ),
                (Some(s), _) => {
                    Vec2::new(core.x + SPINE_GAP + arr[s].ext.x, core.y.max(arr[s].ext.y))
                }
                (None, _) => core,
            };
            let a = &mut arr[b];
            a.row = row;
            a.core = core;
            a.ext = ext;
        }
        (order, arr)
    }

    fn place(&mut self, order: &[usize], arr: Vec<Arrange>) {
        let n = self.boxes.len();
        let mut pos: Vec<Option<Vec2>> = vec![None; n];

        // Frames: a column, all as wide as the widest.
        let frame_w = self.boxes[..self.frames]
            .iter()
            .map(|b| b.rect.w)
            .fold(0.0, f32::max);
        let mut y = 0.0;
        for (frame, p) in self.boxes[..self.frames].iter_mut().zip(pos.iter_mut()) {
            if frame.rect.w < frame_w {
                widen(frame, frame_w);
            }
            *p = Some(Vec2::new(0.0, y));
            y += frame.rect.h + FRAME_GAP;
        }

        // Blocks: whatever a frame slot reaches first, in frame order; then
        // anything left (unreachable boxes).
        let block_x = frame_w + FRAMES_TO_BLOCKS;
        let mut cursor = 0.0f32;
        let mut roots = Vec::new();
        for &b in order {
            if b < self.frames {
                continue;
            }
            match arr[b].parent {
                Some((p, si)) if p < self.frames => {
                    let row_y = pos[p].map_or(0.0, |v| v.y) + self.boxes[p].slots[si].cell.y;
                    roots.push((b, Some(row_y)));
                }
                None => roots.push((b, None)),
                _ => {}
            }
        }
        for (b, want) in roots {
            let top = want.map_or(cursor, |w| w.max(cursor));
            self.place_subtree(b, Vec2::new(block_x, top), &arr, &mut pos);
            cursor = top + arr[b].ext.y + BLOCK_GAP;
        }

        for (b, p) in pos.iter().enumerate() {
            let p = p.unwrap_or(Vec2::ZERO);
            translate(&mut self.boxes[b], p);
        }
        self.edges = self.route(&arr);
        let mut bounds: Option<Rect> = None;
        for b in &self.boxes {
            let r = outline(b).inflate(4.0);
            bounds = Some(match bounds {
                None => r,
                Some(u) => union(u, r),
            });
        }
        self.bounds = bounds.unwrap_or_default();
    }

    /// Places `root`'s subtree with its extent's top-left at `origin`.
    fn place_subtree(&self, root: usize, origin: Vec2, arr: &[Arrange], pos: &mut [Option<Vec2>]) {
        let mut stack = vec![(root, origin)];
        while let Some((b, o)) = stack.pop() {
            let a = &arr[b];
            pos[b] = Some(Vec2::new(o.x + (a.core.x - a.own.x) / 2.0, o.y));
            // `pos` is the box's local origin; its outline may start inside.
            let mut x = o.x + (a.core.x - a.row.x) / 2.0;
            let y = o.y + a.own.y + V_GAP;
            for item in &a.below {
                match *item {
                    Item::Child(c) => {
                        stack.push((c, Vec2::new(x, y)));
                        x += arr[c].ext.x + H_GAP;
                    }
                    Item::Gap => x += NULL_W + H_GAP,
                }
            }
            if let Some(s) = a.spine {
                let at = match a.dir {
                    Some(Dir::Down) => Vec2::new(o.x, o.y + a.core.y + SPINE_GAP * 0.75),
                    _ => Vec2::new(o.x + a.core.x + SPINE_GAP, o.y),
                };
                stack.push((s, at));
            }
        }
    }

    fn route(&self, arr: &[Arrange]) -> Vec<DataEdge> {
        let mut edges = Vec::new();
        let frames_right = self.boxes[..self.frames]
            .iter()
            .map(|b| b.rect.right())
            .fold(0.0, f32::max);
        for (bi, b) in self.boxes.iter().enumerate() {
            for (si, s) in b.slots.iter().enumerate() {
                let Some(t) = s.target else { continue };
                let tree = arr[t].parent == Some((bi, si));
                let kind = match (tree, s.link) {
                    (true, Link::Embedded) => EdgeKind::Embedded,
                    (true, _) => EdgeKind::Tree,
                    _ => EdgeKind::Cross,
                };
                let from = s.port();
                let tr = self.boxes[t].rect;
                let left_in = Vec2::new(tr.x, entry_y(tr));
                let points = if bi < self.frames && tr.x > frames_right {
                    // Out of the frame column: a smooth S-curve, level at both
                    // ends, so it never runs back over another frame.
                    s_curve(from, left_in)
                } else if tree && arr[bi].spine == Some(t) && arr[bi].dir == Some(Dir::Right) {
                    vec![from, left_in]
                } else if tree {
                    vec![from, Vec2::new(tr.x + tr.w / 2.0, tr.y)]
                } else {
                    curve(from, tr)
                };
                edges.push(DataEdge {
                    from: bi,
                    slot: si,
                    to: t,
                    kind,
                    points,
                });
            }
        }
        edges
    }

    /// The box under `p` (world), topmost last-drawn first.
    pub fn box_at(&self, p: Vec2) -> Option<usize> {
        self.boxes.iter().rposition(|b| b.rect.contains(p))
    }

    pub fn find(&self, key: &str) -> Option<usize> {
        self.boxes.iter().position(|b| b.key == key)
    }
}

/// A cross edge: a cubic curve from `from` to the nearest side of `to`,
/// leaving and arriving perpendicular to the box sides.
/// A cubic from `a` to `b` leaving and arriving horizontally.
fn s_curve(a: Vec2, b: Vec2) -> Vec<Vec2> {
    if (a.y - b.y).abs() < 0.5 {
        return vec![a, b];
    }
    let dx = ((b.x - a.x) * 0.5).max(40.0);
    let c1 = a + Vec2::new(dx, 0.0);
    let c2 = b - Vec2::new(dx, 0.0);
    cubic(a, c1, c2, b)
}

fn cubic(a: Vec2, c1: Vec2, c2: Vec2, b: Vec2) -> Vec<Vec2> {
    (0..=16)
        .map(|i| {
            let t = i as f32 / 16.0;
            let u = 1.0 - t;
            a * (u * u * u) + c1 * (3.0 * u * u * t) + c2 * (3.0 * u * t * t) + b * (t * t * t)
        })
        .collect()
}

/// Where edges enter a box from the side: level with its first row, so
/// they read as pointing at the object, not at one of its fields.
fn entry_y(r: Rect) -> f32 {
    r.y + r.h.min(28.0) / 2.0
}

fn curve(from: Vec2, to: Rect) -> Vec<Vec2> {
    let c = to.center();
    let candidates = [
        (Vec2::new(to.x, entry_y(to)), Vec2::new(-1.0, 0.0)),
        (Vec2::new(to.right(), entry_y(to)), Vec2::new(1.0, 0.0)),
        (Vec2::new(c.x, to.y), Vec2::new(0.0, -1.0)),
        (Vec2::new(c.x, to.bottom()), Vec2::new(0.0, 1.0)),
    ];
    let (end, normal) = candidates
        .iter()
        .copied()
        .min_by(|a, b| {
            (a.0 - from)
                .length_sq()
                .total_cmp(&(b.0 - from).length_sq())
        })
        .unwrap_or((c, Vec2::ZERO));
    let d = (end - from).length();
    let bend = (d * 0.45).clamp(30.0, 160.0);
    // Leave sideways, away from the target, so the curve clears the box it
    // starts in.
    let out = if end.x >= from.x {
        Vec2::new(1.0, 0.0)
    } else {
        Vec2::new(-1.0, 0.0)
    };
    let c1 = from + out * bend + Vec2::new(0.0, -bend * 0.35);
    let c2 = end + normal * bend;
    cubic(from, c1, c2, end)
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect::new(
        x,
        y,
        a.right().max(b.right()) - x,
        a.bottom().max(b.bottom()) - y,
    )
}

/// Labels are drawn smaller than this line.
const LABEL_H: f32 = 14.0;

/// Size of the box with its labels, measured from its local origin (all
/// local coordinates are non-negative).
fn footprint(b: &DataBox) -> Vec2 {
    let r = outline(b);
    Vec2::new(r.right(), r.bottom())
}

/// The box's outline united with its labels.
pub fn outline(b: &DataBox) -> Rect {
    b.labels.iter().fold(b.rect, |u, (_, at)| {
        union(u, Rect::new(at.x, at.y, 8.0, LABEL_H))
    })
}

fn translate(b: &mut DataBox, d: Vec2) {
    b.rect.x += d.x;
    b.rect.y += d.y;
    for s in &mut b.slots {
        s.cell.x += d.x;
        s.cell.y += d.y;
        if let Some(n) = &mut s.name_at {
            *n = *n + d;
        }
    }
    for (_, at) in &mut b.labels {
        *at = *at + d;
    }
}

/// Grows a row box to width `w`, keeping values right of the names.
fn widen(b: &mut DataBox, w: f32) {
    let extra = w - b.rect.w;
    b.rect.w = w;
    for s in &mut b.slots {
        s.cell.w += extra;
    }
}

/// `T *` (or `T*`, `const T *`) where the object type is `T`.
fn same_type_ptr(field_ty: &str, obj_ty: &str) -> bool {
    let f = field_ty.trim().trim_start_matches("const ").trim_end();
    let Some(base) = f.strip_suffix('*') else {
        return false;
    };
    let base = base.trim_end().trim_end_matches(" const").trim();
    let base = base
        .strip_prefix("struct ")
        .or_else(|| base.strip_prefix("class "))
        .unwrap_or(base);
    base == obj_ty.trim()
}

/// Inner arrays drawn as rows of a parent grid.
fn matrix_rows(snap: &Snapshot) -> HashSet<&str> {
    let mut out = HashSet::new();
    for o in snap.objects.values() {
        if let Some(rows) = grid_rows(o, snap) {
            out.extend(rows.into_iter().map(|(k, _)| k));
        }
    }
    out
}

/// The rows of `o` if it is an array whose elements are all by-value arrays
/// of scalars.
fn grid_rows<'a>(o: &'a Object, snap: &'a Snapshot) -> Option<Vec<(&'a str, &'a Object)>> {
    if o.kind != ObjectKind::Array || o.fields.is_empty() {
        return None;
    }
    o.fields
        .iter()
        .map(|f| match &f.value {
            Cell::Embedded { to } => {
                let inner = snap.objects.get(to)?;
                let scalars = inner.kind == ObjectKind::Array
                    && !inner.fields.is_empty()
                    && inner
                        .fields
                        .iter()
                        .all(|f| matches!(f.value, Cell::Scalar { .. }));
                scalars.then_some((to.as_str(), inner))
            }
            _ => None,
        })
        .collect()
}

fn shorten(s: &str) -> String {
    if s.chars().count() <= MAX_VALUE_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_VALUE_CHARS - 1).collect();
    out.push('…');
    out
}

fn slot(name: &str, ty: &str, cell: &Cell) -> Slot {
    let (text, raw, link) = match cell {
        Cell::Scalar { v } => (shorten(v), v.clone(), Link::None),
        Cell::Pointer { to: Some(_), v, .. } => (String::new(), v.clone(), Link::Pointer),
        Cell::Pointer { bad: true, v, .. } => {
            (format!("bad {}", shorten(v)), v.clone(), Link::Invalid)
        }
        Cell::Pointer { cut: true, v, .. } => ("…".to_string(), v.clone(), Link::Invalid),
        Cell::Pointer { v, .. } => ("null".to_string(), v.clone(), Link::Null),
        Cell::Embedded { to } => (String::new(), to.clone(), Link::Embedded),
    };
    Slot {
        name: name.to_string(),
        ty: ty.to_string(),
        text,
        raw,
        name_at: None,
        cell: Rect::default(),
        link,
        target_key: cell.target().map(str::to_string),
        target: None,
        changed: false,
    }
}

fn value_width<M: Measure + ?Sized>(s: &Slot, m: &M) -> f32 {
    let w = m.text_width(&s.text, false);
    if matches!(s.link, Link::Pointer | Link::Embedded) {
        w.max(PORT_W)
    } else {
        w
    }
}

fn empty_box(key: String, kind: BoxKind) -> DataBox {
    DataBox {
        key,
        kind,
        rect: Rect::default(),
        slots: Vec::new(),
        labels: Vec::new(),
        header: None,
        header_h: 0.0,
        ty: String::new(),
        size: 0,
        addr: None,
        summary: None,
        source: None,
    }
}

/// Frames, structs, maps and single cells: rows of `name | value`.
fn rows_box<M: Measure + ?Sized>(
    key: String,
    kind: BoxKind,
    mut slots: Vec<Slot>,
    header: Option<(String, String)>,
    m: &M,
    mt: &Metrics,
) -> DataBox {
    if slots.is_empty() {
        let mut s = slot("", "", &Cell::Scalar { v: "{ }".into() });
        s.text = if kind == BoxKind::Frame {
            "no locals".into()
        } else {
            "{ }".into()
        };
        slots.push(s);
    }
    let name_w = slots
        .iter()
        .map(|s| m.text_width(&s.name, false))
        .fold(0.0, f32::max);
    let value_w = slots.iter().map(|s| value_width(s, m)).fold(0.0, f32::max);
    let gap = if name_w > 0.0 { NAME_GAP } else { 0.0 };
    let header_w = header.as_ref().map_or(0.0, |(t, sub)| {
        m.text_width(t, true) + 10.0 + m.text_width(sub, false)
    });
    let w = (PAD_X + name_w + gap + value_w + PAD_X)
        .max(header_w + 2.0 * PAD_X)
        .max(MIN_BOX_W);
    let header_h = if header.is_some() {
        mt.row_h + 2.0
    } else {
        0.0
    };
    let vx = PAD_X + name_w + gap;
    let mut y = header_h;
    for s in &mut slots {
        s.name_at = (name_w > 0.0).then_some(Vec2::new(PAD_X, y + ROW_PAD));
        s.cell = Rect::new(vx - gap / 2.0, y, w - vx + gap / 2.0, mt.row_h);
        y += mt.row_h;
    }
    let mut b = empty_box(key, kind);
    b.rect = Rect::new(0.0, 0.0, w, y);
    b.slots = slots;
    b.header = header;
    b.header_h = header_h;
    b
}

fn object_box<M: Measure + ?Sized>(
    key: &str,
    o: &Object,
    snap: &Snapshot,
    m: &M,
    mt: &Metrics,
) -> DataBox {
    let mut b = if let Some(rows) = grid_rows(o, snap) {
        grid_box(key, &rows, m, mt)
    } else {
        match o.kind {
            ObjectKind::Array => array_box(key, o, m, mt),
            ObjectKind::Struct => {
                rows_box(key.to_string(), BoxKind::Struct, fields(o), None, m, mt)
            }
            ObjectKind::Map => rows_box(key.to_string(), BoxKind::Map, fields(o), None, m, mt),
            ObjectKind::Cell => {
                let mut slots = fields(o);
                for s in &mut slots {
                    s.name.clear();
                }
                rows_box(key.to_string(), BoxKind::Cell, slots, None, m, mt)
            }
        }
    };
    b.ty = o.ty.clone();
    b.size = o.size;
    b.addr = o.addr.clone();
    b.summary = o.summary.clone();
    b
}

fn fields(o: &Object) -> Vec<Slot> {
    o.fields
        .iter()
        .map(|f| slot(&f.name, &f.ty, &f.value))
        .collect()
}

/// A row of cells with the index under each.
fn array_box<M: Measure + ?Sized>(key: &str, o: &Object, m: &M, mt: &Metrics) -> DataBox {
    let elem_ty = o.fields.first().map(|f| f.ty.clone()).unwrap_or_default();
    let mut slots: Vec<Slot> = o
        .fields
        .iter()
        .map(|f| slot(&f.name, &f.ty, &f.value))
        .collect();
    if o.more != 0 {
        let text = if o.more > 0 {
            format!("+{}", o.more)
        } else {
            "…".to_string()
        };
        let mut s = slot("more", &elem_ty, &Cell::Scalar { v: text });
        s.link = Link::None;
        slots.push(s);
    }
    if slots.is_empty() {
        slots.push(slot("empty", "", &Cell::Scalar { v: "empty".into() }));
    }
    let mut b = empty_box(key.to_string(), BoxKind::Array);
    let mut x = 0.0;
    for s in &mut slots {
        let w = (value_width(s, m) + 2.0 * CELL_PAD).max(CELL_MIN);
        s.cell = Rect::new(x, 0.0, w, mt.row_h);
        if s.name != "more" && s.name != "empty" {
            let lw = m.text_width(&s.name, false) * 0.85;
            b.labels.push((
                s.name.clone(),
                Vec2::new(x + (w - lw) / 2.0, mt.row_h + INDEX_GAP),
            ));
        }
        x += w;
    }
    b.rect = Rect::new(0.0, 0.0, x, mt.row_h);
    b.slots = slots;
    b
}

/// A grid of scalars: row numbers on the left, column numbers on top.
fn grid_box<M: Measure + ?Sized>(
    key: &str,
    rows: &[(&str, &Object)],
    m: &M,
    mt: &Metrics,
) -> DataBox {
    let cols = rows.iter().map(|(_, r)| r.fields.len()).max().unwrap_or(0);
    let mut col_w = vec![CELL_MIN; cols];
    for (_, r) in rows {
        for (c, f) in r.fields.iter().enumerate() {
            let text = match &f.value {
                Cell::Scalar { v } => shorten(v),
                _ => String::new(),
            };
            col_w[c] = col_w[c].max(m.text_width(&text, false) + 2.0 * CELL_PAD);
        }
    }
    let label_w = (0..rows.len())
        .map(|r| m.text_width(&r.to_string(), false) * 0.85)
        .fold(0.0, f32::max)
        + 6.0;
    let top = mt.lh * 0.85 + INDEX_GAP;
    let mut b = empty_box(key.to_string(), BoxKind::Matrix);
    let mut x = 0.0;
    for (c, w) in col_w.iter().enumerate() {
        let lw = m.text_width(&c.to_string(), false) * 0.85;
        b.labels
            .push((c.to_string(), Vec2::new(label_w + x + (w - lw) / 2.0, 0.0)));
        x += w;
    }
    for (r, (_, row)) in rows.iter().enumerate() {
        let y = top + r as f32 * mt.row_h;
        b.labels.push((r.to_string(), Vec2::new(0.0, y + ROW_PAD)));
        let mut x = label_w;
        for (c, f) in row.fields.iter().enumerate() {
            let mut s = slot(&format!("[{r}][{c}]"), &f.ty, &f.value);
            s.cell = Rect::new(x, y, col_w[c], mt.row_h);
            x += col_w[c];
            b.slots.push(s);
        }
    }
    let w: f32 = col_w.iter().sum();
    // The outline is the cells; the labels sit above and left of it.
    b.rect = Rect::new(label_w, top, w, rows.len() as f32 * mt.row_h);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 7 units per character, 16-unit lines.
    pub struct Fixed;

    impl Measure for Fixed {
        fn text_width(&self, s: &str, bold: bool) -> f32 {
            s.chars().count() as f32 * if bold { 8.0 } else { 7.0 }
        }
        fn line_height(&self) -> f32 {
            16.0
        }
    }

    fn snap(json: &str) -> Snapshot {
        Snapshot::parse(json).unwrap()
    }

    fn node(key: &str, k: u32, left: Option<&str>, right: Option<&str>) -> String {
        let p = |t: Option<&str>| match t {
            Some(t) => format!(r#"{{"k":"p","to":"{t}","v":"0x1"}}"#),
            None => r#"{"k":"p","to":null,"v":"nullptr"}"#.to_string(),
        };
        format!(
            r#""{key}":{{"addr":"0x1","type":"Node","size":24,"kind":"struct","fields":[
              {{"name":"key","type":"int","value":{{"k":"s","v":"{k}"}}}},
              {{"name":"left","type":"Node *","value":{}}},
              {{"name":"right","type":"Node *","value":{}}}]}}"#,
            p(left),
            p(right)
        )
    }

    fn frame(vars: &str) -> String {
        format!(r#"{{"level":0,"func":"main","file":"m.cpp","line":9,"vars":[{vars}]}}"#)
    }

    fn ptr_var(name: &str, to: &str) -> String {
        format!(r#"{{"name":"{name}","type":"Node *","value":{{"k":"p","to":"{to}","v":"0x1"}}}}"#)
    }

    fn rect_of<'a>(l: &'a DataLayout, key: &str) -> &'a Rect {
        &l.boxes[l.find(key).unwrap()].rect
    }

    fn no_overlaps(l: &DataLayout) {
        for (i, a) in l.boxes.iter().enumerate() {
            for b in &l.boxes[i + 1..] {
                assert!(
                    !a.rect.inflate(-0.5).intersects(&b.rect.inflate(-0.5)),
                    "{} overlaps {}",
                    a.key,
                    b.key
                );
            }
        }
    }

    #[test]
    fn binary_tree_is_drawn_as_a_tree() {
        //        50
        //      30   70
        //        40
        let objects = [
            node("a", 50, Some("b"), Some("c")),
            node("b", 30, None, Some("d")),
            node("c", 70, None, None),
            node("d", 40, None, None),
        ]
        .join(",");
        let s = snap(&format!(
            r#"{{"frames":[{}],"objects":{{{objects}}}}}"#,
            frame(&ptr_var("root", "a"))
        ));
        let l = DataLayout::build(&s, &Fixed);
        let (a, b, c, d) = (
            rect_of(&l, "a"),
            rect_of(&l, "b"),
            rect_of(&l, "c"),
            rect_of(&l, "d"),
        );
        assert!(
            b.y > a.bottom() && c.y > a.bottom(),
            "children below the root"
        );
        assert_eq!(b.y, c.y, "siblings on one level");
        assert!(b.right() < c.x, "left child left of the right child");
        assert!(
            b.x < a.center().x && c.right() > a.center().x,
            "root centred over its children"
        );
        assert!(d.y > b.bottom(), "grandchild below");
        assert!(
            d.center().x > b.center().x,
            "a lone right child stays on the right"
        );
        let tree = l.edges.iter().filter(|e| e.kind == EdgeKind::Tree).count();
        assert_eq!(tree, 4, "frame→root and three child edges");
        assert!(l.edges.iter().all(|e| e.kind != EdgeKind::Cross));
        no_overlaps(&l);
    }

    fn list_node(key: &str, v: u32, next: Option<&str>, prev: Option<&str>) -> String {
        let p = |t: Option<&str>| match t {
            Some(t) => format!(r#"{{"k":"p","to":"{t}","v":"0x1"}}"#),
            None => r#"{"k":"p","to":null,"v":"nullptr"}"#.to_string(),
        };
        format!(
            r#""{key}":{{"addr":"0x1","type":"DNode","kind":"struct","fields":[
              {{"name":"value","type":"int","value":{{"k":"s","v":"{v}"}}}},
              {{"name":"prev","type":"DNode *","value":{}}},
              {{"name":"next","type":"DNode *","value":{}}}]}}"#,
            p(prev),
            p(next)
        )
    }

    #[test]
    fn doubly_linked_list_is_a_chain_with_back_curves() {
        let objects = [
            list_node("n1", 1, Some("n2"), None),
            list_node("n2", 2, Some("n3"), Some("n1")),
            list_node("n3", 3, None, Some("n2")),
        ]
        .join(",");
        let s = snap(&format!(
            r#"{{"frames":[{}],"objects":{{{objects}}}}}"#,
            frame(&ptr_var("head", "n1").replace("Node", "DNode"))
        ));
        let l = DataLayout::build(&s, &Fixed);
        let (a, b, c) = (rect_of(&l, "n1"), rect_of(&l, "n2"), rect_of(&l, "n3"));
        assert_eq!((a.y, b.y), (b.y, c.y), "one row");
        assert!(a.right() < b.x && b.right() < c.x, "left to right");
        let cross: Vec<_> = l
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Cross)
            .collect();
        assert_eq!(cross.len(), 2, "two prev pointers");
        assert!(cross.iter().all(|e| e.points.len() > 2), "drawn as curves");
        no_overlaps(&l);
    }

    #[test]
    fn arrays_are_cells_and_grids_are_grids() {
        let s = snap(
            r#"{"frames":[{"level":0,"func":"main","file":"m.cpp","line":3,"vars":[
                 {"name":"v","type":"std::vector<int>","value":{"k":"o","to":"v"}},
                 {"name":"g","type":"int [2][3]","value":{"k":"o","to":"g"}}]}],
               "objects":{
                 "v":{"addr":"0x1","type":"std::vector<int>","kind":"array","more":0,"fields":[
                    {"name":"0","value":{"k":"s","v":"4"}},{"name":"1","value":{"k":"s","v":"8"}},
                    {"name":"2","value":{"k":"s","v":"15"}}]},
                 "g":{"addr":"0x2","type":"int [2][3]","kind":"array","fields":[
                    {"name":"0","value":{"k":"o","to":"r0"}},{"name":"1","value":{"k":"o","to":"r1"}}]},
                 "r0":{"addr":"0x2","type":"int [3]","kind":"array","fields":[
                    {"name":"0","value":{"k":"s","v":"1"}},{"name":"1","value":{"k":"s","v":"2"}},{"name":"2","value":{"k":"s","v":"3"}}]},
                 "r1":{"addr":"0x3","type":"int [3]","kind":"array","fields":[
                    {"name":"0","value":{"k":"s","v":"4"}},{"name":"1","value":{"k":"s","v":"5"}},{"name":"2","value":{"k":"s","v":"6"}}]}}}"#,
        );
        let l = DataLayout::build(&s, &Fixed);
        let v = &l.boxes[l.find("v").unwrap()];
        assert_eq!(v.kind, BoxKind::Array);
        assert_eq!(v.slots.len(), 3);
        assert!(v
            .slots
            .windows(2)
            .all(|w| w[0].cell.right() <= w[1].cell.x + 0.01 && w[0].cell.y == w[1].cell.y));
        assert_eq!(v.labels.len(), 3, "index labels");
        let g = &l.boxes[l.find("g").unwrap()];
        assert_eq!(g.kind, BoxKind::Matrix);
        assert_eq!(g.slots.len(), 6);
        assert!(
            g.slots[3].cell.y > g.slots[0].cell.y,
            "second row below the first"
        );
        assert_eq!(g.slots[3].cell.x, g.slots[0].cell.x, "columns line up");
        assert!(l.find("r0").is_none(), "rows absorbed into the grid");
        no_overlaps(&l);
    }

    #[test]
    fn hash_buckets_hang_chains_downward() {
        let item = |k: &str, next: Option<&str>| {
            let n = next.map_or(r#"{"k":"p","to":null,"v":"nullptr"}"#.to_string(), |t| {
                format!(r#"{{"k":"p","to":"{t}","v":"0x1"}}"#)
            });
            format!(
                r#""{k}":{{"addr":"0x1","type":"Item","kind":"struct","fields":[
                  {{"name":"v","type":"int","value":{{"k":"s","v":"1"}}}},
                  {{"name":"next","type":"Item *","value":{n}}}]}}"#
            )
        };
        let s = snap(&format!(
            r#"{{"frames":[{{"level":0,"func":"main","file":"m.cpp","line":3,"vars":[
                 {{"name":"t","type":"Item *[2]","value":{{"k":"o","to":"arr"}}}}]}}],
               "objects":{{"arr":{{"addr":"0x9","type":"Item *[2]","kind":"array","fields":[
                   {{"name":"0","type":"Item *","value":{{"k":"p","to":"a","v":"0x1"}}}},
                   {{"name":"1","type":"Item *","value":{{"k":"p","to":"c","v":"0x1"}}}}]}},
                 {},{},{}}}}}"#,
            item("a", Some("b")),
            item("b", None),
            item("c", None)
        ));
        let l = DataLayout::build(&s, &Fixed);
        let (arr, a, b, c) = (
            rect_of(&l, "arr"),
            rect_of(&l, "a"),
            rect_of(&l, "b"),
            rect_of(&l, "c"),
        );
        assert!(
            a.y > arr.bottom() && c.y > arr.bottom(),
            "buckets below the array"
        );
        assert!(b.y > a.bottom(), "the chain goes down");
        assert_eq!(a.x, b.x);
        assert!(a.right() < c.x);
        no_overlaps(&l);
    }

    #[test]
    fn frames_stack_and_shared_objects_get_one_box() {
        let objects = [node("a", 1, None, None)].join(",");
        let s = snap(&format!(
            r#"{{"frames":[{},{}],"objects":{{{objects}}}}}"#,
            frame(&ptr_var("p", "a")),
            frame(&ptr_var("q", "a"))
        ));
        let l = DataLayout::build(&s, &Fixed);
        assert_eq!(l.frames, 2);
        assert!(l.boxes[1].rect.y > l.boxes[0].rect.bottom());
        assert_eq!(l.boxes[0].rect.w, l.boxes[1].rect.w, "frames share a width");
        let kinds: Vec<_> = l.edges.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![EdgeKind::Tree, EdgeKind::Cross]);
        assert!(rect_of(&l, "a").x > l.boxes[0].rect.right());
    }

    #[test]
    fn cycles_and_empty_snapshots_are_fine() {
        let objects = [
            list_node("x", 1, Some("y"), None),
            list_node("y", 2, Some("x"), None),
        ]
        .join(",");
        let s = snap(&format!(
            r#"{{"frames":[{}],"objects":{{{objects}}}}}"#,
            frame(&ptr_var("c", "x").replace("Node", "DNode"))
        ));
        let l = DataLayout::build(&s, &Fixed);
        assert_eq!(
            l.edges.iter().filter(|e| e.kind == EdgeKind::Cross).count(),
            1
        );
        assert!(l.bounds.w > 0.0 && l.bounds.h > 0.0);
        let empty = DataLayout::build(&Snapshot::default(), &Fixed);
        assert!(empty.boxes.is_empty() && empty.edges.is_empty());
        // A frame with no variables still draws.
        let s = snap(&format!(r#"{{"frames":[{}],"objects":{{}}}}"#, frame("")));
        assert_eq!(
            DataLayout::build(&s, &Fixed).boxes[0].slots[0].text,
            "no locals"
        );
    }

    #[test]
    fn same_type_pointer_matching() {
        assert!(same_type_ptr("Node *", "Node"));
        assert!(same_type_ptr("struct Node*", "Node"));
        assert!(same_type_ptr("const Node *", "Node"));
        assert!(!same_type_ptr("Node", "Node"));
        assert!(!same_type_ptr("Other *", "Node"));
    }

    #[test]
    fn long_lists_do_not_recurse() {
        let n = 3000;
        let objects: Vec<String> = (0..n)
            .map(|i| {
                let next = (i + 1 < n).then(|| format!("n{}", i + 1));
                list_node(&format!("n{i}"), i, next.as_deref(), None)
            })
            .collect();
        let s = snap(&format!(
            r#"{{"frames":[{}],"objects":{{{}}}}}"#,
            frame(&ptr_var("head", "n0").replace("Node", "DNode")),
            objects.join(",")
        ));
        let l = DataLayout::build(&s, &Fixed);
        assert_eq!(l.boxes.len(), n as usize + 1);
        let first = rect_of(&l, "n0");
        let last = rect_of(&l, &format!("n{}", n - 1));
        assert_eq!(first.y, last.y);
    }
}
