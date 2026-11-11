//! The Variables panel: the selected frame's arguments and locals, then the
//! file-level variables, as a tree of name, value and type. Clicking a row
//! with children opens it; opening follows pointers, so cycles just stop where
//! the user stops clicking. Right-click for View in memory and Copy value.
//! Values that changed since the last stop are tinted.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use debugger::{Cell, Snapshot};

use crate::event::{Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::memory::{self, MemTarget};
use crate::menubar::Entry;
use crate::panel::{Action, Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 22.0;
const PAD: f32 = 10.0;
const INDENT: f32 = 14.0;
/// Text alpha while the program runs and the values are from the last stop.
const STALE: f32 = 0.55;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    /// Identifies the row across stops: `func/name/field`.
    path: String,
    depth: usize,
    name: String,
    value: String,
    ty: String,
    /// `Some(open)` when the row has children.
    open: Option<bool>,
    /// A section title.
    header: bool,
    changed: bool,
    memory: Option<MemTarget>,
}

impl Row {
    fn note(path: String, depth: usize, name: String, header: bool) -> Self {
        Self {
            path,
            depth,
            name,
            value: String::new(),
            ty: String::new(),
            open: None,
            header,
            changed: false,
            memory: None,
        }
    }
}

/// Walks a stop into rows.
struct Tree<'a> {
    snap: &'a Snapshot,
    expanded: &'a HashSet<String>,
    previous: &'a HashMap<String, String>,
    rows: Vec<Row>,
}

impl Tree<'_> {
    /// One slot, and its children if the user opened it. `extra` are fields
    /// listed on a global rather than reached through an object.
    fn slot(
        &mut self,
        (path, depth): (String, usize),
        (name, ty): (&str, &str),
        cell: &Cell,
        extra: &[debugger::Field],
        memory: Option<MemTarget>,
    ) {
        let key = cell.target();
        let object = key.and_then(|k| self.snap.objects.get(k));
        let value = match cell {
            Cell::Scalar { v } => v.clone(),
            Cell::Pointer { v, bad: true, .. } => format!("{v} (unreadable)"),
            Cell::Pointer { v, cut: true, .. } => format!("{v} (not followed)"),
            Cell::Pointer { v, .. } => v.clone(),
            Cell::Embedded { .. } => object
                .and_then(|o| o.summary.clone())
                .unwrap_or_else(|| "{\u{2026}}".to_string()),
        };
        let fields = object.map_or(extra, |o| &o.fields);
        let has_children = !fields.is_empty();
        let open = has_children && self.expanded.contains(&path);
        let changed = self.previous.get(&path).is_some_and(|p| *p != value);
        self.rows.push(Row {
            path: path.clone(),
            depth,
            name: name.to_string(),
            value,
            ty: ty.to_string(),
            open: has_children.then_some(open),
            header: false,
            changed,
            memory: memory.clone(),
        });
        if !open {
            return;
        }
        for f in fields {
            let child = match (key, &memory) {
                (Some(k), _) if object.is_some() => Some(MemTarget::Slot {
                    key: k.to_string(),
                    name: f.name.clone(),
                }),
                // A global's own field: one level into its expression.
                (_, Some(MemTarget::Expr { file, path })) => Some(MemTarget::Expr {
                    file: file.clone(),
                    path: [path.as_slice(), std::slice::from_ref(&f.name)].concat(),
                }),
                _ => None,
            };
            self.slot(
                (format!("{path}/{}", f.name), depth + 1),
                (&f.name, &f.ty),
                &f.value,
                &[],
                child,
            );
        }
        if let Some(more) = object.map(|o| o.more).filter(|&m| m != 0) {
            let text = if more > 0 {
                format!("\u{2026} {more} more")
            } else {
                "\u{2026} more".to_string()
            };
            self.rows.push(Row::note(
                format!("{path}/\u{2026}"),
                depth + 1,
                text,
                false,
            ));
        }
    }
}

/// The rows of `frame` (`None` is the innermost) in `snap`.
fn build(
    snap: &Snapshot,
    frame: Option<usize>,
    expanded: &HashSet<String>,
    previous: &HashMap<String, String>,
) -> Vec<Row> {
    let mut t = Tree {
        snap,
        expanded,
        previous,
        rows: Vec::new(),
    };
    let Some(innermost) = snap.frames.len().checked_sub(1) else {
        return t.rows;
    };
    let i = frame
        .filter(|&i| i < snap.frames.len())
        .unwrap_or(innermost);
    let f = &snap.frames[i];
    // Arguments first, as they are declared.
    for v in f
        .vars
        .iter()
        .filter(|v| v.arg)
        .chain(f.vars.iter().filter(|v| !v.arg))
    {
        t.slot(
            (format!("{}/{}", f.func, v.name), 0),
            (&v.name, &v.ty),
            &v.value,
            &v.fields,
            Some(MemTarget::Slot {
                key: format!("frame:{i}"),
                name: v.name.clone(),
            }),
        );
    }
    if !snap.globals.is_empty() {
        t.rows
            .push(Row::note("globals".into(), 0, "Globals".into(), true));
        for g in &snap.globals {
            t.slot(
                (format!("::/{}", g.name), 0),
                (&g.name, &g.ty),
                &g.value,
                &g.fields,
                Some(MemTarget::Expr {
                    file: PathBuf::from(&f.file),
                    path: vec![g.name.clone()],
                }),
            );
        }
    }
    t.rows
}

#[derive(Default)]
pub struct Variables {
    rect: RectF,
    list: RectF,
    snap: Option<Arc<Snapshot>>,
    frame: Option<usize>,
    /// Paths of the opened rows; kept from stop to stop.
    expanded: HashSet<String>,
    /// Each row's value at the stop before this one.
    previous: HashMap<String, String>,
    rows: Vec<Row>,
    scroll: f32,
    hover: Option<usize>,
    stale: bool,
}

impl Variables {
    /// A new stop.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.previous = self
            .rows
            .iter()
            .map(|r| (r.path.clone(), r.value.clone()))
            .collect();
        self.snap = Some(snap);
        self.frame = None;
        self.rebuild();
    }

    pub fn clear(&mut self) {
        *self = Self {
            rect: self.rect,
            list: self.list,
            expanded: std::mem::take(&mut self.expanded),
            ..Self::default()
        };
    }

    /// Which frame to show, and whether to dim (set before each paint).
    pub fn set_view(&mut self, frame: Option<usize>, stale: bool) {
        self.stale = stale;
        if self.frame != frame {
            self.frame = frame;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.rows = match &self.snap {
            Some(s) => build(s, self.frame, &self.expanded, &self.previous),
            None => Vec::new(),
        };
        self.hover = None;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = (self.rows.len() as f32 * ROW_H - self.list.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn row_at(&self, p: PointF) -> Option<usize> {
        if !self.list.contains(p) {
            return None;
        }
        let i = ((p.y - self.list.y + self.scroll) / ROW_H) as usize;
        (i < self.rows.len()).then_some(i)
    }

    /// Opens or closes a row. `false` if it has no children.
    fn toggle(&mut self, i: usize) -> bool {
        let Some(row) = self.rows.get(i).filter(|r| r.open.is_some()) else {
            return false;
        };
        let path = row.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild();
        true
    }

    fn menu(&self, i: usize) -> Option<Vec<Entry<Action>>> {
        let row = self
            .rows
            .get(i)
            .filter(|r| !r.header && !r.value.is_empty())?;
        let live = row
            .memory
            .as_ref()
            .filter(|t| memory::card_label(self.snap.as_deref(), t).is_ok());
        let mut items = vec![Entry::item(
            Action::CopyText(row.value.clone()),
            "Copy value",
            "",
            true,
        )];
        // A target that is not live has no memory to show.
        if let Some(t) = live {
            items.insert(
                0,
                Entry::item(Action::ShowMemory(t.clone()), "View in memory", "", true),
            );
        }
        Some(items)
    }

    fn draw(&self, d: &mut DrawCtx, theme: &Theme) {
        let r = self.rect;
        d.fill_rect(r, theme.panel_bg);
        let a = if self.stale { STALE } else { 1.0 };
        let header = RectF::new(r.x + PAD, r.y, r.w - 2.0 * PAD, HEADER_H);
        d.text(TextStyle::UiSmall, "VARIABLES", header, theme.text_dim);
        let list = self.list;
        d.clip(list, |d| {
            if self.rows.is_empty() {
                let at = RectF::new(list.x + PAD, list.y + 4.0, list.w - 2.0 * PAD, 22.0);
                d.text(TextStyle::Ui, "No variables", at, theme.text_dim);
                return;
            }
            // Name, then value, then type.
            let value_x = list.x + list.w * 0.4;
            let type_x = list.x + list.w * 0.72;
            for (i, row) in self.rows.iter().enumerate() {
                let y = list.y + i as f32 * ROW_H - self.scroll;
                if y + ROW_H < list.y || y > list.bottom() {
                    continue;
                }
                let rr = RectF::new(list.x, y, list.w, ROW_H);
                if self.hover == Some(i) && !row.header {
                    d.fill_rect(rr, theme.hover_bg);
                }
                if row.header {
                    let at = RectF::new(list.x + PAD, y, list.w - 2.0 * PAD, ROW_H);
                    d.text(TextStyle::UiSmall, &row.name, at, theme.text_dim);
                    continue;
                }
                let x = list.x + PAD + row.depth as f32 * INDENT;
                if let Some(open) = row.open {
                    let glyph = if open { "\u{25be}" } else { "\u{25b8}" };
                    let at = RectF::new(x, y, INDENT, ROW_H);
                    d.text(TextStyle::Ui, glyph, at, theme.text_dim.with_alpha(a));
                }
                let name_x = x + INDENT;
                let at = RectF::new(name_x, y, (value_x - name_x - 6.0).max(0.0), ROW_H);
                d.text(TextStyle::Ui, &row.name, at, theme.text.with_alpha(a));
                let color = if row.changed {
                    theme.data_changed
                } else {
                    theme.text_bright
                };
                let at = RectF::new(value_x, y, type_x - value_x - 6.0, ROW_H);
                d.text(TextStyle::Ui, &row.value, at, color.with_alpha(a));
                let at = RectF::new(type_x, y, list.right() - type_x - PAD, ROW_H);
                d.text(
                    TextStyle::UiSmall,
                    &row.ty,
                    at,
                    theme.text_dim.with_alpha(a),
                );
            }
        });
    }
}

impl Panel for Variables {
    fn layout(&mut self, rect: RectF, _text: &TextSystem) {
        self.rect = rect;
        self.list = rect.split_top(HEADER_H).1;
        self.clamp_scroll();
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        self.draw(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => {
                if self.row_at(pos).is_some_and(|i| self.toggle(i)) {
                    cx.repaint();
                }
            }
            Event::MouseUp {
                pos,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(items) = self.row_at(pos).and_then(|i| self.menu(i)) {
                    cx.act(Action::ContextMenu { pos, items });
                }
            }
            Event::MouseMove { pos, .. } => {
                let hover = self.row_at(pos);
                if hover != self.hover {
                    self.hover = hover;
                    cx.repaint();
                }
            }
            Event::MouseLeave => {
                if self.hover.take().is_some() {
                    cx.repaint();
                }
            }
            Event::Wheel { lines, .. } => {
                self.scroll -= lines * 3.0 * ROW_H;
                self.clamp_scroll();
                cx.repaint();
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(n: &str) -> Snapshot {
        Snapshot::parse(&format!(
            r#"{{"frames":[
                {{"level":1,"func":"main","file":"C:\\src\\t.cpp","line":9,"vars":[
                    {{"name":"root","type":"Node *","value":{{"k":"p","to":"a","v":"0xa0"}}}}]}},
                {{"level":0,"func":"insert","file":"C:\\src\\t.cpp","line":3,"vars":[
                    {{"name":"local","type":"int","value":{{"k":"s","v":"{n}"}}}},
                    {{"name":"key","arg":true,"type":"int","value":{{"k":"s","v":"7"}}}}]}}],
              "objects":{{"a":{{"addr":"0xa0","type":"Node","kind":"struct","more":2,"fields":[
                    {{"name":"key","type":"int","value":{{"k":"s","v":"5"}}}},
                    {{"name":"next","type":"Node *","value":{{"k":"p","to":"a","v":"0xa0"}}}}]}}}},
              "globals":[{{"name":"count","type":"int","value":{{"k":"s","v":"1"}}}}]}}"#
        ))
        .unwrap()
    }

    fn panel() -> Variables {
        let mut v = Variables::default();
        v.set_snapshot(Arc::new(snap("1")));
        v
    }

    fn names(v: &Variables) -> Vec<&str> {
        v.rows.iter().map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn shows_the_innermost_frame_arguments_first_then_globals() {
        assert_eq!(names(&panel()), ["key", "local", "Globals", "count"]);
    }

    #[test]
    fn selecting_a_frame_shows_its_variables() {
        let mut v = panel();
        v.set_view(Some(0), false);
        assert_eq!(names(&v), ["root", "Globals", "count"]);
        assert_eq!(v.rows[0].open, Some(false));
    }

    #[test]
    fn opening_follows_pointers_even_round_a_cycle() {
        let mut v = panel();
        v.set_view(Some(0), false);
        assert!(v.toggle(0));
        assert_eq!(
            names(&v),
            ["root", "key", "next", "\u{2026} 2 more", "Globals", "count"]
        );
        assert!(v.toggle(2), "next points back at the same node");
        assert_eq!(v.rows[3].name, "key");
        assert_eq!(v.rows[3].depth, 2);
        // Leaves, notes and titles don't toggle.
        assert!(!v.toggle(1));
        assert!(!v.toggle(v.rows.len() - 2));
        // Closing the outer row hides everything under it.
        assert!(v.toggle(0));
        assert_eq!(names(&v), ["root", "Globals", "count"]);
    }

    #[test]
    fn only_values_that_changed_are_marked_and_rows_stay_open() {
        let mut v = panel();
        v.set_view(Some(0), false);
        v.toggle(0);
        v.set_view(None, false);
        v.set_snapshot(Arc::new(snap("2")));
        let local = v.rows.iter().find(|r| r.name == "local").unwrap();
        assert!(local.changed);
        assert!(!v.rows.iter().find(|r| r.name == "key").unwrap().changed);
        v.set_view(Some(0), false);
        assert_eq!(v.rows[1].name, "key", "still open at the next stop");
    }

    #[test]
    fn right_click_offers_memory_only_when_it_is_live() {
        let v = panel();
        let labels = |i| -> Vec<String> {
            v.menu(i)
                .unwrap()
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Item { label, .. } => Some(label),
                    _ => None,
                })
                .collect()
        };
        // `key` in the innermost frame, and a global through its expression.
        assert_eq!(labels(0), ["View in memory", "Copy value"]);
        assert_eq!(labels(3), ["View in memory", "Copy value"]);
        assert!(v.menu(2).is_none(), "section title");
    }

    #[test]
    fn empty_before_any_stop() {
        let mut v = panel();
        v.clear();
        assert!(v.rows.is_empty());
        assert!(v.menu(0).is_none());
    }
}
