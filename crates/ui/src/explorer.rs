//! Solution Explorer: a lazily loaded folder tree. Object files (`.obj`,
//! `.o`) expand into their symbols, loaded by the engine in the background.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use engine::{CodeGraph, EdgeKind, NodeId, NodeKind};

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::objects::is_object_file;
use crate::panel::{Action, Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const HEADER_H: f32 = 34.0;
const ROW_H: f32 = 22.0;
const INDENT: f32 = 12.0;
const LEFT_PAD: f32 = 8.0;
/// Directory names that are never listed.
const HIDDEN: &[&str] = &[".git"];
/// Symbol groups under a loaded object, in display order.
const GROUPS: &[(&str, NodeKind)] = &[
    ("Functions", NodeKind::Function),
    ("Variables", NodeKind::Variable),
    ("Externals", NodeKind::External),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectState {
    Unloaded,
    Loading,
    Loaded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Object(ObjectState),
    /// "Functions (n)" etc. under a loaded object.
    Group,
    Symbol(NodeKind),
}

impl EntryKind {
    fn expandable(self) -> bool {
        matches!(
            self,
            EntryKind::Dir | EntryKind::Object(_) | EntryKind::Group
        )
    }
}

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    path: PathBuf,
    kind: EntryKind,
    depth: usize,
    expanded: bool,
    /// `None` until first expanded (or loaded, for objects).
    children: Option<Vec<usize>>,
    /// Dimmed text after the name: `file:line`, a count, or an error.
    detail: Option<String>,
}

impl Entry {
    fn new(name: String, path: PathBuf, kind: EntryKind, depth: usize) -> Self {
        Self {
            name,
            path,
            kind,
            depth,
            expanded: false,
            children: None,
            detail: None,
        }
    }
}

#[derive(Default)]
pub struct Explorer {
    root: Option<usize>,
    entries: Vec<Entry>,
    /// Visible rows, as indices into `entries` (root excluded).
    rows: Vec<usize>,
    selected: Option<usize>,
    hover_row: Option<usize>,
    scroll: f32,
    rect: RectF,
    header: RectF,
    list: RectF,
    button: RectF,
    button_hover: bool,
}

/// Reads one directory level: directories first, then files, each sorted
/// case-insensitively. Hidden names are skipped.
fn read_level(dir: &Path) -> std::io::Result<Vec<(String, PathBuf, bool)>> {
    let mut items = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if HIDDEN.contains(&name.as_str()) {
            continue;
        }
        let path = entry.path();
        let is_dir = std::fs::metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        items.push((name, path, is_dir));
    }
    items.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    Ok(items)
}

/// `graph.c:21` from a source location (file name only, to keep rows short).
fn short_location(file: &str, line: u32) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    format!("{name}:{line}")
}

/// Group label without its count: "Functions (4)" -> "Functions".
fn group_key(name: &str) -> &str {
    name.split(" (").next().unwrap_or(name)
}

impl Explorer {
    pub fn root_path(&self) -> Option<&Path> {
        self.root.map(|i| self.entries[i].path.as_path())
    }

    pub fn set_root(&mut self, path: &Path) -> Result<(), String> {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut root = Entry::new(name, path.to_path_buf(), EntryKind::Dir, 0);
        root.expanded = true;
        self.entries = vec![root];
        self.root = Some(0);
        self.selected = None;
        self.hover_row = None;
        self.scroll = 0.0;
        let result = self.load_children(0);
        self.rebuild_rows();
        result
    }

    fn load_children(&mut self, idx: usize) -> Result<(), String> {
        if self.entries[idx].children.is_some() {
            return Ok(());
        }
        let depth = self.entries[idx].depth + 1;
        let dir = self.entries[idx].path.clone();
        let items = read_level(&dir).map_err(|e| format!("{}: {e}", dir.display()));
        let mut children = Vec::new();
        if let Ok(items) = &items {
            for (name, path, is_dir) in items {
                let kind = if *is_dir {
                    EntryKind::Dir
                } else if is_object_file(name) {
                    EntryKind::Object(ObjectState::Unloaded)
                } else {
                    EntryKind::File
                };
                children.push(self.entries.len());
                self.entries
                    .push(Entry::new(name.clone(), path.clone(), kind, depth));
            }
        }
        self.entries[idx].children = Some(children);
        items.map(|_| ())
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(root) = self.root else {
            return;
        };
        let mut stack: Vec<usize> = self.entries[root]
            .children
            .iter()
            .flatten()
            .rev()
            .copied()
            .collect();
        while let Some(i) = stack.pop() {
            self.rows.push(i);
            let e = &self.entries[i];
            if e.expanded {
                stack.extend(e.children.iter().flatten().rev().copied());
            }
        }
    }

    fn toggle(&mut self, idx: usize, cx: &mut Cx) {
        let entry = &mut self.entries[idx];
        let expand = !entry.expanded;
        match entry.kind {
            EntryKind::Dir => {
                if expand {
                    if let Err(e) = self.load_children(idx) {
                        cx.log_error(e);
                    }
                }
            }
            EntryKind::Object(state) => {
                if expand && matches!(state, ObjectState::Unloaded | ObjectState::Failed) {
                    entry.kind = EntryKind::Object(ObjectState::Loading);
                    entry.detail = Some("loading…".into());
                    entry.children = Some(Vec::new());
                    cx.act(Action::LoadObject(entry.path.clone()));
                }
            }
            EntryKind::Group => {}
            EntryKind::File | EntryKind::Symbol(_) => return,
        }
        self.entries[idx].expanded = expand;
        self.rebuild_rows();
        self.clamp_scroll();
        cx.repaint();
    }

    fn activate(&mut self, idx: usize, cx: &mut Cx) {
        match self.entries[idx].kind {
            EntryKind::File => cx.act(Action::OpenFile(self.entries[idx].path.clone())),
            kind if kind.expandable() => self.toggle(idx, cx),
            _ => {}
        }
    }

    /// Shows a finished (re)load under the object's row. Returns `false` if
    /// the object is no longer in the tree.
    pub fn apply_object(&mut self, path: &Path, graph: Result<&CodeGraph, &str>) -> bool {
        let Some(idx) = self
            .entries
            .iter()
            .position(|e| matches!(e.kind, EntryKind::Object(_)) && e.path == path)
        else {
            return false;
        };
        // Remember which groups were open so a reload keeps them open.
        let open_groups: HashSet<String> = self.entries[idx]
            .children
            .iter()
            .flatten()
            .filter(|&&c| self.entries[c].kind == EntryKind::Group && self.entries[c].expanded)
            .map(|&c| group_key(&self.entries[c].name).to_owned())
            .collect();
        let first_load = self.entries[idx].kind != EntryKind::Object(ObjectState::Loaded);

        let depth = self.entries[idx].depth;
        let (kind, detail, children) = match graph {
            Ok(graph) => {
                let (children, total) =
                    self.symbol_groups(graph, path, depth, &open_groups, first_load);
                (ObjectState::Loaded, format!("{total} symbols"), children)
            }
            Err(message) => (ObjectState::Failed, message.to_owned(), Vec::new()),
        };
        let entry = &mut self.entries[idx];
        entry.kind = EntryKind::Object(kind);
        entry.detail = Some(detail);
        entry.children = Some(children);
        self.compact();
        self.rebuild_rows();
        self.clamp_scroll();
        true
    }

    /// Appends group and symbol entries for `graph`; returns the group
    /// indices and the symbol count. Locals (targets of `Contains`) are not
    /// listed: they belong under their function, which stage 3 does not show.
    fn symbol_groups(
        &mut self,
        graph: &CodeGraph,
        path: &Path,
        depth: usize,
        open_groups: &HashSet<String>,
        first_load: bool,
    ) -> (Vec<usize>, usize) {
        let locals: HashSet<NodeId> = graph
            .edges()
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains)
            .map(|e| e.to)
            .collect();
        let mut groups = Vec::new();
        let mut total = 0;
        for &(label, kind) in GROUPS {
            let mut symbols: Vec<_> = graph
                .nodes_of(kind)
                .filter(|n| !locals.contains(&n.id))
                .collect();
            if symbols.is_empty() {
                continue;
            }
            symbols.sort_by_key(|n| n.display_name.to_lowercase());
            total += symbols.len();
            let mut group = Entry::new(
                format!("{label} ({})", symbols.len()),
                path.to_path_buf(),
                EntryKind::Group,
                depth + 1,
            );
            group.expanded = open_groups.contains(label) || (first_load && label == "Functions");
            let mut members = Vec::with_capacity(symbols.len());
            for node in symbols {
                let mut e = Entry::new(
                    node.display_name.to_string(),
                    path.to_path_buf(),
                    EntryKind::Symbol(kind),
                    depth + 2,
                );
                e.detail = node
                    .source
                    .as_ref()
                    .map(|s| short_location(&s.file, s.line));
                members.push(self.entries.len());
                self.entries.push(e);
            }
            group.children = Some(members);
            groups.push(self.entries.len());
            self.entries.push(group);
        }
        (groups, total)
    }

    /// Drops entries no longer reachable from the root (old symbol lists
    /// after a reload) and remaps indices.
    fn compact(&mut self) {
        let Some(root) = self.root else {
            return;
        };
        let mut remap: HashMap<usize, usize> = HashMap::new();
        let mut order = Vec::new();
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            remap.insert(i, order.len());
            order.push(i);
            if let Some(children) = &self.entries[i].children {
                stack.extend(children.iter().rev());
            }
        }
        if order.len() == self.entries.len() {
            return;
        }
        let mut entries = Vec::with_capacity(order.len());
        for &old in &order {
            let mut e = self.entries[old].clone();
            if let Some(children) = &mut e.children {
                for c in children.iter_mut() {
                    *c = remap[c];
                }
            }
            entries.push(e);
        }
        self.entries = entries;
        self.root = Some(remap[&root]);
        self.selected = self.selected.and_then(|s| remap.get(&s).copied());
        self.hover_row = None;
    }

    fn row_at(&self, p: PointF) -> Option<usize> {
        if !self.list.contains(p) {
            return None;
        }
        let i = ((p.y - self.list.y + self.scroll) / ROW_H) as usize;
        (i < self.rows.len()).then_some(i)
    }

    fn clamp_scroll(&mut self) {
        let max = (self.rows.len() as f32 * ROW_H - self.list.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn ensure_visible(&mut self, row: usize) {
        let y = row as f32 * ROW_H;
        if y < self.scroll {
            self.scroll = y;
        } else if y + ROW_H > self.scroll + self.list.h {
            self.scroll = y + ROW_H - self.list.h;
        }
        self.clamp_scroll();
    }

    fn select_row(&mut self, row: usize, cx: &mut Cx) {
        if let Some(&idx) = self.rows.get(row) {
            self.selected = Some(idx);
            self.ensure_visible(row);
            cx.repaint();
        }
    }

    fn selected_row(&self) -> Option<usize> {
        let sel = self.selected?;
        self.rows.iter().position(|&i| i == sel)
    }

    fn parent_row(&self, row: usize) -> Option<usize> {
        let depth = self.entries[self.rows[row]].depth;
        (0..row)
            .rev()
            .find(|&r| self.entries[self.rows[r]].depth < depth)
    }

    fn on_key(&mut self, key: Key, cx: &mut Cx) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let row = self.selected_row();
        match key {
            Key::Down => self.select_row(row.map_or(0, |r| (r + 1).min(self.rows.len() - 1)), cx),
            Key::Up => self.select_row(row.map_or(0, |r| r.saturating_sub(1)), cx),
            Key::Home => self.select_row(0, cx),
            Key::End => self.select_row(self.rows.len() - 1, cx),
            Key::Enter => {
                if let Some(r) = row {
                    self.activate(self.rows[r], cx);
                }
            }
            Key::Right => {
                if let Some(r) = row {
                    let idx = self.rows[r];
                    if self.entries[idx].kind.expandable() && !self.entries[idx].expanded {
                        self.toggle(idx, cx);
                    }
                }
            }
            Key::Left => {
                if let Some(r) = row {
                    let idx = self.rows[r];
                    if self.entries[idx].kind.expandable() && self.entries[idx].expanded {
                        self.toggle(idx, cx);
                    } else if let Some(p) = self.parent_row(r) {
                        self.select_row(p, cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

impl Panel for Explorer {
    fn layout(&mut self, rect: RectF, _text: &TextSystem) {
        self.rect = rect;
        let (header, list) = rect.split_top(HEADER_H);
        self.header = header;
        self.list = list;
        self.button = RectF::new(
            list.x + 16.0,
            list.y + 40.0,
            (list.w - 32.0).clamp(0.0, 220.0),
            30.0,
        );
        self.clamp_scroll();
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        d.fill_rect(self.rect, theme.panel_bg);
        let title = match self.root {
            Some(r) => format!("EXPLORER — {}", self.entries[r].name.to_uppercase()),
            None => "EXPLORER".to_owned(),
        };
        d.text(
            TextStyle::UiSmall,
            &title,
            RectF::new(
                self.header.x + 12.0,
                self.header.y,
                self.header.w - 24.0,
                self.header.h,
            ),
            theme.text_dim,
        );

        if self.root.is_none() {
            d.text(
                TextStyle::Ui,
                "No folder opened.",
                RectF::new(
                    self.list.x + 16.0,
                    self.list.y + 6.0,
                    self.list.w - 32.0,
                    24.0,
                ),
                theme.text_dim,
            );
            let bg = if self.button_hover {
                theme.accent.with_alpha(0.85)
            } else {
                theme.button_bg
            };
            d.fill_round_rect(self.button, 3.0, bg);
            let label_w = d
                .text_system()
                .measure(TextStyle::Ui, "Open Folder (Ctrl+O)");
            let b = self.button;
            d.text(
                TextStyle::Ui,
                "Open Folder (Ctrl+O)",
                RectF::new(b.x + ((b.w - label_w) / 2.0).max(4.0), b.y, b.w - 8.0, b.h),
                theme.button_text,
            );
            return;
        }

        let list = self.list;
        let first = (self.scroll / ROW_H) as usize;
        let last = (first + (list.h / ROW_H).ceil() as usize + 1).min(self.rows.len());
        let text = d.text_system();
        d.clip(list, |d| {
            for row in first..last {
                let idx = self.rows[row];
                let e = &self.entries[idx];
                let y = list.y + row as f32 * ROW_H - self.scroll;
                let r = RectF::new(list.x, y, list.w, ROW_H);
                if self.selected == Some(idx) {
                    let bg = if focused {
                        theme.row_selected_focus_bg
                    } else {
                        theme.row_selected_bg
                    };
                    d.fill_rect(r, bg);
                } else if self.hover_row == Some(row) {
                    d.fill_rect(r, theme.hover_bg);
                }
                let x = list.x + LEFT_PAD + (e.depth.saturating_sub(1)) as f32 * INDENT;
                let marker = match e.kind {
                    k if k.expandable() => {
                        Some((if e.expanded { "▾" } else { "▸" }, theme.text_dim))
                    }
                    EntryKind::Symbol(NodeKind::Function) => Some(("ƒ", theme.accent)),
                    EntryKind::Symbol(NodeKind::External) => Some(("↗", theme.text_dim)),
                    EntryKind::Symbol(_) => Some(("▪", theme.text_dim)),
                    _ => None,
                };
                if let Some((glyph, color)) = marker {
                    d.text(TextStyle::Ui, glyph, RectF::new(x, y, 14.0, ROW_H), color);
                }
                let name_x = x + 16.0;
                let name_color = match e.kind {
                    EntryKind::Group => theme.text_dim,
                    _ => theme.text,
                };
                d.text(
                    TextStyle::Ui,
                    &e.name,
                    RectF::new(name_x, y, (list.right() - name_x - 4.0).max(0.0), ROW_H),
                    name_color,
                );
                if let Some(detail) = &e.detail {
                    let dx = name_x + text.measure(TextStyle::Ui, &e.name) + 8.0;
                    let color = if e.kind == EntryKind::Object(ObjectState::Failed) {
                        theme.error
                    } else {
                        theme.text_dim
                    };
                    d.text(
                        TextStyle::UiSmall,
                        detail,
                        RectF::new(dx, y, (list.right() - dx - 4.0).max(0.0), ROW_H),
                        color,
                    );
                }
            }
        });
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        match *e {
            Event::KeyDown { key, mods } if !mods.ctrl && !mods.alt => self.on_key(key, cx),
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                clicks,
                ..
            } => {
                if self.root.is_none() {
                    if self.button.contains(pos) {
                        cx.act(Action::PickFolder);
                    }
                    return true;
                }
                if let Some(row) = self.row_at(pos) {
                    let idx = self.rows[row];
                    self.selected = Some(idx);
                    let kind = self.entries[idx].kind;
                    if kind.expandable() {
                        if clicks == 1 {
                            self.toggle(idx, cx);
                        }
                    } else if clicks >= 2 {
                        self.activate(idx, cx);
                    }
                    cx.repaint();
                }
                true
            }
            Event::MouseMove { pos, .. } => {
                let row = self.row_at(pos);
                let on_button = self.root.is_none() && self.button.contains(pos);
                if row != self.hover_row || on_button != self.button_hover {
                    self.hover_row = row;
                    self.button_hover = on_button;
                    cx.repaint();
                }
                if on_button {
                    cx.cursor = CursorShape::Hand;
                }
                true
            }
            Event::MouseLeave => {
                if self.hover_row.is_some() || self.button_hover {
                    self.hover_row = None;
                    self.button_hover = false;
                    cx.repaint();
                }
                false
            }
            Event::Wheel { lines, .. } => {
                self.scroll -= lines * 3.0 * ROW_H;
                self.clamp_scroll();
                cx.repaint();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ex: &Explorer) -> Vec<&str> {
        ex.rows
            .iter()
            .map(|&i| ex.entries[i].name.as_str())
            .collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lattice-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../engine/tests/fixtures")
            .join(name)
    }

    #[test]
    fn tree_lists_dirs_first_hides_git_and_expands_lazily() {
        let root = temp_dir("tree");
        std::fs::create_dir_all(root.join("src/inner")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("A.md"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();

        let mut ex = Explorer::default();
        ex.set_root(&root).unwrap();
        assert_eq!(names(&ex), ["src", "A.md", "b.txt"]);

        let src = ex.rows[0];
        assert!(ex.entries[src].children.is_none());
        ex.load_children(src).unwrap();
        ex.entries[src].expanded = true;
        ex.rebuild_rows();
        assert_eq!(names(&ex), ["src", "inner", "main.rs", "A.md", "b.txt"]);
        assert_eq!(ex.parent_row(2), Some(0));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn object_files_expand_into_grouped_symbols_and_reload_in_place() {
        let root = temp_dir("objects");
        let obj = root.join("graph.obj");
        std::fs::copy(fixture("msvc_z7.obj"), &obj).unwrap();
        std::fs::write(root.join("notes.txt"), "").unwrap();

        let mut ex = Explorer::default();
        ex.set_root(&root).unwrap();
        let idx = ex.rows[0];
        assert_eq!(
            ex.entries[idx].kind,
            EntryKind::Object(ObjectState::Unloaded)
        );
        assert_eq!(ex.entries[ex.rows[1]].kind, EntryKind::File);

        let graph = engine::load(&obj).unwrap();
        assert!(ex.apply_object(&obj, Ok(&graph)));
        let obj_idx = ex.rows[0];
        ex.entries[obj_idx].expanded = true;
        ex.rebuild_rows();
        assert_eq!(
            names(&ex),
            [
                "graph.obj",
                "Functions (4)",
                "callee",
                "caller",
                "helper",
                "read_counter",
                "Variables (2)",
                "Externals (1)",
                "notes.txt"
            ]
        );
        let callee = ex.rows[2];
        assert_eq!(ex.entries[callee].detail.as_deref(), Some("graph.c:21"));

        // Open Variables, then reload: groups keep their state and the old
        // entries are compacted away.
        let vars = ex.rows[6];
        ex.entries[vars].expanded = true;
        let before = ex.entries.len();
        assert!(ex.apply_object(&obj, Ok(&graph)));
        ex.rebuild_rows();
        assert_eq!(ex.entries.len(), before);
        assert!(names(&ex).contains(&"counter"));
        assert!(names(&ex).contains(&"callee"));

        assert!(ex.apply_object(&obj, Err("broken")));
        ex.rebuild_rows();
        assert_eq!(names(&ex), ["graph.obj", "notes.txt"]);
        let obj_idx = ex.rows[0];
        assert_eq!(
            ex.entries[obj_idx].kind,
            EntryKind::Object(ObjectState::Failed)
        );
        assert!(!ex.apply_object(&root.join("missing.o"), Err("x")));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
