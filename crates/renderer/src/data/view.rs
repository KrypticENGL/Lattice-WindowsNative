//! The Data mode viewport: the latest [`Snapshot`] laid out by shape, with
//! pan, zoom, hover and focus.
//!
//! Types are abstracted away: boxes show names and values only. Focusing a
//! box (click) brings in its type, size and address in a chip above it and
//! each field's type beside its row, and dims what is not connected to it.
//!
//! Changes ease in rather than jump: the camera glides to a new framing,
//! after a step boxes slide from where they were to where they are now
//! while new ones fade in and changed values flash, and focus, hover and
//! the stale fade cross-fade. [`DataView::is_animating`] tells the host to
//! keep painting. Pointer drags and the wheel move the camera directly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use debugger::Snapshot;

use super::anim::{approach, Tween};
use super::layout::{outline, translate, BoxKind, DataLayout, EdgeKind, Link};
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
/// Alpha of everything while the program runs (the data is from the last
/// stop).
const STALE_ALPHA: f32 = 0.55;
/// Camera moves: Fit, the zoom buttons, revealing a frame, following stops.
const GLIDE: Duration = Duration::from_millis(320);
/// Boxes sliding to their new place after a step.
const SETTLE: Duration = Duration::from_millis(300);
/// Focus dimming and the stale fade, in seconds from one end to the other.
const FADE_S: f32 = 0.16;
const HOVER_FADE_S: f32 = 0.1;

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
    /// Centre the focused box once the pending layout is built.
    reveal: bool,
    gesture: Gesture,
    /// The program is running: the data shown is from the last stop.
    stale: bool,

    // Transitions; see the module docs.
    /// The camera gliding from one framing to another.
    glide: Option<(Camera, Camera, Tween)>,
    /// After a step, each box's previous drawn position minus its new one
    /// (`None`: new, it fades in). Boxes slide home as the tween runs.
    settle: Option<(Vec<Option<Vec2>>, Tween)>,
    /// Shown emphasis per box: 1 lit, 0 dimmed by the focus.
    lit: Vec<f32>,
    /// Shown hover per box, 0..=1.
    hovered: Vec<f32>,
    /// Shown focus for the edges: 0 nothing focused, 1 focused.
    focused: f32,
    /// Shown alpha for staleness: 1, or `STALE_ALPHA` while running.
    stale_alpha: f32,
    last_tick: Option<Instant>,
    /// Something was still in motion at the last paint.
    moving: bool,
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
            reveal: false,
            gesture: Gesture::None,
            stale: false,
            glide: None,
            settle: None,
            lit: Vec::new(),
            hovered: Vec::new(),
            focused: 0.0,
            stale_alpha: 1.0,
            last_tick: None,
            moving: false,
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

    /// A transition is under way: the host should paint again.
    pub fn is_animating(&self) -> bool {
        self.moving
    }

    /// Resizing follows at once (no glide).
    pub fn set_viewport(&mut self, r: Rect) {
        if r != self.camera.viewport() {
            self.camera.set_viewport(r);
            if let Some((from, to, _)) = &mut self.glide {
                from.set_viewport(r);
                to.set_viewport(r);
            }
            if self.follow {
                self.frame(false);
            }
        }
    }

    /// Frames everything, or the top-left part at a readable zoom when it
    /// is too big, and keeps doing so for later stops.
    pub fn fit(&mut self) {
        self.follow = true;
        self.frame(true);
    }

    fn frame(&mut self, glide: bool) {
        let Some(l) = &self.layout else {
            return;
        };
        let b = l.bounds;
        let mut to = self.camera;
        to.fit(b, FIT_MARGIN, FIT_MAX_ZOOM);
        if to.zoom() < READABLE_ZOOM {
            let v = to.viewport();
            let z = READABLE_ZOOM;
            let half = Vec2::new(v.w / z / 2.0, v.h / z / 2.0);
            let margin = FIT_MARGIN / z;
            to.look_at(Vec2::new(b.x - margin, b.y - margin) + half, z);
        }
        self.go(to, glide);
    }

    /// Moves the camera to `to`, gliding there from where it is now.
    fn go(&mut self, to: Camera, glide: bool) {
        if glide && to != self.camera {
            self.glide = Some((self.camera, to, Tween::new(GLIDE)));
        } else {
            self.camera = to;
            self.glide = None;
        }
    }

    /// Where the camera is heading.
    fn target(&self) -> Camera {
        self.glide.map_or(self.camera, |(_, to, _)| to)
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.follow = false;
        let mut to = self.target();
        to.zoom_at(to.viewport().center(), factor);
        self.go(to, true);
    }

    pub fn pan_by(&mut self, delta: Vec2) {
        self.follow = false;
        self.glide = None;
        self.camera.pan(delta);
    }

    /// Clears the focus. Returns `true` if there was one.
    pub fn clear_focus(&mut self) -> bool {
        self.focus_key.take().is_some()
    }

    /// Focuses the box of `snapshot.frames[i]` and centres it, at a
    /// readable zoom. Stepping then keeps the camera where it is.
    pub fn reveal_frame(&mut self, i: usize) {
        self.focus_key = Some(format!("frame:{i}"));
        self.follow = false;
        self.reveal = true;
        if !self.dirty {
            self.center_focus(true);
        }
    }

    /// The frame whose box has the focus (an index into
    /// `snapshot.frames`).
    pub fn focused_frame(&self) -> Option<usize> {
        self.focus_key
            .as_deref()?
            .strip_prefix("frame:")?
            .parse()
            .ok()
    }

    fn center_focus(&mut self, glide: bool) {
        self.reveal = false;
        let (Some(b), Some(l)) = (self.focus(), &self.layout) else {
            return;
        };
        let at = outline(&l.boxes[b]).center();
        let mut to = self.target();
        to.look_at(at, to.zoom().max(READABLE_ZOOM));
        self.go(to, glide);
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
        let old = self.layout.take();
        let Some(snap) = &self.snapshot else {
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
        let first = old.is_none();
        if let Some(old) = old {
            self.carry_over(&old, &l);
        }
        self.layout = Some(l);
        if self.focus().is_none() {
            self.focus_key = None;
        }
        if self.follow {
            self.frame(!first);
        } else if self.reveal {
            self.center_focus(!first);
        }
    }

    /// Starts the settle from the old layout to `new`: boxes are matched by
    /// key and start where they were drawn (mid-slide, if a step came
    /// quickly), keeping their shown focus and hover.
    fn carry_over(&mut self, old: &DataLayout, new: &DataLayout) {
        let offsets = self.offsets(old.boxes.len());
        let was: HashMap<&str, usize> = old
            .boxes
            .iter()
            .enumerate()
            .map(|(i, b)| (b.key.as_str(), i))
            .collect();
        let mut from = Vec::with_capacity(new.boxes.len());
        let mut lit = Vec::with_capacity(new.boxes.len());
        let mut hovered = Vec::with_capacity(new.boxes.len());
        for b in &new.boxes {
            let i = was.get(b.key.as_str()).copied();
            from.push(i.map(|i| {
                let o = &old.boxes[i].rect;
                Vec2::new(o.x - b.rect.x, o.y - b.rect.y) + offsets[i]
            }));
            let shown = |v: &[f32]| i.and_then(|i| v.get(i).copied()).unwrap_or(f32::NAN);
            lit.push(shown(&self.lit));
            hovered.push(shown(&self.hovered));
        }
        self.lit = lit;
        self.hovered = hovered;
        self.settle = Some((from, Tween::new(SETTLE)));
    }

    /// How far each of `n` boxes is drawn from its place right now.
    fn offsets(&self, n: usize) -> Vec<Vec2> {
        match &self.settle {
            Some((from, tw)) if from.len() == n => {
                let k = 1.0 - tw.eased();
                from.iter()
                    .map(|d| d.map_or(Vec2::ZERO, |d| d * k))
                    .collect()
            }
            _ => vec![Vec2::ZERO; n],
        }
    }

    /// Which boxes the focus leaves lit: all of them with no focus, else
    /// the focus and its neighbours.
    fn lit_targets(&self) -> Vec<bool> {
        let Some(l) = &self.layout else {
            return Vec::new();
        };
        let focus = self.focus();
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
        lit
    }

    /// Steps every transition to `now`.
    fn advance(&mut self, now: Instant) {
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        let mut moving = false;
        if let Some((from, to, tw)) = &mut self.glide {
            let k = tw.advance(now);
            self.camera = Camera::lerp(*from, *to, k);
            if tw.done() {
                self.glide = None;
            } else {
                moving = true;
            }
        }
        if let Some((_, tw)) = &mut self.settle {
            tw.advance(now);
            if tw.done() {
                self.settle = None;
            } else {
                moving = true;
            }
        }
        let lit = self.lit_targets();
        self.lit.resize(lit.len(), f32::NAN);
        self.hovered.resize(lit.len(), f32::NAN);
        let (fade, hover_fade) = (dt / FADE_S, dt / HOVER_FADE_S);
        for (i, on) in lit.into_iter().enumerate() {
            moving |= approach(&mut self.lit[i], f32::from(u8::from(on)), fade);
            let h = f32::from(u8::from(self.hover == Some(i)));
            moving |= approach(&mut self.hovered[i], h, hover_fade);
        }
        let focused = f32::from(u8::from(self.focus().is_some()));
        moving |= approach(&mut self.focused, focused, fade);
        let stale = if self.stale { STALE_ALPHA } else { 1.0 };
        moving |= approach(&mut self.stale_alpha, stale, fade * (1.0 - STALE_ALPHA));
        self.moving = moving;
        // Idle, the clock stops: the next change starts from its first
        // frame rather than jumping by the idle time.
        self.last_tick = moving.then_some(now);
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
                self.glide = None;
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
        self.glide = None;
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
        self.paint_at(c, style, Instant::now());
    }

    fn paint_at(&mut self, c: &mut dyn Canvas, style: &DataStyle, now: Instant) {
        self.refresh(&*c);
        self.advance(now);
        let Some(real) = &self.layout else { return };
        // Mid-settle, draw a copy with the boxes where they are on the way.
        let settling = self.settle.as_ref().map(|(from, tw)| (from, tw.eased()));
        let shown;
        let l = match settling {
            Some((from, _)) => {
                shown = displaced(real, &self.offsets(from.len()));
                &shown
            }
            None => real,
        };
        let k = settling.map_or(1.0, |(_, k)| k);
        let born = |i: usize| match settling {
            Some((from, k)) if from[i].is_none() => k,
            _ => 1.0,
        };
        let zoom = self.camera.zoom();
        let visible = self.camera.visible_world().inflate(40.0 / zoom);
        let focus = self.focus();
        let stale = self.stale_alpha;
        // A box's alpha: dimmed by the focus, faded in when new.
        let alpha = |i: usize| stale * born(i) * (style.dim + (1.0 - style.dim) * self.lit[i]);
        let fade = |c: Rgba, a: f32| c.faded(a);
        let text_on = zoom * c.line_height() >= MIN_TEXT_PX;
        let lh = c.line_height();

        c.set_transform(self.camera.transform());
        c.set_antialias(true);

        // Edges under the boxes, bucketed by colour, and apart while they
        // fade in with a new box.
        let w = 1.4f32.max(1.0 / zoom);
        let mut buckets: [[Vec<[Vec2; 2]>; 3]; 2] = Default::default();
        for e in &l.edges {
            let Some(bb) = Rect::bounding(&e.points) else {
                continue;
            };
            if !bb.inflate(ARROW).intersects(&visible) {
                continue;
            }
            let hot = focus.is_some_and(|f| e.from == f || e.to == f);
            let i = match (e.kind, hot, focus.is_some()) {
                (_, true, _) => 2,
                (EdgeKind::Cross, false, false) => 1,
                _ => 0,
            };
            let new = born(e.from).min(born(e.to)) < 1.0;
            let seg = &mut buckets[usize::from(new)][i];
            for p in e.points.windows(2) {
                seg.push([p[0], p[1]]);
            }
            if e.kind != EdgeKind::Embedded {
                arrowhead(seg, &e.points);
            }
        }
        // Edges away from the focus dim with it.
        let calm = stale * (1.0 + (style.dim - 1.0) * self.focused);
        let colors = [
            style.pointer.faded(calm),
            style.cross.faded(calm),
            style.highlight.faded(stale),
        ];
        for (set, new) in buckets.iter().zip([1.0, k]) {
            for (seg, color) in set.iter().zip(colors) {
                if !seg.is_empty() {
                    c.stroke_segments(seg, color.faded(new), w);
                }
            }
        }

        // Boxes. Ports are batched by their (rounded) alpha.
        let mut ports: Vec<(u8, Vec<Vec2>)> = Vec::new();
        for (bi, b) in l.boxes.iter().enumerate() {
            if !outline(b).intersects(&visible) {
                continue;
            }
            let on = alpha(bi);
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
                    // Fades in as the step settles, under a brief flash.
                    let cell = s.cell.inflate(-1.0);
                    c.fill_rect(cell, 2.0, fade(style.changed, on * k));
                    if k < 1.0 {
                        let flash = style.highlight.faded(on * (1.0 - k));
                        c.stroke_rect(cell, 2.0, flash, 1.5f32.max(1.0 / zoom));
                    }
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
            let h = self.hovered[bi];
            let (border, bw) = if focus == Some(bi) {
                (style.highlight, 2.0)
            } else {
                (style.border.mix(style.hover, h), 1.0f32 + 0.5 * h)
            };
            c.stroke_rect(b.rect, 4.0, fade(border, on), bw.max(1.0 / zoom));

            let q = (on * 32.0).round() as u8;
            for s in &b.slots {
                if matches!(s.link, Link::Pointer | Link::Embedded) && s.target.is_some() {
                    match ports.iter_mut().find(|(a, _)| *a == q) {
                        Some((_, p)) => p.push(s.port()),
                        None => ports.push((q, vec![s.port()])),
                    }
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
        for (q, p) in &ports {
            c.fill_discs(p, PORT_R, fade(style.pointer, f32::from(*q) / 32.0));
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

/// A copy of `l` with box `i` drawn `offsets[i]` from its place; edges
/// bend along, each end following its box.
fn displaced(l: &DataLayout, offsets: &[Vec2]) -> DataLayout {
    let mut out = l.clone();
    if offsets.len() != out.boxes.len() {
        return out;
    }
    for (b, d) in out.boxes.iter_mut().zip(offsets) {
        translate(b, *d);
    }
    for e in &mut out.edges {
        let (a, b) = (offsets[e.from], offsets[e.to]);
        let n = e.points.len().saturating_sub(1).max(1) as f32;
        for (j, p) in e.points.iter_mut().enumerate() {
            let t = j as f32 / n;
            *p = *p + a * (1.0 - t) + b * t;
        }
    }
    out
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
        assert_eq!(v.camera, moved, "Fit glides from where the camera is");
        settle(&mut v);
        assert_eq!(v.camera, framed);
        assert!(!v.is_animating());
    }

    /// Paints now and again once every transition has finished.
    fn settle(v: &mut DataView) {
        let t0 = Instant::now();
        v.paint_at(&mut Recorder::default(), &style(), t0);
        v.paint_at(
            &mut Recorder::default(),
            &style(),
            t0 + Duration::from_secs(2),
        );
    }

    #[test]
    fn the_camera_glides_and_direct_moves_cancel_it() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let framed = v.camera;
        let t0 = Instant::now();
        v.zoom_by(2.0);
        v.paint_at(&mut Recorder::default(), &style(), t0);
        assert_eq!(v.camera, framed, "the glide starts at its first frame");
        assert!(v.is_animating());
        v.paint_at(&mut Recorder::default(), &style(), t0 + GLIDE / 2);
        let mid = v.zoom();
        assert!(mid > framed.zoom() && mid < framed.zoom() * 2.0, "{mid}");
        // Zooming again builds on where the glide was heading.
        v.zoom_by(2.0);
        settle(&mut v);
        assert!((v.zoom() - framed.zoom() * 4.0).abs() < 1e-3);

        // Dragging takes over at once.
        v.fit();
        v.paint_at(&mut Recorder::default(), &style(), t0);
        v.pointer_down(Vec2::new(500.0, 500.0), 1);
        v.pointer_move(Vec2::new(560.0, 520.0), true);
        v.pointer_up(Vec2::new(560.0, 520.0));
        let dragged = v.camera;
        v.paint_at(&mut Recorder::default(), &style(), t0 + GLIDE);
        assert_eq!(v.camera, dragged);
    }

    #[test]
    fn a_step_settles_then_stops_painting() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        assert!(!v.is_animating(), "the first stop just appears");
        v.set_snapshot(snap(2));
        let t0 = Instant::now();
        v.paint_at(&mut Recorder::default(), &style(), t0);
        assert!(v.is_animating(), "the changed value flashes");
        v.paint_at(&mut Recorder::default(), &style(), t0 + SETTLE / 2);
        assert!(v.is_animating());
        v.paint_at(&mut Recorder::default(), &style(), t0 + SETTLE);
        assert!(!v.is_animating());
    }

    #[test]
    fn moved_boxes_slide_from_where_they_were() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        // Pretend box "a" was drawn 100 to the left at the last stop.
        let l = v.layout.clone().unwrap();
        let a = l.find("a").unwrap();
        let mut old = l.clone();
        translate(&mut old.boxes[a], Vec2::new(-100.0, 0.0));
        v.carry_over(&old, &l);
        let t0 = Instant::now();
        v.advance(t0);
        assert_eq!(v.offsets(l.boxes.len())[a], Vec2::new(-100.0, 0.0));
        v.advance(t0 + SETTLE / 2);
        let mid = v.offsets(l.boxes.len())[a].x;
        assert!(mid > -100.0 && mid < 0.0, "{mid}");
        let shown = displaced(&l, &v.offsets(l.boxes.len()));
        assert_eq!(shown.boxes[a].rect.x, l.boxes[a].rect.x + mid);
        v.advance(t0 + SETTLE);
        assert_eq!(v.offsets(l.boxes.len())[a], Vec2::ZERO);
    }

    #[test]
    fn hover_and_stale_fade() {
        let mut v = view();
        v.set_snapshot(snap(1));
        v.paint(&mut Recorder::default(), &style());
        let l = v.layout().unwrap();
        let a = l.find("a").unwrap();
        let at = v.camera.to_screen(l.boxes[a].rect.center());
        v.pointer_move(at, false);
        v.set_stale(true);
        let t0 = Instant::now();
        v.paint_at(&mut Recorder::default(), &style(), t0);
        v.paint_at(
            &mut Recorder::default(),
            &style(),
            t0 + Duration::from_millis(50),
        );
        assert!(v.hovered[a] > 0.0 && v.hovered[a] < 1.0, "{}", v.hovered[a]);
        assert!(v.stale_alpha < 1.0 && v.stale_alpha > STALE_ALPHA);
        v.paint_at(
            &mut Recorder::default(),
            &style(),
            t0 + Duration::from_secs(1),
        );
        assert_eq!((v.hovered[a], v.stale_alpha), (1.0, STALE_ALPHA));
        assert!(!v.is_animating());
    }

    #[test]
    fn revealing_a_frame_focuses_and_centres_it() {
        let mut v = view();
        v.set_snapshot(snap(1));
        // Before the first paint the layout is pending; it is centred then.
        v.reveal_frame(0);
        v.paint(&mut Recorder::default(), &style());
        assert_eq!(v.focused_frame(), Some(0));
        let b = outline(&v.layout().unwrap().boxes[0]).center();
        let on_screen = v.camera.to_screen(b);
        let mid = Rect::new(0.0, 0.0, 1000.0, 700.0).center();
        assert!((on_screen - mid).length() < 1e-3);
        assert!(v.zoom() >= READABLE_ZOOM);

        // A later stop keeps the camera and the focus.
        let cam = v.camera;
        v.set_snapshot(snap(2));
        v.paint(&mut Recorder::default(), &style());
        assert_eq!(v.camera, cam);
        assert_eq!(v.focused_frame(), Some(0));

        // Focusing an object box is not a frame.
        let l = v.layout().unwrap();
        let screen = v
            .camera
            .to_screen(l.boxes[l.find("a").unwrap()].rect.center());
        v.pointer_down(screen, 1);
        v.pointer_up(screen);
        assert_eq!(v.focused_frame(), None);
    }

    #[test]
    fn short_paths() {
        assert_eq!(short_path(r"C:\a\b\c.cpp"), "b/c.cpp");
        assert_eq!(short_path("c.cpp"), "c.cpp");
    }
}
