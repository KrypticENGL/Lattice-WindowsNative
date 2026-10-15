//! The Data mode viewport: the latest [`Snapshot`] laid out by shape, with
//! pan, zoom, hover and focus.
//!
//! Types are abstracted away: boxes show names and values only. Focusing a
//! box (click) brings in its type, size and address in a chip above it and
//! each field's type beside its row, and dims what is not connected to it.

use std::collections::HashMap;

use debugger::Snapshot;

use super::layout::{outline, BoxKind, DataLayout, EdgeKind, Link};
use crate::camera::Camera;
use crate::canvas::{Canvas, Rgba};
use crate::geom::{Rect, Vec2};
use crate::view::Cursor;

const DRAG_SLOP: f32 = 3.0;
const WHEEL_STEP: f32 = 1.2;
const FIT_MARGIN: f32 = 32.0;
const FIT_MAX_ZOOM: f32 = 1.4;
/// When fitting everything would make text smaller than this zoom, show
/// the top-left part at this zoom instead.
const READABLE_ZOOM: f32 = 0.7;
/// Below this on-screen line height (DIPs), text is not drawn.
const MIN_TEXT_PX: f32 = 5.0;
const PORT_R: f32 = 3.2;
const ARROW: f32 = 7.0;

/// Colours for the data view; filled from the UI theme.
#[derive(Debug, Clone, PartialEq)]
pub struct DataStyle {
    /// The canvas behind everything; dimmed boxes are painted over it so
    /// they stay opaque.
    pub canvas_bg: Rgba,
    pub frame_bg: Rgba,
    pub header_bg: Rgba,
    pub box_bg: Rgba,
    pub border: Rgba,
    pub grid_line: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    /// Ports and tree edges.
    pub pointer: Rgba,
    /// Back pointers, cycles, sharing.
    pub cross: Rgba,
    /// Fill behind a value that changed since the last stop.
    pub changed: Rgba,
    /// Focus ring and focused edges.
    pub highlight: Rgba,
    pub hover: Rgba,
    pub chip_bg: Rgba,
    pub chip_border: Rgba,
    /// Alpha multiplier for boxes unrelated to the focus.
    pub dim: f32,
}

/// What the host should do after an input call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataResponse {
    pub repaint: bool,
    pub cursor: Cursor,
    /// A frame was double-clicked: open its source at the line.
    pub open: Option<(String, u32)>,
}

enum Gesture {
    None,
    Press { at: Vec2 },
    Pan { last: Vec2 },
}

pub struct DataView {
    snapshot: Option<Snapshot>,
    layout: Option<DataLayout>,
    /// The snapshot changed; rebuild the layout at the next paint (it needs
    /// text metrics).
    dirty: bool,
    /// Values at the previous stop, keyed by (box, slot).
    previous: HashMap<(String, String), String>,
    camera: Camera,
    /// Frame each new stop until the user pans or zooms; Fit turns it back
    /// on.
    follow: bool,
    hover: Option<usize>,
    focus_key: Option<String>,
    gesture: Gesture,
    /// The program is running: the data shown is from the last stop.
    stale: bool,
}

impl Default for DataView {
    fn default() -> Self {
        Self::new()
    }
}

impl DataView {
    pub fn new() -> Self {
        Self {
            snapshot: None,
            layout: None,
            dirty: false,
            previous: HashMap::new(),
            camera: Camera::default(),
            follow: true,
            hover: None,
            focus_key: None,
            gesture: Gesture::None,
            stale: false,
        }
    }

    /// Shows a new stop. It is framed unless the user has moved the camera,
    /// in which case the camera stays put so stepping keeps the structure
    /// in place.
    pub fn set_snapshot(&mut self, snap: Snapshot) {
        self.snapshot = Some(snap);
        self.dirty = true;
        self.stale = false;
        self.hover = None;
    }

    /// Forgets everything (the session ended).
    pub fn clear(&mut self) {
        *self = Self {
            camera: self.camera,
            ..Self::new()
        };
    }

    pub fn set_stale(&mut self, stale: bool) {
        self.stale = stale;
    }

    pub fn is_empty(&self) -> bool {
        self.snapshot.is_none()
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn layout(&self) -> Option<&DataLayout> {
        self.layout.as_ref()
    }

    pub fn zoom(&self) -> f32 {
        self.camera.zoom()
    }

    pub fn set_viewport(&mut self, r: Rect) {
        if r != self.camera.viewport() {
            self.camera.set_viewport(r);
            if self.follow {
                self.frame();
            }
        }
    }

    /// Frames everything, or the top-left part at a readable zoom when it
    /// is too big, and keeps doing so for later stops.
    pub fn fit(&mut self) {
        self.follow = true;
        self.frame();
    }

    fn frame(&mut self) {
        let Some(l) = &self.layout else {
            return;
        };
        let b = l.bounds;
        self.camera.fit(b, FIT_MARGIN, FIT_MAX_ZOOM);
        if self.camera.zoom() < READABLE_ZOOM {
            let v = self.camera.viewport();
            let z = READABLE_ZOOM;
            let half = Vec2::new(v.w / z / 2.0, v.h / z / 2.0);
            let margin = FIT_MARGIN / z;
            self.camera
                .look_at(Vec2::new(b.x - margin, b.y - margin) + half, z);
        }
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.follow = false;
        let c = self.camera.viewport().center();
        self.camera.zoom_at(c, factor);
    }

    pub fn pan_by(&mut self, delta: Vec2) {
        self.follow = false;
        self.camera.pan(delta);
    }

    /// Clears the focus. Returns `true` if there was one.
    pub fn clear_focus(&mut self) -> bool {
        self.focus_key.take().is_some()
    }

    fn focus(&self) -> Option<usize> {
        let l = self.layout.as_ref()?;
        l.find(self.focus_key.as_deref()?)
    }

    /// Rebuilds the layout if the snapshot changed, flagging changed values.
    pub fn refresh<C: Canvas + ?Sized>(&mut self, c: &C) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let Some(snap) = &self.snapshot else {
            self.layout = None;
            return;
        };
        let mut l = DataLayout::build(snap, c);
        let mut values = HashMap::new();
        for b in &mut l.boxes {
            for s in &mut b.slots {
                let key = (b.key.clone(), s.name.clone());
                let now = s.target_key.clone().unwrap_or_else(|| s.raw.clone());
                s.changed = self.previous.get(&key).is_some_and(|before| *before != now);
                values.insert(key, now);
            }
        }
        self.previous = values;
        self.layout = Some(l);
        if self.focus().is_none() {
            self.focus_key = None;
        }
        if self.follow {
            self.frame();
        }
    }

    fn box_at(&self, screen: Vec2) -> Option<usize> {
        let l = self.layout.as_ref()?;
        l.box_at(self.camera.to_world(screen))
    }

    // ---- input ----------------------------------------------------------

    pub fn pointer_down(&mut self, pos: Vec2, clicks: u8) -> DataResponse {
        let hit = self.box_at(pos);
        if clicks >= 2 {
            self.gesture = Gesture::None;
            let open = hit.and_then(|b| self.layout.as_ref()?.boxes[b].source.clone());
            return DataResponse {
                repaint: false,
                cursor: cursor_for(hit),
                open,
            };
        }
        self.gesture = Gesture::Press { at: pos };
        DataResponse {
            cursor: cursor_for(hit),
            ..DataResponse::default()
        }
    }

    pub fn pointer_move(&mut self, pos: Vec2, button_down: bool) -> DataResponse {
        if !button_down {
            self.gesture = Gesture::None;
        }
        match self.gesture {
            Gesture::Press { at } if (pos - at).length() >= DRAG_SLOP => {
                self.gesture = Gesture::Pan { last: at };
                self.pointer_move(pos, true)
            }
            Gesture::Press { .. } => DataResponse::default(),
            Gesture::Pan { last } => {
                self.camera.pan(pos - last);
                self.follow = false;
                self.gesture = Gesture::Pan { last: pos };
                DataResponse {
                    repaint: true,
                    cursor: Cursor::Move,
                    open: None,
                }
            }
            Gesture::None => {
                let hover = self.box_at(pos);
                let changed = hover != self.hover;
                self.hover = hover;
                DataResponse {
                    repaint: changed,
                    cursor: cursor_for(hover),
                    open: None,
                }
            }
        }
    }

    pub fn pointer_up(&mut self, pos: Vec2) -> DataResponse {
        let repaint = match self.gesture {
            Gesture::Press { .. } => {
                let hit = self.box_at(pos);
                let key = hit.and_then(|b| Some(self.layout.as_ref()?.boxes[b].key.clone()));
                let changed = key != self.focus_key;
                self.focus_key = key;
                changed
            }
            _ => true,
        };
        self.gesture = Gesture::None;
        DataResponse {
            repaint,
            cursor: cursor_for(self.box_at(pos)),
            open: None,
        }
    }

    pub fn pointer_leave(&mut self) -> DataResponse {
        DataResponse {
            repaint: self.hover.take().is_some(),
            ..DataResponse::default()
        }
    }

    pub fn wheel(&mut self, pos: Vec2, notches: f32) -> DataResponse {
        self.camera.zoom_at(pos, WHEEL_STEP.powf(notches));
        self.follow = false;
        DataResponse {
            repaint: true,
            cursor: cursor_for(self.hover),
            open: None,
        }
    }

    // ---- painting -------------------------------------------------------

    /// Draws into the viewport. The caller clears the background and clips.
    pub fn paint(&mut self, c: &mut dyn Canvas, style: &DataStyle) {
        self.refresh(&*c);
        let Some(l) = &self.layout else { return };
        let zoom = self.camera.zoom();
        let visible = self.camera.visible_world().inflate(40.0 / zoom);
        let focus = self.focus();
        // Boxes connected to the focus stay lit.
        let mut lit = vec![focus.is_none(); l.boxes.len()];
        if let Some(f) = focus {
            lit[f] = true;
            for e in &l.edges {
                if e.from == f {
                    lit[e.to] = true;
                }
                if e.to == f {
                    lit[e.from] = true;
                }
            }
        }
        let stale = if self.stale { 0.55 } else { 1.0 };
        let fade = |c: Rgba, on: bool| c.faded(stale * if on { 1.0 } else { style.dim });
        let text_on = zoom * c.line_height() >= MIN_TEXT_PX;
        let lh = c.line_height();

        c.set_transform(self.camera.transform());
        c.set_antialias(true);

        // Edges under the boxes, bucketed by colour.
        let w = 1.4f32.max(1.0 / zoom);
        let mut buckets: [Vec<[Vec2; 2]>; 4] = Default::default();
        for e in &l.edges {
            let Some(bb) = Rect::bounding(&e.points) else {
                continue;
            };
            if !bb.inflate(ARROW).intersects(&visible) {
                continue;
            }
            let hot = focus.is_some_and(|f| e.from == f || e.to == f);
            let i = match (e.kind, hot, focus.is_some()) {
                (_, true, _) => 3,
                (EdgeKind::Cross, false, false) => 1,
                (_, false, false) => 0,
                (_, false, true) => 2,
            };
            let seg = &mut buckets[i];
            for p in e.points.windows(2) {
                seg.push([p[0], p[1]]);
            }
            if e.kind != EdgeKind::Embedded {
                arrowhead(seg, &e.points);
            }
        }
        let colors = [
            fade(style.pointer, true),
            fade(style.cross, true),
            fade(style.pointer, false),
            fade(style.highlight, true),
        ];
        for (seg, color) in buckets.iter().zip(colors) {
            if !seg.is_empty() {
                c.stroke_segments(seg, color, w);
            }
        }

        // Boxes.
        let mut ports: [Vec<Vec2>; 2] = Default::default();
        for (bi, b) in l.boxes.iter().enumerate() {
            if !outline(b).intersects(&visible) {
                continue;
            }
            let on = lit[bi];
            let bg = if b.kind == BoxKind::Frame {
                style.frame_bg
            } else {
                style.box_bg
            };
            // Opaque even when dimmed, so edges underneath stay hidden.
            c.fill_rect(b.rect, 4.0, style.canvas_bg);
            c.fill_rect(b.rect, 4.0, fade(bg, on));
            if b.header.is_some() {
                c.fill_rect(
                    Rect::new(b.rect.x, b.rect.y, b.rect.w, b.header_h),
                    4.0,
                    fade(style.header_bg, on),
                );
            }
            for s in &b.slots {
                if s.changed {
                    c.fill_rect(s.cell.inflate(-1.0), 2.0, fade(style.changed, on));
                }
            }
            // Cell and row separators.
            let mut lines = Vec::new();
            match b.kind {
                BoxKind::Array | BoxKind::Matrix => {
                    for s in &b.slots {
                        if s.cell.x > b.rect.x + 0.5 {
                            lines.push([
                                Vec2::new(s.cell.x, s.cell.y),
                                Vec2::new(s.cell.x, s.cell.bottom()),
                            ]);
                        }
                        if s.cell.y > b.rect.y + 0.5 {
                            lines.push([
                                Vec2::new(s.cell.x, s.cell.y),
                                Vec2::new(s.cell.right(), s.cell.y),
                            ]);
                        }
                    }
                }
                _ => {
                    for s in b.slots.iter().skip(1) {
                        lines.push([
                            Vec2::new(b.rect.x + 1.0, s.cell.y),
                            Vec2::new(b.rect.right() - 1.0, s.cell.y),
                        ]);
                    }
                    if b.header.is_some() {
                        let y = b.rect.y + b.header_h;
                        lines.push([Vec2::new(b.rect.x, y), Vec2::new(b.rect.right(), y)]);
                    }
                }
            }
            if !lines.is_empty() {
                c.stroke_segments(&lines, fade(style.grid_line, on), 1.0f32.max(0.75 / zoom));
            }
            let (border, bw) = if focus == Some(bi) {
                (style.highlight, 2.0)
            } else if self.hover == Some(bi) {
                (style.hover, 1.5)
            } else {
                (style.border, 1.0f32)
            };
            c.stroke_rect(b.rect, 4.0, fade(border, on), bw.max(1.0 / zoom));

            for s in &b.slots {
                if matches!(s.link, Link::Pointer | Link::Embedded) && s.target.is_some() {
                    ports[usize::from(!on)].push(s.port());
                }
            }
            if !text_on {
                continue;
            }
            if let Some((title, sub)) = &b.header {
                let y = b.rect.y + (b.header_h - lh) / 2.0;
                c.text(
                    title,
                    Vec2::new(b.rect.x + 8.0, y),
                    fade(style.text, on),
                    true,
                );
                let x = b.rect.right() - 8.0 - c.measure_text(sub, false);
                c.text(sub, Vec2::new(x, y), fade(style.text_dim, on), false);
            }
            let centred = matches!(b.kind, BoxKind::Array | BoxKind::Matrix);
            for s in &b.slots {
                if let Some(at) = s.name_at {
                    c.text(&s.name, at, fade(style.text_dim, on), false);
                }
                if s.text.is_empty() {
                    continue;
                }
                let dim = matches!(s.link, Link::Null | Link::Invalid) || s.name == "more";
                let color = if dim { style.text_dim } else { style.text };
                let tw = c.measure_text(&s.text, false);
                let x = if centred {
                    s.cell.x + (s.cell.w - tw) / 2.0
                } else {
                    s.cell.x + 7.0
                };
                let y = s.cell.y + (s.cell.h - lh) / 2.0;
                c.text(&s.text, Vec2::new(x, y), fade(color, on), false);
            }
            for (label, at) in &b.labels {
                c.text(label, *at, fade(style.text_dim, on).faded(0.8), false);
            }
        }
        if !ports[0].is_empty() {
            c.fill_discs(&ports[0], PORT_R, fade(style.pointer, true));
        }
        if !ports[1].is_empty() {
            c.fill_discs(&ports[1], PORT_R, fade(style.pointer, false));
        }

        if let (Some(f), true) = (focus, text_on) {
            self.paint_types(c, l, f, style);
        }
        c.reset_transform();
    }

    /// The focused box's type chip and per-field types: the only place
    /// types appear.
    fn paint_types(&self, c: &mut dyn Canvas, l: &DataLayout, f: usize, style: &DataStyle) {
        let b = &l.boxes[f];
        let lh = c.line_height();
        let chip = if b.kind == BoxKind::Frame {
            b.source
                .as_ref()
                .map(|(file, line)| format!("{}:{line}", short_path(file)))
        } else {
            let mut s = b.ty.clone();
            if b.size > 0 {
                s.push_str(&format!(" · {} B", b.size));
            }
            if let Some(a) = &b.addr {
                s.push_str(&format!(" · {a}"));
            }
            if let Some(summary) = &b.summary {
                if !summary.is_empty() {
                    s.push_str(&format!(" · {summary}"));
                }
            }
            Some(s)
        };
        let o = outline(b);
        if let Some(chip) = chip {
            let w = c.measure_text(&chip, false) + 16.0;
            let r = Rect::new(o.x, o.y - lh - 14.0, w, lh + 8.0);
            c.fill_rect(r, 4.0, style.chip_bg);
            c.stroke_rect(r, 4.0, style.highlight.with_alpha(0.6), 1.0);
            c.text(
                &chip,
                Vec2::new(r.x + 8.0, r.y + 4.0),
                style.highlight,
                false,
            );
        }
        // Field types beside rows (row boxes) and the element type under
        // arrays' chips is already in the chip.
        if matches!(
            b.kind,
            BoxKind::Frame | BoxKind::Struct | BoxKind::Map | BoxKind::Cell
        ) {
            let types: Vec<&str> = b.slots.iter().map(|s| s.ty.as_str()).collect();
            if types.iter().all(|t| t.is_empty()) {
                return;
            }
            let w = types
                .iter()
                .map(|t| c.measure_text(t, false))
                .fold(0.0, f32::max)
                + 16.0;
            let r = Rect::new(
                b.rect.right() + 6.0,
                b.rect.y + b.header_h,
                w,
                b.rect.h - b.header_h,
            );
            c.fill_rect(r, 4.0, style.chip_bg);
            c.stroke_rect(r, 4.0, style.chip_border, 1.0);
            for (s, t) in b.slots.iter().zip(types) {
                let y = s.cell.y + (s.cell.h - lh) / 2.0;
                c.text(t, Vec2::new(r.x + 8.0, y), style.text_dim, false);
            }
        }
    }
}

fn cursor_for(hit: Option<usize>) -> Cursor {
    if hit.is_some() {
        Cursor::Hand
    } else {
        Cursor::Arrow
    }
}

/// A chevron at the end of a polyline.
fn arrowhead(seg: &mut Vec<[Vec2; 2]>, points: &[Vec2]) {
    let n = points.len();
    if n < 2 {
        return;
    }
    let tip = points[n - 1];
    let d = tip - points[n - 2];
    let len = d.length();
    if len <= 0.0 {
        return;
    }
    let u = d * (1.0 / len);
    let side = Vec2::new(-u.y, u.x);
    let back = tip - u * ARROW;
    seg.push([tip, back + side * (ARROW * 0.5)]);
    seg.push([tip, back - side * (ARROW * 0.5)]);
}

/// The last two components of a path.
fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let k = parts.len().saturating_sub(2);
    parts[k..].join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records text draws.
    #[derive(Default)]
    struct Recorder {
        texts: Vec<String>,
        rects: usize,
    }

    impl Canvas for Recorder {
        fn set_transform(&mut self, _: crate::geom::Affine) {}
        fn reset_transform(&mut self) {}
        fn set_antialias(&mut self, _: bool) {}
        fn stroke_segments(&mut self, _: &[[Vec2; 2]], _: Rgba, _: f32) {}
        fn fill_discs(&mut self, _: &[Vec2], _: f32, _: Rgba) {}
        fn fill_squares(&mut self, _: &[Vec2], _: f32, _: Rgba) {}
        fn stroke_circle(&mut self, _: Vec2, _: f32, _: Rgba, _: f32) {}
        fn fill_rect(&mut self, _: Rect, _: f32, _: Rgba) {
            self.rects += 1;
        }
        fn stroke_rect(&mut self, _: Rect, _: f32, _: Rgba, _: f32) {}
        fn text(&mut self, s: &str, _: Vec2, _: Rgba, _: bool) {
            self.texts.push(s.to_string());
        }
        fn measure_text(&self, s: &str, _: bool) -> f32 {
            s.chars().count() as f32 * 7.0
        }
        fn line_height(&self) -> f32 {
            16.0
        }
    }

    fn style() -> DataStyle {
        let c = Rgba::new(1.0, 1.0, 1.0, 1.0);
        DataStyle {
            canvas_bg: c,
            frame_bg: c,
            header_bg: c,
            box_bg: c,
            border: c,
            grid_line: c,
            text: c,
            text_dim: c,
            pointer: c,
            cross: c,
            changed: c,
            highlight: c,
            hover: c,
            chip_bg: c,
            chip_border: c,
            dim: 0.2,
        }
    }

    fn snap(key: u32) -> Snapshot {
        Snapshot::parse(&format!(
            r#"{{"frames":[{{"level":0,"func":"main","file":"C:/src/m.cpp","line":7,"vars":[
                {{"name":"p","type":"Node *","value":{{"k":"p","to":"a","v":"0x10"}}}}]}}],
              "objects":{{"a":{{"addr":"0x10","type":"Node","size":16,"kind":"struct","fields":[
                {{"name":"key","type":"int","value":{{"k":"s","v":"{key}"}}}}]}}}}}}"#
        ))
        .unwrap()
    }

    fn view() -> DataView {
        let mut v = DataView::new();
        v.set_viewport(Rect::new(0.0, 0.0, 1000.0, 700.0));
        v
    }

    #[test]
    fn types_show_only_for_the_focused_box() {
        let mut v = view();
        v.set_snapshot(snap(1));
        let mut c = Recorder::default();
        v.paint(&mut c, &style());
        assert!(c.texts.iter().any(|t| t == "key"));
        assert!(
            !c.texts.iter().any(|t| t.contains("Node") || t == "int"),
            "{:?}",
            c.texts
        );

        // Click the object box.
        let l = v.layout().unwrap();
        let center = l.boxes[l.find("a").unwrap()].rect.center();
        let screen = v.camera.to_screen(center);
        v.pointer_down(screen, 1);
        assert!(v.pointer_up(screen).repaint);
        let mut c = Recorder::default();
        v.paint(&mut c, &style());
        assert!(
            c.texts.iter().any(|t| t.starts_with("Node · 16 B · 0x10")),
            "{:?}",
            c.texts
        );
        assert!(c.texts.iter().any(|t| t == "int"));

        // Clicking empty space clears the focus.
        v.pointer_down(Vec2::new(990.0, 690.0), 1);
        v.pointer_up(Vec2::new(990.0, 690.0));
        let mut c = Recorder::default();
        v.paint(&mut c, &style());
        assert!(!c.texts.iter().any(|t| t.contains("Node")));
    }

    #[test]
    fn changed_values_are_flagged_after_a_step() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let slot = |v: &DataView| {
            let l = v.layout().unwrap();
            l.boxes[l.find("a").unwrap()].slots[0].changed
        };
        assert!(!slot(&v));
        v.set_snapshot(snap(2));
        v.paint(&mut Recorder::default(), &style());
        assert!(slot(&v));
        v.set_snapshot(snap(2));
        v.paint(&mut Recorder::default(), &style());
        assert!(!slot(&v));
    }

    #[test]
    fn double_clicking_a_frame_opens_its_source() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let frame = v.layout().unwrap().boxes[0].rect.center();
        let r = v.pointer_down(v.camera.to_screen(frame), 2);
        assert_eq!(r.open, Some(("C:/src/m.cpp".to_string(), 7)));
    }

    #[test]
    fn dragging_pans() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let before = v.camera.to_world(Vec2::new(500.0, 500.0));
        v.pointer_down(Vec2::new(500.0, 500.0), 1);
        let r = v.pointer_move(Vec2::new(560.0, 520.0), true);
        assert_eq!(r.cursor, Cursor::Move);
        v.pointer_up(Vec2::new(560.0, 520.0));
        let after = v.camera.to_world(Vec2::new(560.0, 520.0));
        assert!((after - before).length() < 1e-3);
    }

    #[test]
    fn new_stops_are_framed_until_the_user_moves_the_camera() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let framed = v.camera;
        v.pan_by(Vec2::new(200.0, 0.0));
        let moved = v.camera;
        v.set_snapshot(snap(2));
        v.paint(&mut Recorder::default(), &style());
        assert_eq!(v.camera, moved, "stepping keeps the user's camera");
        v.fit();
        assert_eq!(v.camera, framed);
    }

    #[test]
    fn short_paths() {
        assert_eq!(short_path(r"C:\a\b\c.cpp"), "b/c.cpp");
        assert_eq!(short_path("c.cpp"), "c.cpp");
    }
}
