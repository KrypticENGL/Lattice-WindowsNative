//! Solution Explorer: a lazily loaded folder tree. Object files (`.obj`,
//! `.o`) expand into their symbols, loaded by the engine in the background.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use engine::{CodeGraph, DebugFormat, EdgeKind, Node, NodeId, NodeKind};

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::fileops;
use crate::geom::{PointF, RectF};
use crate::menubar::Entry as Row;
use crate::namefield::NameField;
use crate::objects::is_object_file;
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::sys::clipboard;
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const HEADER_H: f32 = 34.0;
const ROW_H: f32 = 22.0;
/// The name box's strip: its label, the box and a gap under it.
const NAME_STRIP_H: f32 = 2.0 * ROW_H + 6.0;
const NAME_PAD: f32 = 5.0;
/// How far a press must move to be a drag rather than a click.
const DRAG_SLOP: f32 = 5.0;
const INDENT: f32 = 12.0;
const LEFT_PAD: f32 = 8.0;
/// Directory names that are never listed.
const HIDDEN: &[&str] = &[".git"];
/// Symbol groups under a loaded object, in display order. Compiler-
/// generated symbols of every kind go to `GENERATED` instead.
const GROUPS: &[(&str, NodeKind)] = &[
    ("Functions", NodeKind::Function),
    ("Variables", NodeKind::Variable),
    ("Externals", NodeKind::External),
];
/// Code from headers outside the project (standard-library templates...).
const LIBRARY: &str = "Library";
const GENERATED: &str = "Compiler-generated";

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

/// A real file or folder, rather than a row listing an object's contents.
fn is_on_disk(entry: &Entry) -> bool {
    matches!(
        entry.kind,
        EntryKind::Dir | EntryKind::File | EntryKind::Object(_)
    )
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

    /// Folders, objects and groups always; functions when they have
    /// parameters or locals to show.
    fn expandable(&self) -> bool {
        match self.kind {
            EntryKind::Dir | EntryKind::Object(_) | EntryKind::Group => true,
            EntryKind::Symbol(_) => self.children.as_ref().is_some_and(|c| !c.is_empty()),
            EntryKind::File => false,
        }
    }

    /// Identity used to restore expansion after a reload.
    fn expansion_key(&self) -> String {
        match self.kind {
            EntryKind::Group => format!("group:{}", group_key(&self.name)),
            _ => format!("symbol:{}", self.name),
        }
    }
}

/// A press on a row, which ends as a click or a drag.
struct Press {
    idx: usize,
    start: PointF,
    /// A first click on something that opens: it does, on release, unless
    /// this turns out to be a drag.
    toggles: bool,
    dragging: bool,
    pos: PointF,
    /// The folder a drop here would move the item into, and where it would
    /// end up.
    drop: Option<(PathBuf, PathBuf)>,
}

/// What the name box is asking for.
#[derive(Debug, Clone, PartialEq)]
enum NameTarget {
    New { dir: PathBuf, folder: bool },
    Rename { path: PathBuf },
}

/// The name box, in a strip above the tree while it is open.
struct NameEdit {
    target: NameTarget,
    field: NameField,
    error: Option<String>,
}

impl NameEdit {
    /// What Enter does: an action, `None` for a rename to the same name, or
    /// why the name is no good.
    fn commit(&self) -> Result<Option<Action>, String> {
        let name = self.field.text().trim();
        fileops::validate_name(name)?;
        Ok(match &self.target {
            NameTarget::New { dir, folder } => Some(Action::CreatePath {
                path: dir.join(name),
                folder: *folder,
            }),
            NameTarget::Rename { path } => {
                let to = path.with_file_name(name);
                (to != *path).then(|| Action::RenamePath {
                    from: path.clone(),
                    to,
                })
            }
        })
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
    name_edit: Option<NameEdit>,
    /// The name box's strip, while it is open.
    name_strip: RectF,
    press: Option<Press>,
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

fn entry_kind(name: &str, is_dir: bool) -> EntryKind {
    if is_dir {
        EntryKind::Dir
    } else if is_object_file(name) {
        EntryKind::Object(ObjectState::Unloaded)
    } else {
        EntryKind::File
    }
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
        self.press = None;
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
                let kind = entry_kind(name, *is_dir);
                children.push(self.entries.len());
                self.entries
                    .push(Entry::new(name.clone(), path.clone(), kind, depth));
            }
        }
        self.entries[idx].children = Some(children);
        items.map(|_| ())
    }

    /// Re-reads loaded folders after changes on disk: `dirs`, or every
    /// loaded folder when `None`. Entries still on disk keep their state
    /// (expansion, loaded symbols); new ones are added and removed ones
    /// dropped. Returns whether the tree changed.
    pub fn refresh(&mut self, dirs: Option<&HashSet<PathBuf>>) -> bool {
        let targets: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.kind == EntryKind::Dir
                    && e.children.is_some()
                    && dirs.is_none_or(|d| d.contains(&e.path))
            })
            .map(|(i, _)| i)
            .collect();
        let mut changed = false;
        for idx in targets {
            changed |= self.refresh_dir(idx);
        }
        if changed {
            self.compact();
            self.rebuild_rows();
            self.clamp_scroll();
        }
        changed
    }

    /// Re-reads one loaded folder; unreachable old entries are left for
    /// `compact`. A folder that can no longer be read is left as it is: its
    /// parent's refresh drops it if it was removed.
    fn refresh_dir(&mut self, idx: usize) -> bool {
        let Ok(items) = read_level(&self.entries[idx].path) else {
            return false;
        };
        let old = self.entries[idx].children.clone().unwrap_or_default();
        let depth = self.entries[idx].depth + 1;
        let mut children = Vec::with_capacity(items.len());
        for (name, path, is_dir) in items {
            let kind = entry_kind(&name, is_dir);
            let existing = old.iter().copied().find(|&c| {
                let e = &self.entries[c];
                e.name == name && std::mem::discriminant(&e.kind) == std::mem::discriminant(&kind)
            });
            match existing {
                Some(c) => children.push(c),
                None => {
                    children.push(self.entries.len());
                    self.entries.push(Entry::new(name, path, kind, depth));
                }
            }
        }
        if children == old {
            return false;
        }
        self.entries[idx].children = Some(children);
        true
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
            EntryKind::Symbol(_) if entry.expandable() => {}
            EntryKind::File | EntryKind::Symbol(_) => return,
        }
        self.entries[idx].expanded = expand;
        self.rebuild_rows();
        self.clamp_scroll();
        cx.repaint();
    }

    fn activate(&mut self, idx: usize, cx: &mut Cx) {
        let entry = &self.entries[idx];
        match entry.kind {
            EntryKind::File => cx.act(Action::OpenFile(entry.path.clone())),
            _ if entry.expandable() => self.toggle(idx, cx),
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
        // Remember what was open so a reload keeps it open.
        let mut open_groups: HashSet<String> = HashSet::new();
        let mut stack: Vec<usize> = self.entries[idx]
            .children
            .iter()
            .flatten()
            .copied()
            .collect();
        while let Some(c) = stack.pop() {
            let e = &self.entries[c];
            if e.expanded {
                open_groups.insert(e.expansion_key());
                stack.extend(e.children.iter().flatten().copied());
            }
        }
        let first_load = self.entries[idx].kind != EntryKind::Object(ObjectState::Loaded);

        let depth = self.entries[idx].depth;
        let (kind, detail, children) = match graph {
            Ok(graph) => {
                let (children, total) =
                    self.symbol_groups(graph, path, depth, &open_groups, first_load);
                let detail = if graph.debug == DebugFormat::None {
                    // Without debug info there are no locals, types or lines.
                    format!("{total} symbols · no debug info")
                } else {
                    format!("{total} symbols")
                };
                (ObjectState::Loaded, detail, children)
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
    /// indices and the number of top-level symbols. Parameters and locals
    /// (targets of `Contains`) are listed under their function, with their
    /// type as the detail; compiler-generated symbols get their own group.
    fn symbol_groups(
        &mut self,
        graph: &CodeGraph,
        path: &Path,
        depth: usize,
        open: &HashSet<String>,
        first_load: bool,
    ) -> (Vec<usize>, usize) {
        let mut locals_of: HashMap<NodeId, Vec<&Node>> = HashMap::new();
        for e in graph
            .edges()
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains)
        {
            if let Some(local) = graph.node(e.to) {
                locals_of.entry(e.from).or_default().push(local);
            }
        }
        let is_local: HashSet<NodeId> = locals_of.values().flatten().map(|n| n.id).collect();
        let type_of = |id: NodeId| {
            graph
                .edges_from(id)
                .find(|e| e.kind == EdgeKind::HasType)
                .and_then(|e| graph.node(e.to))
                .map(|t| t.display_name.to_string())
        };

        let mut buckets: Vec<(&str, NodeKind, Vec<&Node>)> = GROUPS
            .iter()
            .map(|&(label, kind)| (label, kind, Vec::new()))
            .collect();
        let mut generated: Vec<&Node> = Vec::new();
        let mut library: Vec<&Node> = Vec::new();
        for node in graph.nodes() {
            if is_local.contains(&node.id) {
                continue;
            }
            if node.artificial {
                if matches!(
                    node.kind,
                    NodeKind::Function | NodeKind::Variable | NodeKind::External
                ) {
                    generated.push(node);
                }
                continue;
            }
            if node.library {
                library.push(node);
                continue;
            }
            if let Some(bucket) = buckets.iter_mut().find(|b| b.1 == node.kind) {
                bucket.2.push(node);
            }
        }

        let mut groups = Vec::new();
        let mut total = 0;
        let all = buckets.into_iter().chain([
            (LIBRARY, NodeKind::Function, library),
            (GENERATED, NodeKind::Function, generated),
        ]);
        for (label, _, mut symbols) in all {
            if symbols.is_empty() {
                continue;
            }
            symbols.sort_by_key(|n| n.display_name.to_lowercase());
            if label != GENERATED && label != LIBRARY {
                total += symbols.len();
            }
            let mut group = Entry::new(
                format!("{label} ({})", symbols.len()),
                path.to_path_buf(),
                EntryKind::Group,
                depth + 1,
            );
            group.expanded =
                open.contains(&group.expansion_key()) || (first_load && label == "Functions");
            let mut members = Vec::with_capacity(symbols.len());
            for node in symbols {
                let mut e = Entry::new(
                    node.display_name.to_string(),
                    path.to_path_buf(),
                    EntryKind::Symbol(node.kind),
                    depth + 2,
                );
                e.detail = node
                    .source
                    .as_ref()
                    .map(|s| short_location(&s.file, s.line));
                if let Some(locals) = locals_of.get_mut(&node.id) {
                    // Declaration order when lines are known, else by name.
                    locals.sort_by_key(|l| {
                        (
                            l.source.as_ref().map_or(u32::MAX, |s| s.line),
                            l.display_name.clone(),
                        )
                    });
                    let mut children = Vec::with_capacity(locals.len());
                    for local in locals.iter() {
                        let mut le = Entry::new(
                            local.display_name.to_string(),
                            path.to_path_buf(),
                            EntryKind::Symbol(NodeKind::Variable),
                            depth + 3,
                        );
                        le.detail = type_of(local.id);
                        children.push(self.entries.len());
                        self.entries.push(le);
                    }
                    e.children = Some(children);
                    e.expanded = open.contains(&e.expansion_key());
                }
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
        // If what was being dragged has gone, so has the drag.
        self.press = self.press.take().and_then(|mut p| {
            p.idx = *remap.get(&p.idx)?;
            Some(p)
        });
    }

    /// The folder under `pos` for a drop: the row's, a file row's parent, or
    /// the top folder below the last row.
    fn drop_folder(&self, pos: PointF) -> Option<PathBuf> {
        if !self.list.contains(pos) {
            return None;
        }
        let Some(row) = self.row_at(pos) else {
            return self.root_path().map(Path::to_path_buf);
        };
        let entry = &self.entries[self.rows[row]];
        match entry.kind {
            EntryKind::Dir => Some(entry.path.clone()),
            EntryKind::File | EntryKind::Object(_) => entry.path.parent().map(Path::to_path_buf),
            EntryKind::Group | EntryKind::Symbol(_) => None,
        }
    }

    /// The mouse moved with a press down. Returns whether it is a drag.
    fn drag_to(&mut self, pos: PointF, cx: &mut Cx) -> bool {
        let Some(press) = &self.press else {
            return false;
        };
        let moved = (pos.x - press.start.x).hypot(pos.y - press.start.y) >= DRAG_SLOP;
        let entry = &self.entries[press.idx];
        if !(press.dragging || moved && is_on_disk(entry)) {
            return false;
        }
        let from = entry.path.clone();
        // shortcut: only scrolls as the mouse moves, not while it rests at
        // the edge; add a timer if long trees make that a chore.
        if pos.y < self.list.y + ROW_H {
            self.scroll -= ROW_H;
        } else if pos.y > self.list.bottom() - ROW_H {
            self.scroll += ROW_H;
        }
        self.clamp_scroll();
        let drop = self.drop_folder(pos).and_then(|dir| {
            let to = fileops::move_target(&from, &dir)?;
            Some((dir, to))
        });
        if let Some(press) = &mut self.press {
            press.dragging = true;
            press.pos = pos;
            press.drop = drop;
        }
        cx.cursor = CursorShape::Move;
        cx.repaint();
        true
    }

    /// The selected file or folder, not a symbol listed under an object.
    pub fn selected_path(&self) -> Option<&Path> {
        let entry = &self.entries[self.selected?];
        is_on_disk(entry).then_some(entry.path.as_path())
    }

    /// Where a new item goes for this selection: the folder itself, or the
    /// one holding the file.
    fn folder_of_selection(&self) -> Option<PathBuf> {
        let root = self.root_path()?;
        let dir = self
            .selected_path()
            .and_then(|p| if p.is_dir() { Some(p) } else { p.parent() });
        Some(
            dir.filter(|d| d.starts_with(root))
                .unwrap_or(root)
                .to_path_buf(),
        )
    }

    /// Opens the name box for a new item in `dir` (by default, by the
    /// selection). Does nothing with no folder open.
    pub fn begin_new(&mut self, folder: bool, dir: Option<PathBuf>) {
        let Some(dir) = dir.or_else(|| self.folder_of_selection()) else {
            return;
        };
        self.reveal(&dir);
        self.name_edit = Some(NameEdit {
            target: NameTarget::New { dir, folder },
            field: NameField::new("", 0),
            error: None,
        });
    }

    /// Opens the name box on `path`'s name, with the part before the
    /// extension selected.
    pub fn begin_rename(&mut self, path: &Path) {
        self.reveal(path);
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let stem = match name.rfind('.') {
            Some(dot) if dot > 0 && !path.is_dir() => dot,
            _ => name.len(),
        };
        self.name_edit = Some(NameEdit {
            target: NameTarget::Rename {
                path: path.to_path_buf(),
            },
            field: NameField::new(&name, stem),
            error: None,
        });
    }

    /// The create or rename worked: closes the name box.
    pub fn finish_name_edit(&mut self) {
        self.name_edit = None;
    }

    /// The create or rename failed: says why in the name box, still open.
    pub fn name_edit_failed(&mut self, message: String) {
        if let Some(edit) = &mut self.name_edit {
            edit.error = Some(message);
        }
    }

    /// Input while the name box is open; `None` if it isn't the box's.
    fn name_event(&mut self, e: &Event, cx: &mut Cx) -> Option<bool> {
        let edit = self.name_edit.as_mut()?;
        match *e {
            Event::Char(c) => {
                edit.field.insert(c.encode_utf8(&mut [0; 4]));
                edit.error = None;
            }
            Event::KeyDown {
                key: Key::Escape, ..
            } => self.name_edit = None,
            Event::KeyDown {
                key: Key::Enter, ..
            } => match edit.commit() {
                Ok(Some(action)) => cx.act(action),
                Ok(None) => self.name_edit = None,
                Err(e) => edit.error = Some(e),
            },
            Event::KeyDown {
                key: Key::Letter(letter),
                mods,
            } if mods.ctrl => match letter {
                'A' => edit.field.select_all(),
                'C' | 'X' if !edit.field.selected_text().is_empty() => {
                    if let Err(e) = clipboard::set_text(cx.window, edit.field.selected_text()) {
                        cx.log_error(format!("Copy failed: {e}"));
                    } else if letter == 'X' {
                        edit.field.insert("");
                    }
                }
                'V' => {
                    if let Some(text) = clipboard::get_text(cx.window) {
                        edit.field.insert(&text);
                    }
                }
                _ => {}
            },
            Event::KeyDown { key, mods } => {
                if edit.field.key(key, mods) {
                    edit.error = None;
                }
            }
            // shortcut: clicking in the box doesn't move the caret; use the
            // arrow keys, or measure the text here if that is missed.
            Event::MouseDown { pos, .. } if self.name_strip.contains(pos) => {}
            // A click anywhere else gives up rather than creating something.
            Event::MouseDown { .. } => {
                self.name_edit = None;
                cx.repaint();
                return None;
            }
            _ => return None,
        }
        cx.repaint();
        Some(true)
    }

    /// Expands the way down to `path` and selects it, if it is in the tree.
    pub fn reveal(&mut self, path: &Path) {
        let Some(mut at) = self.root else {
            return;
        };
        while self.entries[at].path != path {
            let entry = &self.entries[at];
            if entry.kind != EntryKind::Dir {
                return;
            }
            if entry.children.is_none() && self.load_children(at).is_err() {
                return;
            }
            self.entries[at].expanded = true;
            let children = self.entries[at].children.as_deref().unwrap_or_default();
            let Some(&next) = children
                .iter()
                .find(|&&c| path.starts_with(&self.entries[c].path))
            else {
                self.rebuild_rows();
                return;
            };
            at = next;
        }
        self.rebuild_rows();
        if at != self.root.unwrap_or(0) {
            self.selected = Some(at);
        }
        if let Some(row) = self.selected_row() {
            self.ensure_visible(row);
        }
    }

    /// The right-click menu for an entry, or for the empty space below the
    /// tree.
    fn context_menu(&self, idx: Option<usize>) -> Vec<Row<Action>> {
        let has_root = self.root.is_some();
        let Some(entry) = idx.map(|i| &self.entries[i]) else {
            let new = |folder, label: &'static str, hint| {
                let dir = self.root_path().map(Path::to_path_buf);
                Row::item(Action::NewEntry { folder, dir }, label, hint, has_root)
            };
            return vec![
                new(false, "New file\u{2026}", "Ctrl+N"),
                new(true, "New folder\u{2026}", "Ctrl+Shift+N"),
                Row::Separator,
                Row::item(Action::PickFolder, "Open folder\u{2026}", "Ctrl+O", true),
                Row::item(
                    Action::LoadFolderObjects,
                    "Add folder objects",
                    "",
                    has_root,
                ),
                Row::item(Action::Debug(DebugAction::Build), "Build", "", has_root),
            ];
        };
        let first = match entry.kind {
            EntryKind::Group => return Vec::new(),
            EntryKind::Symbol(_) => {
                let name = Action::CopyText(entry.name.clone());
                return vec![Row::item(name, "Copy name", "", true)];
            }
            EntryKind::File => Some((Action::OpenFile(entry.path.clone()), "Open")),
            EntryKind::Object(_) => Some((Action::LoadObject(entry.path.clone()), "Load symbols")),
            EntryKind::Dir => None,
        };
        let mut items: Vec<Row<Action>> = first
            .into_iter()
            .flat_map(|(a, label)| [Row::item(a, label, "", true), Row::Separator])
            .collect();
        let dir = if entry.kind == EntryKind::Dir {
            Some(entry.path.clone())
        } else {
            entry.path.parent().map(Path::to_path_buf)
        };
        items.extend([
            Row::item(
                Action::NewEntry {
                    folder: false,
                    dir: dir.clone(),
                },
                "New file\u{2026}",
                "Ctrl+N",
                true,
            ),
            Row::item(
                Action::NewEntry { folder: true, dir },
                "New folder\u{2026}",
                "Ctrl+Shift+N",
                true,
            ),
            Row::Separator,
            Row::item(
                Action::BeginRename(entry.path.clone()),
                "Rename\u{2026}",
                "F2",
                true,
            ),
            Row::item(
                Action::DeletePath(entry.path.clone()),
                "Delete",
                "Del",
                true,
            ),
            Row::Separator,
        ]);
        let relative = self
            .root_path()
            .and_then(|r| entry.path.strip_prefix(r).ok())
            .unwrap_or(&entry.path);
        items.extend([
            Row::item(
                Action::CopyText(entry.path.display().to_string()),
                "Copy path",
                "",
                true,
            ),
            Row::item(
                Action::CopyText(relative.display().to_string()),
                "Copy relative path",
                "",
                true,
            ),
            Row::item(
                Action::Reveal(entry.path.clone()),
                "Reveal in File Explorer",
                "",
                true,
            ),
        ]);
        items
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
            Key::F(2) => {
                if let Some(p) = self.selected_path() {
                    cx.act(Action::BeginRename(p.to_path_buf()));
                }
            }
            Key::Delete => {
                if let Some(p) = self.selected_path() {
                    cx.act(Action::DeletePath(p.to_path_buf()));
                }
            }
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
                    if self.entries[idx].expandable() && !self.entries[idx].expanded {
                        self.toggle(idx, cx);
                    }
                }
            }
            Key::Left => {
                if let Some(r) = row {
                    let idx = self.rows[r];
                    if self.entries[idx].expandable() && self.entries[idx].expanded {
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

impl Explorer {
    /// The strip above the tree: what is being asked for (or what is wrong
    /// with the answer), and the box.
    fn paint_name_edit(&self, d: &mut DrawCtx, theme: &Theme) {
        let Some(edit) = &self.name_edit else {
            return;
        };
        let strip = self.name_strip;
        let label_rect = RectF::new(strip.x + 12.0, strip.y, strip.w - 24.0, ROW_H);
        let label = match &edit.error {
            Some(error) => error.clone(),
            None => match &edit.target {
                NameTarget::New { dir, folder } => {
                    let what = if *folder { "New folder" } else { "New file" };
                    let name = dir.file_name().unwrap_or_default().to_string_lossy();
                    format!("{what} in {name}")
                }
                NameTarget::Rename { path } => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    format!("Rename {name}")
                }
            },
        };
        let (label_color, border) = match edit.error {
            Some(_) => (theme.error, theme.error),
            None => (theme.text_dim, theme.accent),
        };
        d.text(TextStyle::UiSmall, &label, label_rect, label_color);

        let field = RectF::new(
            strip.x + 12.0,
            strip.y + ROW_H,
            (strip.w - 24.0).max(0.0),
            ROW_H,
        );
        d.fill_rect(field, theme.editor_bg);
        let text = d.text_system();
        let name = edit.field.text();
        let x_of = |byte: usize| text.measure(TextStyle::Ui, &name[..byte]);
        // Slides left so the caret stays in the box.
        let inner = (field.w - 2.0 * NAME_PAD).max(0.0);
        let caret_x = x_of(edit.field.caret());
        let origin = field.x + NAME_PAD - (caret_x - inner).max(0.0);
        d.clip(field, |d| {
            let (start, end) = edit.field.selection();
            if start != end {
                let x0 = origin + x_of(start);
                let sel = RectF::new(x0, field.y + 2.0, origin + x_of(end) - x0, field.h - 4.0);
                d.fill_rect(sel, theme.selection_bg);
            }
            let w = text.measure(TextStyle::Ui, name) + 4.0;
            d.text(
                TextStyle::Ui,
                name,
                RectF::new(origin, field.y, w, field.h),
                theme.text,
            );
            d.fill_rect(
                RectF::new(origin + caret_x, field.y + 3.0, 1.5, field.h - 6.0),
                theme.caret,
            );
        });
        d.stroke_rect(field, border, 1.0);
    }
}

impl Panel for Explorer {
    fn layout(&mut self, rect: RectF, _text: &TextSystem) {
        self.rect = rect;
        let (header, mut list) = rect.split_top(HEADER_H);
        self.header = header;
        if self.name_edit.is_some() {
            (self.name_strip, list) = list.split_top(NAME_STRIP_H);
        }
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

        self.paint_name_edit(d, theme);
        let list = self.list;
        let first = (self.scroll / ROW_H) as usize;
        let last = (first + (list.h / ROW_H).ceil() as usize + 1).min(self.rows.len());
        let text = d.text_system();
        let drag = self.press.as_ref().filter(|p| p.dragging);
        let drop_dir = drag.and_then(|p| p.drop.as_ref()).map(|(dir, _)| dir);
        d.clip(list, |d| {
            for row in first..last {
                let idx = self.rows[row];
                let e = &self.entries[idx];
                let y = list.y + row as f32 * ROW_H - self.scroll;
                let r = RectF::new(list.x, y, list.w, ROW_H);
                if e.kind == EntryKind::Dir && drop_dir == Some(&e.path) {
                    d.fill_rect(r, theme.dock_drop);
                    d.stroke_rect(r, theme.accent, 1.0);
                } else if self.selected == Some(idx) {
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
                let chevron = e.expandable().then_some(if e.expanded { "▾" } else { "▸" });
                let icon = match e.kind {
                    EntryKind::Symbol(NodeKind::Function) => Some(("ƒ", theme.accent)),
                    EntryKind::Symbol(NodeKind::External) => Some(("↗", theme.text_dim)),
                    EntryKind::Symbol(_) => Some(("▪", theme.text_dim)),
                    _ => None,
                };
                // A function with locals shows its chevron left of its icon,
                // in the indent column.
                let chevron_x = if icon.is_some() { x - INDENT } else { x };
                if let Some(glyph) = chevron {
                    d.text(
                        TextStyle::Ui,
                        glyph,
                        RectF::new(chevron_x, y, 14.0, ROW_H),
                        theme.text_dim,
                    );
                }
                if let Some((glyph, color)) = icon {
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
            // The top folder has no row: the whole list lights up for it.
            if drop_dir.is_some() && drop_dir.map(PathBuf::as_path) == self.root_path() {
                d.stroke_rect(list, theme.accent, 1.0);
            }
            // The dragged item's name, by the pointer.
            if let Some(p) = drag {
                let name = &self.entries[p.idx].name;
                let w = text.measure(TextStyle::Ui, name) + 16.0;
                let r = RectF::new(p.pos.x + 12.0, p.pos.y + 4.0, w, ROW_H);
                d.fill_round_rect(r, 4.0, theme.row_selected_bg);
                let at = RectF::new(r.x + 8.0, r.y, w, r.h);
                d.text(TextStyle::Ui, name, at, theme.text);
            }
        });
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if let Some(taken) = self.name_event(e, cx) {
            return taken;
        }
        match *e {
            Event::KeyDown {
                key: Key::Escape, ..
            } if self.press.as_ref().is_some_and(|p| p.dragging) => {
                self.press = None;
                cx.repaint();
                true
            }
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
                    let expandable = self.entries[idx].expandable();
                    if !expandable && clicks >= 2 {
                        self.activate(idx, cx);
                    }
                    self.press = (clicks == 1).then_some(Press {
                        idx,
                        start: pos,
                        toggles: expandable,
                        dragging: false,
                        pos,
                        drop: None,
                    });
                    cx.repaint();
                }
                true
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(row) = self.row_at(pos) {
                    self.selected = Some(self.rows[row]);
                    cx.repaint();
                }
                true
            }
            Event::MouseUp {
                pos,
                button: MouseButton::Right,
                ..
            } => {
                let idx = self.row_at(pos).map(|row| self.rows[row]);
                let items = self.context_menu(idx);
                if !items.is_empty() {
                    cx.act(Action::ContextMenu { pos, items });
                }
                true
            }
            Event::MouseUp {
                button: MouseButton::Left,
                ..
            } => {
                let Some(press) = self.press.take() else {
                    return false;
                };
                if press.dragging {
                    if let Some((_, to)) = press.drop {
                        cx.act(Action::MovePath {
                            from: self.entries[press.idx].path.clone(),
                            to,
                        });
                    }
                } else if press.toggles {
                    self.toggle(press.idx, cx);
                }
                cx.repaint();
                true
            }
            Event::MouseMove {
                left_down: false, ..
            } if self.press.is_some() => {
                // The release went missing (focus was taken, say).
                self.press = None;
                cx.repaint();
                true
            }
            Event::MouseMove { pos, .. } if self.drag_to(pos, cx) => true,
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
    use crate::event::Modifiers;
    use crate::sys::NativeWindow;

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

    /// An Explorer laid out on a tree, and the events a mouse would send.
    struct Mouse {
        ex: Explorer,
        text: TextSystem,
    }

    impl Mouse {
        fn new(root: &Path) -> Self {
            let text = TextSystem::new().expect("DirectWrite");
            let mut ex = Explorer::default();
            ex.set_root(root).unwrap();
            ex.layout(RectF::new(0.0, 0.0, 300.0, 400.0), &text);
            Self { ex, text }
        }

        fn at(&self, name: &str) -> PointF {
            let row = names(&self.ex).iter().position(|n| *n == name).unwrap();
            PointF::new(50.0, self.ex.list.y + row as f32 * ROW_H + ROW_H / 2.0)
        }

        fn send(&mut self, e: Event) -> Vec<Action> {
            let mut cx = Cx::new(&self.text, NativeWindow::none());
            self.ex.on_event(&e, &mut cx);
            cx.take_actions()
        }

        fn press(&mut self, pos: PointF) {
            self.send(Event::MouseDown {
                pos,
                button: MouseButton::Left,
                mods: Modifiers::NONE,
                clicks: 1,
            });
        }

        fn drag(&mut self, pos: PointF) {
            self.send(Event::MouseMove {
                pos,
                mods: Modifiers::NONE,
                left_down: true,
            });
        }

        fn release(&mut self, pos: PointF) -> Vec<Action> {
            self.send(Event::MouseUp {
                pos,
                button: MouseButton::Left,
                mods: Modifiers::NONE,
            })
        }

        fn drag_and_drop(&mut self, from: &str, to: PointF) -> Vec<Action> {
            let start = self.at(from);
            self.press(start);
            self.drag(to);
            self.release(to)
        }
    }

    fn drag_tree(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        std::fs::write(dir.join("a.c"), "").unwrap();
        std::fs::write(dir.join("src/b.c"), "").unwrap();
        dir
    }

    #[test]
    fn dragging_moves_into_the_folder_under_the_pointer() {
        let dir = drag_tree("drag-file");
        let mut m = Mouse::new(&dir);
        assert_eq!(names(&m.ex), ["lib", "src", "a.c"]);
        let lib = m.at("lib");
        assert_eq!(
            m.drag_and_drop("a.c", lib),
            [Action::MovePath {
                from: dir.join("a.c"),
                to: dir.join("lib").join("a.c"),
            }]
        );

        // Onto a file: into the folder holding it, which here it is already.
        let a = m.at("a.c");
        assert_eq!(m.drag_and_drop("lib", a), [], "lib is already at the top");
        // A folder can't go into itself.
        let src = m.at("src");
        assert_eq!(m.drag_and_drop("src", src), []);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dragging_to_empty_space_moves_to_the_top_folder() {
        let dir = drag_tree("drag-top");
        let mut m = Mouse::new(&dir);
        // Open src: the press alone must not, its release does.
        let src = m.at("src");
        m.press(src);
        assert_eq!(names(&m.ex), ["lib", "src", "a.c"], "not on press");
        m.release(src);
        assert_eq!(names(&m.ex), ["lib", "src", "inner", "b.c", "a.c"]);

        let below = PointF::new(50.0, m.ex.list.y + 10.0 * ROW_H);
        assert_eq!(
            m.drag_and_drop("b.c", below),
            [Action::MovePath {
                from: dir.join("src").join("b.c"),
                to: dir.join("b.c"),
            }]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_drag_does_not_open_a_folder_and_can_be_cancelled() {
        let dir = drag_tree("drag-cancel");
        let mut m = Mouse::new(&dir);
        let lib = m.at("lib");
        assert_eq!(m.drag_and_drop("src", lib).len(), 1);
        assert_eq!(names(&m.ex), ["lib", "src", "a.c"], "src stayed closed");

        let start = m.at("a.c");
        m.press(start);
        m.drag(lib);
        m.send(Event::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
        });
        assert_eq!(m.release(lib), [], "Esc cancelled it");

        // Outside the list there is nowhere to drop.
        let outside = PointF::new(50.0, 500.0);
        assert_eq!(m.drag_and_drop("a.c", outside), []);

        // A small wobble is still a click, not a drag.
        m.press(start);
        m.drag(PointF::new(start.x + 2.0, start.y));
        assert_eq!(m.release(start), []);
        assert!(m.ex.press.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn name_box_targets_the_selections_folder() {
        let dir = temp_dir("namebox");
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("sub/deep/f.txt"), "").unwrap();
        let mut ex = Explorer::default();
        ex.set_root(&dir).unwrap();

        // Revealing expands the way down and selects.
        let file = dir.join("sub/deep/f.txt");
        ex.reveal(&file);
        assert_eq!(ex.selected_path(), Some(file.as_path()));

        // A new item goes beside a selected file.
        ex.begin_new(false, None);
        let edit = ex.name_edit.as_mut().unwrap();
        assert!(edit.commit().is_err(), "empty name");
        edit.field.insert(" a.c ");
        assert_eq!(
            edit.commit(),
            Ok(Some(Action::CreatePath {
                path: dir.join("sub/deep").join("a.c"),
                folder: false,
            }))
        );
        edit.field.select_all();
        edit.field.insert("a/b");
        assert!(edit.commit().is_err(), "separator");

        // Renaming selects the name before the extension.
        ex.begin_rename(&file);
        let edit = ex.name_edit.as_mut().unwrap();
        assert_eq!(edit.field.selected_text(), "f");
        assert_eq!(edit.commit(), Ok(None), "unchanged");
        edit.field.insert("g");
        assert_eq!(
            edit.commit(),
            Ok(Some(Action::RenamePath {
                from: file.clone(),
                to: dir.join("sub/deep").join("g.txt"),
            }))
        );
        ex.finish_name_edit();
        assert!(ex.name_edit.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn refresh_follows_disk_and_keeps_state() {
        let root = temp_dir("refresh");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        std::fs::write(root.join("old.txt"), "").unwrap();

        let mut ex = Explorer::default();
        ex.set_root(&root).unwrap();
        let src = ex.rows[0];
        ex.load_children(src).unwrap();
        ex.entries[src].expanded = true;
        ex.rebuild_rows();
        ex.selected = Some(ex.rows[1]);
        assert_eq!(names(&ex), ["src", "a.rs", "old.txt"]);
        assert!(!ex.refresh(None));

        std::fs::write(root.join("src/b.rs"), "").unwrap();
        std::fs::remove_file(root.join("old.txt")).unwrap();
        std::fs::create_dir(root.join("docs")).unwrap();
        let dirs = HashSet::from([root.clone(), root.join("src")]);
        assert!(ex.refresh(Some(&dirs)));
        // `src` stays open and `a.rs` stays selected.
        assert_eq!(names(&ex), ["docs", "src", "a.rs", "b.rs"]);
        assert_eq!(
            ex.selected.map(|s| ex.entries[s].name.as_str()),
            Some("a.rs")
        );
        assert_eq!(ex.entries.len(), 5);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cpp_object_lists_user_symbols_and_sets_generated_aside() {
        let root = temp_dir("cpp");
        let obj = root.join("shapes.o");
        std::fs::copy(fixture("mingw_cpp.o"), &obj).unwrap();
        let mut ex = Explorer::default();
        ex.set_root(&root).unwrap();
        let graph = engine::load(&obj).unwrap();
        assert!(ex.apply_object(&obj, Ok(&graph)));
        let obj_idx = ex.rows[0];
        ex.entries[obj_idx].expanded = true;
        ex.rebuild_rows();

        let groups: Vec<&str> = ex.entries[obj_idx]
            .children
            .iter()
            .flatten()
            .map(|&g| ex.entries[g].name.as_str())
            .collect();
        assert_eq!(
            groups,
            [
                "Functions (8)",
                "Variables (2)",
                "Externals (4)",
                "Compiler-generated (14)"
            ],
        );
        let vars = ex.entries[obj_idx].children.as_ref().unwrap()[1];
        let var_names: Vec<&str> = ex.entries[vars]
            .children
            .iter()
            .flatten()
            .map(|&v| ex.entries[v].name.as_str())
            .collect();
        assert_eq!(var_names, ["total", "unit"]);

        // Open main, reload: it stays open and shows its locals.
        let main = ex
            .rows
            .iter()
            .copied()
            .find(|&i| ex.entries[i].name == "main")
            .unwrap();
        ex.entries[main].expanded = true;
        assert!(ex.apply_object(&obj, Ok(&graph)));
        ex.rebuild_rows();
        let rows = names(&ex);
        let at = rows.iter().position(|&n| n == "main").unwrap();
        assert_eq!(&rows[at + 1..at + 4], ["count", "sq", "sum"]);

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

        // Functions with debug info expand into their typed locals.
        let callee = ex
            .rows
            .iter()
            .copied()
            .find(|&i| ex.entries[i].name == "callee")
            .unwrap();
        assert!(ex.entries[callee].expandable());
        ex.entries[callee].expanded = true;
        ex.rebuild_rows();
        let at = names(&ex).iter().position(|&n| n == "callee").unwrap();
        assert_eq!(&names(&ex)[at + 1..at + 4], ["a", "b", "sum"]);
        let a = ex.rows[at + 1];
        assert_eq!(ex.entries[a].detail.as_deref(), Some("int"));

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
