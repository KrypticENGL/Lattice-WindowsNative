//! The Memory panel: the bytes of one variable or object at the last stop,
//! laid out by field. Each field is a row (offset, name, value, bytes),
//! padding between fields gets a row of its own, and the raw bytes below
//! are tinted by the field they belong to, so a struct's layout reads at a
//! glance. Bytes that changed since the previous stop are lit.
//!
//! Opened from the "View in memory" hover card on a variable in the Data
//! view, the call graph or the source; it follows its target from stop to
//! stop. It needs a snapshot, so the program must have run (or stopped)
//! under the debugger at least once.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use debugger::{Cell, Field, ObjectKind, Snapshot, Var};

use crate::event::{Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const HEADER_H: f32 = 30.0;
const PAD: f32 = 10.0;
const TITLE_H: f32 = 46.0;
const LABEL_H: f32 = 24.0;
const NAV_W: f32 = 22.0;
/// Bytes shown in a field row before an ellipsis.
const ROW_BYTES: usize = 4;

/// What the user asked to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemTarget {
    /// A Data view slot: `key` is an object id or `frame:N`.
    Slot {
        key: String,
        name: String,
    },
    /// `a`, `a.b` or `p->next->key` in `file`, read in the innermost frame
    /// stopped in that file.
    Expr {
        file: PathBuf,
        path: Vec<String>,
    },
    /// Call graph: a data member, in every live object of its class.
    Field {
        class: String,
        name: String,
    },
    /// Call graph: a parameter or local, in every frame of its function.
    Local {
        func: String,
        name: String,
    },
    Global {
        name: String,
    },
}

/// One variable or object to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub title: String,
    pub ty: String,
    pub addr: Option<u64>,
    pub size: u64,
    /// The first bytes (GDB sends at most 256).
    pub bytes: Vec<u8>,
    /// By offset, padding included; empty when the layout is unknown
    /// (pretty-printed containers keep their elements elsewhere).
    pub rows: Vec<Row>,
    /// The row the user pointed at.
    pub focus: Option<usize>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    pub ty: String,
    pub value: String,
    pub off: u64,
    pub size: u64,
    pub padding: bool,
}

impl Region {
    /// The row each shown byte belongs to.
    fn owners(&self) -> Vec<Option<usize>> {
        let mut owners = vec![None; self.bytes.len()];
        for (i, r) in self.rows.iter().enumerate() {
            let end = (r.off + r.size).min(owners.len() as u64);
            for o in &mut owners[r.off.min(end) as usize..end as usize] {
                // The first field wins (bitfields share bytes).
                o.get_or_insert(i);
            }
        }
        owners
    }
}

/// Everything `t` names in `snap`; several for a class's fields or a
/// recursive function's locals, none when it is not live.
pub fn resolve(snap: &Snapshot, t: &MemTarget) -> Vec<Region> {
    match t {
        MemTarget::Slot { key, name } => slot(snap, key, name).into_iter().collect(),
        MemTarget::Expr { file, path } => expr(snap, file, path).into_iter().collect(),
        MemTarget::Field { class, name } => {
            let objects = snap
                .objects
                .iter()
                .filter(|(_, o)| same_type(&o.ty, class))
                .filter_map(|(k, _)| object_region(snap, k, None, Some(name)));
            let globals = snap
                .globals
                .iter()
                .filter(|g| same_type(&g.ty, class))
                .map(|g| focused(var_region(snap, g, g.name.clone()), name));
            let mut all: Vec<Region> = objects.chain(globals).collect();
            all.sort_by_key(|r| r.addr);
            all
        }
        MemTarget::Local { func, name } => snap
            .frames
            .iter()
            .rev()
            .filter(|f| same_func(&f.func, func))
            .filter_map(|f| {
                let v = f.vars.iter().find(|v| v.name == *name)?;
                let title = format!("{name} in {} (line {})", f.func, f.line);
                Some(var_region(snap, v, title))
            })
            .collect(),
        MemTarget::Global { name } => snap
            .globals
            .iter()
            .filter(|g| same_func(&g.name, name))
            .map(|g| var_region(snap, g, g.name.clone()))
            .collect(),
    }
}

fn slot(snap: &Snapshot, key: &str, name: &str) -> Option<Region> {
    if let Some(i) = key.strip_prefix("frame:") {
        let f = snap.frames.get(i.parse::<usize>().ok()?)?;
        let v = f.vars.iter().find(|v| v.name == name)?;
        return Some(var_region(snap, v, name.to_string()));
    }
    let o = snap.objects.get(key)?;
    let field = o.fields.iter().find(|f| f.name == name)?;
    // A container element lives outside the container's own bytes: show
    // the element when it is an object.
    if let (None, Cell::Embedded { to }) = (field.off, &field.value) {
        return object_region(snap, to, Some(format!("[{name}]")), None);
    }
    object_region(snap, key, None, Some(name))
}

fn expr(snap: &Snapshot, file: &Path, path: &[String]) -> Option<Region> {
    let (first, rest) = path.split_first()?;
    let frame = snap
        .frames
        .iter()
        .rev()
        .find(|f| same_file(Path::new(&f.file), file));
    if let Some(v) = frame.and_then(|f| f.vars.iter().find(|v| v.name == *first)) {
        if rest.is_empty() {
            return Some(var_region(snap, v, first.clone()));
        }
        return walk(snap, &v.value, first.clone(), rest);
    }
    if let Some(g) = snap.globals.iter().find(|g| g.name == *first) {
        let r = var_region(snap, g, first.clone());
        return match rest {
            [] => Some(r),
            [field] => Some(focused(r, field)).filter(|r| r.focus.is_some()),
            _ => None,
        };
    }
    // A bare member inside a method: `key` for `this->key`.
    let this = frame?.vars.iter().find(|v| v.name == "this")?;
    walk(snap, &this.value, "this".into(), path)
}

/// Follows `path` through fields from `cell`; the last name is shown as a
/// focused row of the object holding it.
fn walk<'a>(
    snap: &'a Snapshot,
    mut cell: &'a Cell,
    mut title: String,
    path: &[String],
) -> Option<Region> {
    for (i, name) in path.iter().enumerate() {
        let key = cell.target()?;
        let holder = match cell {
            Cell::Pointer { .. } => format!("*{title}"),
            _ => title.clone(),
        };
        let field = snap
            .objects
            .get(key)?
            .fields
            .iter()
            .find(|f| f.name == *name)?;
        if i + 1 == path.len() {
            return object_region(snap, key, Some(holder), Some(name));
        }
        let sep = if matches!(cell, Cell::Pointer { .. }) {
            "->"
        } else {
            "."
        };
        title = format!("{title}{sep}{name}");
        cell = &field.value;
    }
    None
}

fn object_region(
    snap: &Snapshot,
    key: &str,
    title: Option<String>,
    focus: Option<&str>,
) -> Option<Region> {
    let o = snap.objects.get(key)?;
    let addr = o.addr.as_deref().and_then(parse_addr);
    let title = title.unwrap_or_else(|| match addr {
        Some(a) => format!("{} @ {a:#x}", o.ty),
        None => o.ty.clone(),
    });
    let rows = match (o.kind, o.fields.as_slice()) {
        // A pointer to a scalar: the one value fills the cell.
        (ObjectKind::Cell, [f]) if f.off.is_none() => vec![Row {
            name: "*".into(),
            ty: o.ty.clone(),
            value: cell_text(&f.value).into(),
            off: 0,
            size: o.size,
            padding: false,
        }],
        (_, fields) => layout_rows(fields, o.size, o.more == 0),
    };
    let r = Region {
        title,
        ty: o.ty.clone(),
        addr,
        size: o.size,
        bytes: o.bytes.clone(),
        rows,
        focus: None,
        summary: o.summary.clone(),
    };
    Some(match focus {
        Some(name) => focused(r, name),
        None => r,
    })
}

fn var_region(snap: &Snapshot, v: &Var, title: String) -> Region {
    if let Cell::Embedded { to } = &v.value {
        if let Some(r) = object_region(snap, to, Some(title.clone()), None) {
            return r;
        }
    }
    let rows = if v.fields.is_empty() {
        vec![Row {
            name: v.name.clone(),
            ty: v.ty.clone(),
            value: cell_text(&v.value).into(),
            off: 0,
            size: v.size,
            padding: false,
        }]
    } else {
        layout_rows(&v.fields, v.size, true)
    };
    Region {
        title,
        ty: v.ty.clone(),
        addr: v.addr.as_deref().and_then(parse_addr),
        size: v.size,
        bytes: v.bytes.clone(),
        rows,
        focus: None,
        summary: None,
    }
}

fn focused(mut r: Region, name: &str) -> Region {
    r.focus = r
        .rows
        .iter()
        .position(|row| !row.padding && row.name == name);
    r
}

/// Fields with offsets in order, gaps filled with padding rows. `complete`:
/// every field is listed, so bytes after the last one are tail padding.
fn layout_rows(fields: &[Field], size: u64, complete: bool) -> Vec<Row> {
    let mut placed: Vec<(&Field, u64)> = fields.iter().filter_map(|f| Some((f, f.off?))).collect();
    placed.sort_by_key(|&(_, off)| off);
    let pad = |off, size| Row {
        name: "padding".into(),
        ty: String::new(),
        value: String::new(),
        off,
        size,
        padding: true,
    };
    let mut rows = Vec::new();
    let mut at = 0;
    for (f, off) in placed {
        if off > at {
            rows.push(pad(at, off - at));
        }
        let size = f.size.unwrap_or(0);
        rows.push(Row {
            name: f.name.clone(),
            ty: f.ty.clone(),
            value: cell_text(&f.value).into(),
            off,
            size,
            padding: false,
        });
        at = at.max(off + size);
    }
    if complete && !rows.is_empty() && size > at {
        rows.push(pad(at, size - at));
    }
    rows
}

fn cell_text(c: &Cell) -> &str {
    match c {
        Cell::Scalar { v } | Cell::Pointer { v, .. } => v,
        Cell::Embedded { .. } => "{\u{2026}}",
    }
}

fn parse_addr(s: &str) -> Option<u64> {
    u64::from_str_radix(s.strip_prefix("0x")?, 16).ok()
}

/// `const struct ns::Node` and `Node` name the same class.
fn same_type(a: &str, b: &str) -> bool {
    fn bare(s: &str) -> &str {
        let mut s = s.trim();
        for p in ["const ", "volatile ", "struct ", "class ", "union "] {
            s = s.strip_prefix(p).unwrap_or(s);
        }
        s
    }
    same_qualified(bare(a), bare(b))
}

/// GDB's `Tree::insert(Node*, int)` and the graph's `insert` name the same
/// function.
fn same_func(a: &str, b: &str) -> bool {
    let bare = |s: &str| s.split('(').next().unwrap_or(s).trim().to_string();
    same_qualified(&bare(a), &bare(b))
}

fn same_qualified(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("::{b}")) || b.ends_with(&format!("::{a}"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// The member chain ending at the identifier under byte `col` of `line`
/// (`p->next` when over `next`) and that identifier's byte range. `None`
/// off an identifier or when the chain starts with a call or index.
pub fn expr_at(line: &str, col: usize) -> Option<(Vec<String>, Range<usize>)> {
    let b = line.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if col >= b.len() || !is_id(b[col]) {
        return None;
    }
    let mut s = col;
    while s > 0 && is_id(b[s - 1]) {
        s -= 1;
    }
    let mut e = col;
    while e < b.len() && is_id(b[e]) {
        e += 1;
    }
    if b[s].is_ascii_digit() {
        return None;
    }
    let mut path = vec![&line[s..e]];
    let mut i = s;
    loop {
        let j = if i >= 1 && b[i - 1] == b'.' {
            i - 1
        } else if i >= 2 && &b[i - 2..i] == b"->" {
            i - 2
        } else {
            break;
        };
        let mut k = j;
        while k > 0 && is_id(b[k - 1]) {
            k -= 1;
        }
        if k == j || b[k].is_ascii_digit() {
            return None;
        }
        path.push(&line[k..j]);
        i = k;
    }
    path.reverse();
    Some((path.into_iter().map(String::from).collect(), s..e))
}

/// Where things sit in the panel body, before scrolling.
struct Geo {
    /// Top of the first field row.
    rows_y: f32,
    /// Top of the first byte line.
    dump_y: f32,
    /// Bytes per dump line.
    per: usize,
    /// Left of the hex bytes and of the ASCII column (if it fits).
    hex_x: f32,
    ascii_x: Option<f32>,
    /// Field row columns: name, value, bytes (if they fit).
    name_x: f32,
    value_x: f32,
    bytes_x: Option<f32>,
    content_h: f32,
}

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Prev,
    Next,
    Row(usize),
    Byte(usize),
}

#[derive(Default)]
pub struct Memory {
    rect: RectF,
    body: RectF,
    line_h: f32,
    char_w: f32,
    snap: Option<Arc<Snapshot>>,
    target: Option<MemTarget>,
    regions: Vec<Region>,
    /// The region shown.
    pick: usize,
    /// Each shown address's bytes at the previous stop.
    previous: HashMap<u64, Vec<u8>>,
    scroll: f32,
    /// Row under the pointer (a field row, or the field of a byte).
    hover: Option<usize>,
    nav: Option<(RectF, RectF)>,
}

impl Memory {
    /// A new stop's data; the shown target is looked up again.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.previous = self
            .regions
            .iter()
            .filter_map(|r| Some((r.addr?, r.bytes.clone())))
            .collect();
        self.snap = Some(snap);
        self.refresh();
    }

    /// Shows `target` from the last stop.
    pub fn show(&mut self, target: MemTarget) {
        self.target = Some(target);
        self.pick = 0;
        self.scroll = 0.0;
        self.previous.clear();
        self.refresh();
    }

    fn refresh(&mut self) {
        let kept = self.region().and_then(|r| r.addr);
        self.regions = match (&self.snap, &self.target) {
            (Some(s), Some(t)) => resolve(s, t),
            _ => Vec::new(),
        };
        self.pick = kept
            .and_then(|a| self.regions.iter().position(|r| r.addr == Some(a)))
            .unwrap_or(self.pick.min(self.regions.len().saturating_sub(1)));
        self.hover = None;
        self.clamp_scroll();
    }

    fn region(&self) -> Option<&Region> {
        self.regions.get(self.pick)
    }

    fn place(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        self.body = rect.split_top(HEADER_H).1;
        self.line_h = (text.line_height(TextStyle::Mono) * 1.35).round();
        self.char_w = text.cell_size().w;
        self.nav = (self.regions.len() > 1).then(|| {
            let y = rect.y + (HEADER_H - NAV_W) / 2.0;
            let next = RectF::new(rect.right() - PAD - NAV_W, y, NAV_W, NAV_W);
            let prev = RectF::new(next.x - 64.0 - NAV_W, y, NAV_W, NAV_W);
            (prev, next)
        });
        self.clamp_scroll();
    }

    fn geo(&self) -> Option<Geo> {
        let r = self.region()?;
        let b = self.body;
        let (lh, cw) = (self.line_h, self.char_w);
        let rows_y = TITLE_H + LABEL_H;
        let dump_y = rows_y + r.rows.len() as f32 * lh + 12.0 + LABEL_H;
        // `+0x000` labels, then the bytes, then ASCII when it fits.
        let hex_x = b.x + PAD + 8.0 * cw;
        let room = b.right() - PAD - hex_x;
        let fits = |n: usize, ascii: bool| {
            let w = n as f32 * 3.0 * cw + if ascii { (n + 1) as f32 * cw } else { 0.0 };
            w <= room
        };
        let (per, ascii) = [(16, true), (8, true), (8, false), (4, true), (4, false)]
            .into_iter()
            .find(|&(n, a)| fits(n, a))
            .unwrap_or((4, false));
        let ascii_x = ascii.then_some(hex_x + (per as f32 * 3.0 + 1.0) * cw);
        let lines = r.bytes.len().div_ceil(per);
        let name_x = b.x + PAD + 7.0 * cw;
        let bytes_w = (ROW_BYTES as f32 * 3.0 + 1.0) * cw;
        let bytes_x = (b.w > 360.0).then(|| b.right() - PAD - bytes_w);
        let value_x = name_x + ((bytes_x.unwrap_or(b.right() - PAD) - name_x) * 0.45).max(60.0);
        Some(Geo {
            rows_y,
            dump_y,
            per,
            hex_x,
            ascii_x,
            name_x,
            value_x,
            bytes_x,
            content_h: dump_y + lines as f32 * lh + PAD,
        })
    }

    fn clamp_scroll(&mut self) {
        let h = self.geo().map_or(0.0, |g| g.content_h);
        self.scroll = self.scroll.clamp(0.0, (h - self.body.h).max(0.0));
    }

    fn hit(&self, p: PointF) -> Option<Hit> {
        if let Some((prev, next)) = self.nav {
            if prev.contains(p) {
                return Some(Hit::Prev);
            }
            if next.contains(p) {
                return Some(Hit::Next);
            }
        }
        if !self.body.contains(p) {
            return None;
        }
        let (r, g) = (self.region()?, self.geo()?);
        let y = p.y - self.body.y + self.scroll;
        let lh = self.line_h;
        if y >= g.rows_y && y < g.rows_y + r.rows.len() as f32 * lh {
            return Some(Hit::Row(((y - g.rows_y) / lh) as usize));
        }
        if y >= g.dump_y && p.x >= g.hex_x {
            let line = ((y - g.dump_y) / lh) as usize;
            let col = ((p.x - g.hex_x) / (3.0 * self.char_w)) as usize;
            let i = line * g.per + col;
            return (col < g.per && i < r.bytes.len()).then_some(Hit::Byte(i));
        }
        None
    }

    fn handle(&mut self, e: &Event) -> bool {
        match *e {
            Event::MouseMove { pos, .. } => {
                let hover = match self.hit(pos) {
                    Some(Hit::Row(i)) => Some(i),
                    Some(Hit::Byte(i)) => self.region().and_then(|r| r.owners()[i]),
                    _ => None,
                };
                std::mem::replace(&mut self.hover, hover) != hover
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => {
                let n = self.regions.len();
                match self.hit(pos) {
                    Some(Hit::Prev) if n > 0 => self.pick = (self.pick + n - 1) % n,
                    Some(Hit::Next) if n > 0 => self.pick = (self.pick + 1) % n,
                    Some(Hit::Row(i)) => self.regions[self.pick].focus = Some(i),
                    Some(Hit::Byte(i)) => {
                        let owner = self.region().and_then(|r| r.owners()[i]);
                        self.regions[self.pick].focus = owner;
                    }
                    _ => return false,
                }
                self.clamp_scroll();
                true
            }
            Event::Wheel { lines, .. } => {
                let before = self.scroll;
                self.scroll -= lines * 3.0 * self.line_h;
                self.clamp_scroll();
                self.scroll != before
            }
            Event::MouseLeave => self.hover.take().is_some(),
            _ => false,
        }
    }

    fn draw(&self, d: &mut DrawCtx, theme: &Theme) {
        let r = self.rect;
        d.fill_rect(r, theme.panel_bg);
        let header = RectF::new(r.x + PAD, r.y, r.w - 2.0 * PAD, HEADER_H);
        d.text(TextStyle::UiSmall, "MEMORY", header, theme.text_dim);
        if let Some((prev, next)) = self.nav {
            let count = format!("{} of {}", self.pick + 1, self.regions.len());
            let at = RectF::new(prev.right(), r.y, next.x - prev.right(), HEADER_H);
            let w = d.text_system().measure(TextStyle::UiSmall, &count);
            let at = RectF::new(at.x + (at.w - w) / 2.0, at.y, w + 2.0, at.h);
            d.text(TextStyle::UiSmall, &count, at, theme.text);
            for (b, glyph) in [(prev, "\u{2039}"), (next, "\u{203A}")] {
                d.fill_round_rect(b, 4.0, theme.hover_bg);
                let w = d.text_system().measure(TextStyle::Ui, glyph);
                let at = RectF::new(b.x + (b.w - w) / 2.0, b.y, w + 2.0, b.h);
                d.text(TextStyle::Ui, glyph, at, theme.text_bright);
            }
        }
        let body = self.body;
        let msg_at = RectF::new(body.x + PAD, body.y + 6.0, body.w - 2.0 * PAD, 22.0);
        let (Some(region), Some(g)) = (self.region(), self.geo()) else {
            let msg = match (&self.snap, &self.target) {
                (None, _) => "Run the program once (Visualizer \u{2192} Data \u{2192} Run) to see its memory.".to_string(),
                (Some(_), None) => "Hover a variable in the Data view, call graph or source and choose View in memory.".to_string(),
                (Some(_), Some(t)) => format!("{} is not live at this stop.", target_label(t)),
            };
            d.text(TextStyle::Ui, &msg, msg_at, theme.text_dim);
            return;
        };
        d.clip(body, |d| self.draw_region(d, theme, region, &g));
    }

    fn draw_region(&self, d: &mut DrawCtx, theme: &Theme, r: &Region, g: &Geo) {
        let b = self.body;
        let (lh, cw) = (self.line_h, self.char_w);
        let top = b.y - self.scroll;
        let left = b.x + PAD;
        let w = b.w - 2.0 * PAD;
        let text_dy = ((lh - d.text_system().line_height(TextStyle::Mono)) / 2.0).round();

        // Title: what it is, then type, size and address.
        d.text(
            TextStyle::UiBold,
            &r.title,
            RectF::new(left, top + 6.0, w, 20.0),
            theme.text_bright,
        );
        let mut info = format!(
            "{} \u{b7} {} byte{}",
            r.ty,
            r.size,
            if r.size == 1 { "" } else { "s" }
        );
        if let Some(a) = r.addr {
            info.push_str(&format!(" \u{b7} {a:#x}"));
        }
        if let Some(s) = &r.summary {
            info.push_str(&format!(" \u{b7} {s}"));
        }
        d.text(
            TextStyle::UiSmall,
            &info,
            RectF::new(left, top + 26.0, w, 18.0),
            theme.text_dim,
        );

        let owners = r.owners();
        let previous = r.addr.and_then(|a| self.previous.get(&a));
        let changed =
            |i: usize| previous.is_some_and(|p| p.get(i).is_some_and(|&x| x != r.bytes[i]));
        // Fields alternate two tints so neighbours stay apart in the dump.
        let tint = |row: usize| {
            if r.focus == Some(row) {
                Some(theme.selection_bg)
            } else if self.hover == Some(row) {
                Some(theme.hover_bg)
            } else if r.rows[row].padding {
                None
            } else if row.is_multiple_of(2) {
                Some(theme.data_box_bg)
            } else {
                Some(theme.data_header_bg)
            }
        };

        // Field rows.
        let label = |d: &mut DrawCtx, y: f32, s: &str| {
            d.text(
                TextStyle::UiSmall,
                s,
                RectF::new(left, y, w, LABEL_H),
                theme.text_dim,
            );
        };
        if r.rows.is_empty() {
            label(d, top + TITLE_H, "LAYOUT UNKNOWN (CONTAINER)");
        } else {
            label(d, top + TITLE_H, "OFFSET  FIELD");
        }
        for (i, row) in r.rows.iter().enumerate() {
            let y = top + g.rows_y + i as f32 * lh;
            if y + lh < b.y || y > b.bottom() {
                continue;
            }
            let rr = RectF::new(b.x, y, b.w, lh);
            if r.focus == Some(i) {
                d.fill_rect(rr, theme.selection_bg);
                d.fill_rect(RectF::new(rr.x, rr.y, 2.0, rr.h), theme.accent);
            } else if self.hover == Some(i) {
                d.fill_rect(rr, theme.hover_bg);
            }
            if let Some(c) = tint(i).filter(|_| r.focus != Some(i) && self.hover != Some(i)) {
                d.fill_rect(RectF::new(left - 6.0, y + 3.0, 3.0, lh - 6.0), c);
            }
            let ty = y + text_dy;
            let dim = if row.padding {
                theme.text_dim.with_alpha(0.6)
            } else {
                theme.text_dim
            };
            d.text_at(
                TextStyle::Mono,
                &format!("+{:#x}", row.off),
                PointF::new(left, ty),
                dim,
            );
            let name = if row.padding {
                format!(
                    "padding ({} byte{})",
                    row.size,
                    if row.size == 1 { "" } else { "s" }
                )
            } else {
                row.name.clone()
            };
            let name_c = if row.padding { dim } else { theme.text };
            d.text(
                TextStyle::Ui,
                &name,
                RectF::new(g.name_x, y, g.value_x - g.name_x - 6.0, lh),
                name_c,
            );
            let value_r = g.bytes_x.unwrap_or(b.right() - PAD) - g.value_x - 6.0;
            d.text(
                TextStyle::Ui,
                &row.value,
                RectF::new(g.value_x, y, value_r, lh),
                theme.text_bright,
            );
            if let Some(x) = g.bytes_x {
                let end = (row.off + row.size).min(r.bytes.len() as u64);
                let shown = &r.bytes[(row.off.min(end) as usize)..end as usize];
                let mut hex: Vec<String> = shown
                    .iter()
                    .take(ROW_BYTES)
                    .map(|x| format!("{x:02x}"))
                    .collect();
                if shown.len() > ROW_BYTES {
                    hex.push("\u{2026}".into());
                }
                d.text_at(TextStyle::Mono, &hex.join(" "), PointF::new(x, ty), dim);
            }
        }

        // The bytes, tinted by field.
        let label_y = top + g.dump_y - LABEL_H;
        label(d, label_y, "BYTES");
        if (r.bytes.len() as u64) < r.size {
            let note = if r.bytes.is_empty() {
                "unreadable".to_string()
            } else {
                format!("first {} of {}", r.bytes.len(), r.size)
            };
            let nw = d.text_system().measure(TextStyle::UiSmall, &note);
            let at = RectF::new(b.right() - PAD - nw, label_y, nw + 2.0, LABEL_H);
            d.text(TextStyle::UiSmall, &note, at, theme.text_dim);
        }
        for (line, chunk) in r.bytes.chunks(g.per).enumerate() {
            let y = top + g.dump_y + line as f32 * lh;
            if y + lh < b.y || y > b.bottom() {
                continue;
            }
            let ty = y + text_dy;
            let base = line * g.per;
            d.text_at(
                TextStyle::Mono,
                &format!("+{base:#05x}"),
                PointF::new(left, ty),
                theme.gutter_text,
            );
            for (k, &byte) in chunk.iter().enumerate() {
                let i = base + k;
                let x = g.hex_x + k as f32 * 3.0 * cw;
                let owner = owners[i];
                if let Some(c) = owner.and_then(tint) {
                    // Bytes of one field join into one band.
                    let joined = k + 1 < chunk.len() && owners[i + 1] == owner;
                    let bw = if joined { 3.0 * cw } else { 2.0 * cw + 2.0 };
                    d.fill_rect(RectF::new(x - 1.0, y + 2.0, bw, lh - 4.0), c);
                }
                let color = if changed(i) {
                    theme.accent
                } else if owner.is_none_or(|o| r.rows[o].padding) {
                    theme.text_dim.with_alpha(0.6)
                } else {
                    theme.text
                };
                d.text_at(
                    TextStyle::Mono,
                    &format!("{byte:02x}"),
                    PointF::new(x, ty),
                    color,
                );
            }
            if let Some(ax) = g.ascii_x {
                let ascii: String = chunk
                    .iter()
                    .map(|&c| {
                        if c.is_ascii_graphic() || c == b' ' {
                            c as char
                        } else {
                            '.'
                        }
                    })
                    .collect();
                d.text_at(TextStyle::Mono, &ascii, PointF::new(ax, ty), theme.text_dim);
            }
        }
    }
}

fn target_label(t: &MemTarget) -> String {
    match t {
        MemTarget::Slot { name, .. } | MemTarget::Global { name } => name.clone(),
        MemTarget::Expr { path, .. } => path.join("."),
        MemTarget::Field { class, name } => format!("No {class} (for {name})"),
        MemTarget::Local { func, name } => format!("{name} in {func}"),
    }
}

/// The hover card's text for `target`: `Ok` when it can open.
pub fn card_label(snap: Option<&Snapshot>, target: &MemTarget) -> Result<&'static str, String> {
    let Some(snap) = snap else {
        return Err("Run the program once to view memory".into());
    };
    if resolve(snap, target).is_empty() {
        return Err(format!("{} is not live at this stop", target_label(target)));
    }
    Ok("View in memory")
}

impl Panel for Memory {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.place(rect, text);
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        self.draw(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if self.handle(e) {
            cx.repaint();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// main: `root` (Node*), `p` (Point by value), `n`; insert(this-less)
    /// recursion twice; one global struct.
    fn snap() -> Snapshot {
        Snapshot::parse(
            r#"{"frames":[
                {"level":2,"func":"main","file":"C:\\src\\t.cpp","line":20,"vars":[
                    {"name":"root","type":"Node *","value":{"k":"p","to":"a0:Node","v":"0xa0"},
                     "addr":"0x10","size":8,"bytes":"a000000000000000"},
                    {"name":"p","type":"Point","value":{"k":"o","to":"20:Point"},"addr":"0x20","size":8},
                    {"name":"n","type":"int","value":{"k":"s","v":"3"},"addr":"0x30","size":4,"bytes":"03000000"}]},
                {"level":1,"func":"insert(Node*, int)","file":"C:/src/t.cpp","line":9,"vars":[
                    {"name":"k","arg":true,"type":"int","value":{"k":"s","v":"5"},"addr":"0x40","size":4,"bytes":"05000000"}]},
                {"level":0,"func":"Tree::insert(Node*, int)","file":"c:/SRC/t.cpp","line":8,"vars":[
                    {"name":"this","arg":true,"type":"Tree *","value":{"k":"p","to":"b0:Tree","v":"0xb0"}},
                    {"name":"k","arg":true,"type":"int","value":{"k":"s","v":"7"},"addr":"0x50","size":4,"bytes":"07000000"}]}],
              "objects":{
                "a0:Node":{"addr":"0xa0","type":"Node","size":24,"kind":"struct",
                  "bytes":"32000000aaaaaaaa000000000000000000000000000000c0",
                  "fields":[{"name":"key","type":"int","off":0,"size":4,"value":{"k":"s","v":"50"}},
                            {"name":"left","type":"Node *","off":8,"size":8,"value":{"k":"p","to":null,"v":"0x0"}},
                            {"name":"right","type":"Node *","off":16,"size":8,"value":{"k":"p","to":"c0:Node","v":"0xc0"}}]},
                "c0:Node":{"addr":"0xc0","type":"Node","size":24,"kind":"struct","bytes":"46",
                  "fields":[{"name":"key","type":"int","off":0,"size":4,"value":{"k":"s","v":"70"}}]},
                "20:Point":{"addr":"0x20","type":"Point","size":8,"kind":"struct","bytes":"0100000002000000",
                  "fields":[{"name":"x","type":"int","off":0,"size":4,"value":{"k":"s","v":"1"}},
                            {"name":"y","type":"int","off":4,"size":4,"value":{"k":"s","v":"2"}}]},
                "b0:Tree":{"addr":"0xb0","type":"Tree","size":8,"kind":"struct",
                  "fields":[{"name":"root","type":"Node *","off":0,"size":8,"value":{"k":"p","to":"a0:Node","v":"0xa0"}}]},
                "f0:std::vector<Point>":{"addr":"0xf0","type":"std::vector<Point>","size":24,"kind":"array",
                  "fields":[{"name":"0","type":"","value":{"k":"o","to":"20:Point"}}],"summary":"std::vector of length 1"}},
              "globals":[{"name":"origin","type":"Point","value":{"k":"s","v":"{x = 3, y = 4}"},"addr":"0x400","size":8,
                "bytes":"0300000004000000",
                "fields":[{"name":"x","type":"int","off":0,"size":4,"value":{"k":"s","v":"3"}},
                          {"name":"y","type":"int","off":4,"size":4,"value":{"k":"s","v":"4"}}]}]}"#,
        )
        .unwrap()
    }

    fn one(t: MemTarget) -> Region {
        let mut all = resolve(&snap(), &t);
        assert_eq!(all.len(), 1, "{t:?}");
        all.remove(0)
    }

    fn names(r: &Region) -> Vec<&str> {
        r.rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn struct_rows_fill_padding() {
        let r = one(MemTarget::Slot {
            key: "a0:Node".into(),
            name: "left".into(),
        });
        assert_eq!(names(&r), ["key", "padding", "left", "right"]);
        assert_eq!((r.rows[1].off, r.rows[1].size), (4, 4));
        assert_eq!(r.focus, Some(2));
        assert_eq!(r.addr, Some(0xa0));
        let owners = r.owners();
        assert_eq!(
            (owners[0], owners[5], owners[8], owners[23]),
            (Some(0), Some(1), Some(2), Some(3))
        );
    }

    #[test]
    fn frame_slots_show_the_variable() {
        let ptr = one(MemTarget::Slot {
            key: "frame:0".into(),
            name: "root".into(),
        });
        // A pointer variable: its own 8 bytes.
        assert_eq!((ptr.addr, ptr.size, ptr.bytes.len()), (Some(0x10), 8, 8));
        assert_eq!(names(&ptr), ["root"]);
        // A by-value struct: the object, laid out.
        let p = one(MemTarget::Slot {
            key: "frame:0".into(),
            name: "p".into(),
        });
        assert_eq!((p.title.as_str(), names(&p)), ("p", vec!["x", "y"]));
    }

    #[test]
    fn container_elements_open_the_element() {
        let r = one(MemTarget::Slot {
            key: "f0:std::vector<Point>".into(),
            name: "0".into(),
        });
        assert_eq!((r.title.as_str(), r.addr), ("[0]", Some(0x20)));
    }

    #[test]
    fn expressions_walk_pointers_in_the_files_frame() {
        let file = PathBuf::from(r"C:\src\t.cpp");
        let ex = |p: &[&str]| MemTarget::Expr {
            file: file.clone(),
            path: p.iter().map(|s| s.to_string()).collect(),
        };
        // `k` is read in the innermost frame of that file.
        assert_eq!(one(ex(&["k"])).bytes, [7, 0, 0, 0]);
        let r = one(ex(&["this", "root", "right", "key"]));
        assert_eq!(
            (r.title.as_str(), r.addr),
            ("*this->root->right", Some(0xc0))
        );
        assert_eq!(r.focus, Some(0));
        // A bare member reads through `this`.
        assert_eq!(one(ex(&["root"])).focus, Some(0));
        // Globals, and one level into them.
        assert_eq!(one(ex(&["origin", "y"])).focus, Some(1));
        assert!(resolve(&snap(), &ex(&["nope"])).is_empty());
        assert!(resolve(&snap(), &ex(&["this", "nope"])).is_empty());
        assert!(resolve(&snap(), &ex(&["origin", "z"])).is_empty());
    }

    #[test]
    fn graph_targets_find_every_instance() {
        let s = snap();
        let nodes = resolve(
            &s,
            &MemTarget::Field {
                class: "ns::Node".into(),
                name: "key".into(),
            },
        );
        let addrs: Vec<_> = nodes.iter().map(|r| r.addr).collect();
        assert_eq!(addrs, [Some(0xa0), Some(0xc0)]);
        assert!(nodes.iter().all(|r| r.focus == Some(0)));
        // A by-value global of the class counts too.
        let points = resolve(
            &s,
            &MemTarget::Field {
                class: "Point".into(),
                name: "y".into(),
            },
        );
        assert_eq!(points.len(), 2);
        // Both recursive frames of `insert`, innermost first.
        let ks = resolve(
            &s,
            &MemTarget::Local {
                func: "insert".into(),
                name: "k".into(),
            },
        );
        let bytes: Vec<u8> = ks.iter().map(|r| r.bytes[0]).collect();
        assert_eq!(bytes, [7, 5]);
        let g = resolve(
            &s,
            &MemTarget::Global {
                name: "origin".into(),
            },
        );
        assert_eq!(g[0].addr, Some(0x400));
    }

    #[test]
    fn card_needs_a_run_and_a_live_target() {
        let t = MemTarget::Global {
            name: "origin".into(),
        };
        assert!(card_label(None, &t).is_err());
        assert_eq!(card_label(Some(&snap()), &t), Ok("View in memory"));
        let gone = MemTarget::Global {
            name: "gone".into(),
        };
        assert!(card_label(Some(&snap()), &gone).is_err());
    }

    #[test]
    fn expression_under_the_pointer() {
        let line = "    total += p->next->key + a.b[2].c + 1.5f;";
        let at = |needle: &str| line.find(needle).unwrap();
        let (path, span) = expr_at(line, at("next")).unwrap();
        assert_eq!(
            (path, span),
            (
                vec!["p".to_string(), "next".into()],
                at("next")..at("next") + 4
            )
        );
        assert_eq!(
            expr_at(line, at("key") + 1).unwrap().0,
            ["p", "next", "key"]
        );
        assert_eq!(expr_at(line, at("total")).unwrap().0, ["total"]);
        assert_eq!(expr_at(line, at("b[")).unwrap().0, ["a", "b"]);
        assert_eq!(expr_at(line, at("c +")), None, "after an index");
        assert_eq!(expr_at(line, at("5f")), None, "a number");
        assert_eq!(expr_at(line, at("+=")), None);
        assert_eq!(expr_at(line, line.len()), None);
    }

    #[test]
    fn following_a_target_across_stops() {
        let mut m = Memory::default();
        assert!(m.region().is_none());
        m.show(MemTarget::Field {
            class: "Node".into(),
            name: "key".into(),
        });
        let s = Arc::new(snap());
        m.set_snapshot(s.clone());
        m.pick = 1;
        // The next stop keeps the picked instance and remembers its bytes.
        m.set_snapshot(s);
        assert_eq!(m.region().and_then(|r| r.addr), Some(0xc0));
        assert_eq!(m.previous.get(&0xc0), Some(&vec![0x46]));
    }
}
