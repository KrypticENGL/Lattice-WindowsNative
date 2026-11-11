//! Root of the UI: the title bar with its menus, the top strip
//! (Code/Visualizer/Debugger workspaces), each workspace's dock area, focus and input
//! routing.

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::build::{self, BuildEvent, Builder};
use crate::callstack::CallStack;
use crate::codepane::CodePane;
use crate::dataview::DataPanel;
use crate::debug::{self, Debug, Phase, Update};
use crate::debugview::{DebugStatus, DebugView};
use crate::dock::{Axis, DockArea, Drop, Node, PaneHost, PaneId, Request, Spot, TabDrag, Zone};
use crate::editor::EditorArea;
use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::explorer::Explorer;
use crate::fileops;
use crate::folder_watch::{Changes, FolderWatcher};
use crate::geom::{PointF, RectF, SizeF};
use crate::memory::Memory;
use crate::menubar::{Entry, Mark, MenuBar, Outcome};
use crate::objects::{LoadResult, ObjectEvent, ObjectService};
use crate::output::Output;
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::replay::{self, Replay, ReplayAction};
use crate::sys::clipboard;
use crate::sys::dialog::{self, ComGuard};
use crate::sys::gfx::{DrawCtx, Frame, Graphics};
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::{NativeWindow, Waker};
use crate::terminal::Terminal;
use crate::theme::Theme;
use crate::titlebar::{TitleBar, WindowHit, TITLE_BAR_H};
use crate::variables::Variables;
use crate::visualizer::Visualizer;
use crate::Error;
use renderer::GraphMode;

const TOP_STRIP_H: f32 = 34.0;
const TOP_TAB_PAD: f32 = 16.0;

/// What a menu row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuCmd {
    File(FileCmd),
    TogglePane(PaneId),
    GraphMode(GraphMode),
    Build,
    CompilerOptions,
}

/// The File menu's commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileCmd {
    NewFile,
    NewFolder,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    SaveAll,
    Rename,
    Delete,
    CloseFile,
    CloseAll,
    Exit,
}

/// The menu bar's titles, in order.
const MENUS: [&str; 4] = ["File", "Panels", "Build", "Visualizers"];

/// What the app should do after a call into the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Response {
    /// Invalidate the window so `paint` runs.
    pub repaint: bool,
}

/// The workspaces, each with its own dock layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopTab {
    Code,
    Visualizer,
    Debugger,
}

impl TopTab {
    const ALL: [Self; 3] = [Self::Code, Self::Visualizer, Self::Debugger];

    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            Self::Code => "Code",
            Self::Visualizer => "Visualizer",
            Self::Debugger => "Debugger",
        }
    }
}

/// Who receives mouse events between a press and its release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseOwner {
    TopStrip,
    Dock,
}

/// Every panel, wherever it is docked.
struct Panes {
    explorer: Explorer,
    editor: EditorArea,
    output: Output,
    terminal: Terminal,
    visualizer: Visualizer,
    data: DataPanel,
    debug_view: DebugView,
    variables: Variables,
    stack: CallStack,
    memory: Memory,
    vis_source: CodePane,
    vis_stack: CallStack,
    vis_memory: Memory,
}

impl PaneHost for Panes {
    fn pane(&mut self, id: PaneId) -> &mut dyn Panel {
        match id {
            PaneId::Explorer => &mut self.explorer,
            PaneId::Editor => &mut self.editor,
            PaneId::Output => &mut self.output,
            PaneId::Terminal => &mut self.terminal,
            PaneId::Visualizer => &mut self.visualizer,
            PaneId::Data => &mut self.data,
            PaneId::Debugger => &mut self.debug_view,
            PaneId::Variables => &mut self.variables,
            PaneId::CallStack => &mut self.stack,
            PaneId::Memory => &mut self.memory,
            PaneId::VisSource => &mut self.vis_source,
            PaneId::VisStack => &mut self.vis_stack,
            PaneId::VisMemory => &mut self.vis_memory,
        }
    }
}

/// The layouts a fresh install starts with.
fn default_tree(tab: TopTab) -> Node {
    use PaneId::*;
    match tab {
        TopTab::Code => Node::split(
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
        ),
        TopTab::Visualizer => Node::split(
            Axis::Horizontal,
            vec![
                (Node::tabs(&[Visualizer, Data]), 0.72),
                (
                    Node::split(
                        Axis::Vertical,
                        vec![
                            (Node::tabs(&[VisSource]), 0.6),
                            (Node::tabs(&[VisStack, VisMemory]), 0.4),
                        ],
                    ),
                    0.28,
                ),
            ],
        ),
        TopTab::Debugger => Node::split(
            Axis::Horizontal,
            vec![
                (Node::tabs(&[Debugger]), 0.72),
                (
                    Node::split(
                        Axis::Vertical,
                        vec![
                            (Node::tabs(&[Variables]), 0.6),
                            (Node::tabs(&[CallStack, Memory]), 0.4),
                        ],
                    ),
                    0.28,
                ),
            ],
        ),
    }
}

/// Where a panel goes when shown with no remembered spot.
fn default_spot(pane: PaneId) -> (TopTab, Spot) {
    use PaneId::*;
    let spot = |anchor, zone, share| Spot {
        anchor,
        zone,
        share,
    };
    match pane {
        Explorer => (TopTab::Code, spot(None, Zone::Left, 0.2)),
        Editor => (TopTab::Code, spot(None, Zone::Center, 1.0)),
        Output | Terminal => (TopTab::Code, spot(Some(Editor), Zone::Bottom, 0.3)),
        Visualizer => (TopTab::Visualizer, spot(None, Zone::Center, 1.0)),
        Data => (
            TopTab::Visualizer,
            spot(Some(Visualizer), Zone::Center, 1.0),
        ),
        Debugger => (TopTab::Debugger, spot(None, Zone::Center, 1.0)),
        Variables | CallStack | Memory => {
            (TopTab::Debugger, spot(Some(Debugger), Zone::Right, 0.28))
        }
        VisSource | VisStack | VisMemory => (
            TopTab::Visualizer,
            spot(Some(Visualizer), Zone::Right, 0.28),
        ),
    }
}

/// Everything except the device-independent resources the context borrows.
struct Views {
    tab: TopTab,
    strip: RectF,
    tab_rects: [RectF; 3],
    tab_hover: Option<usize>,
    title_bar: TitleBar,
    menu_bar: MenuBar<MenuCmd>,
    /// The open right-click menu.
    popup: MenuBar<Action>,
    /// The whole client area, which right-click menus stay inside.
    bounds: RectF,
    /// Where the menu titles end.
    menu_end: f32,
    areas: [DockArea; 3],
    /// Where each closed panel was, so showing it puts it back.
    closed: HashMap<PaneId, (TopTab, Spot)>,
    /// A tab being pressed or dragged, in either workspace.
    drag: Option<TabDrag>,
    panes: Panes,
    owner: Option<MouseOwner>,
    window_focused: bool,
    objects: ObjectService,
    folders: FolderWatcher,
    debug: Debug,
    build: Builder,
    /// The last recorded run, played back in the Data view.
    replay: Option<Replay>,
    /// Times the replay's steps.
    waker: Waker,
}

impl Views {
    fn area(&mut self) -> &mut DockArea {
        &mut self.areas[self.tab.index()]
    }

    fn workspace_of(&self, pane: PaneId) -> Option<TopTab> {
        TopTab::ALL
            .into_iter()
            .find(|t| self.areas[t.index()].contains(pane))
    }

    /// Docks a closed panel where it was (or its default spot).
    fn show_pane(&mut self, pane: PaneId) {
        if self.workspace_of(pane).is_some() {
            return;
        }
        let (tab, spot) = self
            .closed
            .remove(&pane)
            .unwrap_or_else(|| default_spot(pane));
        self.areas[tab.index()].tree.restore(pane, spot);
    }

    fn hide_pane(&mut self, pane: PaneId, cx: &mut Cx) {
        if !pane.closable() {
            return;
        }
        let Some(tab) = self.workspace_of(pane) else {
            return;
        };
        let area = &mut self.areas[tab.index()];
        area.leave(&mut self.panes, cx);
        if let Some(spot) = area.tree.remove(pane) {
            self.closed.insert(pane, (tab, spot));
        }
        cx.repaint();
    }

    fn toggle_pane(&mut self, pane: PaneId, cx: &mut Cx) {
        if self.workspace_of(pane).is_some() && pane.closable() {
            self.hide_pane(pane, cx);
        } else {
            self.reveal_pane(pane, cx);
        }
    }

    /// Shows a panel (docking it if closed), switches to its workspace and
    /// gives it the keyboard.
    fn reveal_pane(&mut self, pane: PaneId, cx: &mut Cx) {
        self.show_pane(pane);
        if let Some(tab) = self.workspace_of(pane) {
            self.switch_tab(tab, cx);
            self.areas[tab.index()].reveal(pane, cx);
        }
    }

    /// Gives a panel the keyboard if it is in the current workspace.
    fn focus_if_here(&mut self, pane: PaneId, cx: &mut Cx) {
        if self.area().contains(pane) {
            self.area().reveal(pane, cx);
        }
    }

    fn apply_request(&mut self, r: Request, cx: &mut Cx) {
        match r {
            Request::Close(pane) => self.hide_pane(pane, cx),
            Request::Move { pane, to } => {
                // Out of whichever workspace it was in, into the current one.
                if let Some(from) = self.workspace_of(pane) {
                    self.areas[from.index()].tree.remove(pane);
                }
                let (anchor, zone) = match to {
                    Drop::Group { anchor, zone } => (Some(anchor), zone),
                    Drop::Edge(zone) => (None, zone),
                };
                let area = self.area();
                area.tree.insert(pane, anchor, zone);
                area.reveal(pane, cx);
            }
        }
        cx.repaint();
    }

    fn layout(&mut self, size: SizeF, text: &TextSystem) {
        let building = self.build.is_building();
        self.panes
            .visualizer
            .set_build_state(building, self.debug.phase() == Phase::Idle && !building);
        self.panes.debug_view.set_status(DebugStatus {
            phase: self.debug.phase(),
            program: self
                .debug
                .program()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            stopped: self.debug.stop_description(),
            recording: self.debug.recording(),
            replay: self.replay.as_ref().map(Replay::status),
            building,
        });
        // The source follows the frame selected in the call stack.
        let target = self.panes.debug_view.code_target();
        self.panes.vis_source.show(target.clone());
        self.panes.debug_view.source.show(target);

        let full = RectF::new(0.0, 0.0, size.w, size.h);
        self.bounds = full;
        let (bar, rest) = full.split_top(TITLE_BAR_H);
        let menus = self.title_bar.layout(bar);
        self.menu_end = self.menu_bar.layout(menus, text);
        if self.menu_bar.is_open() {
            let menus = self.menus();
            self.menu_bar.refresh(text, |i| menus[i].clone());
        }
        let (strip, body) = rest.split_top(TOP_STRIP_H);
        self.strip = strip;
        let mut x = strip.x + 8.0;
        for t in TopTab::ALL {
            let w = text.measure(TextStyle::Ui, t.title()) + 2.0 * TOP_TAB_PAD;
            self.tab_rects[t.index()] = RectF::new(x, strip.y, w, strip.h);
            x += w;
        }
        for area in &mut self.areas {
            area.layout(body, text, &mut self.panes);
        }
    }

    /// Each menu's rows, with the current state.
    fn menus(&self) -> [Vec<Entry<MenuCmd>>; 4] {
        let has_folder = self.panes.explorer.root_path().is_some();
        let has_doc = self.panes.editor.has_documents();
        let has_item = self.panes.explorer.selected_path().is_some();
        let row = |cmd, label: &str, hint: &str, enabled: bool| {
            Entry::item(MenuCmd::File(cmd), label, hint, enabled)
        };
        let file = vec![
            row(FileCmd::NewFile, "New file\u{2026}", "Ctrl+N", true),
            row(
                FileCmd::NewFolder,
                "New folder\u{2026}",
                "Ctrl+Shift+N",
                has_folder,
            ),
            Entry::Separator,
            row(FileCmd::OpenFile, "Open file\u{2026}", "Ctrl+Shift+O", true),
            row(FileCmd::OpenFolder, "Open folder\u{2026}", "Ctrl+O", true),
            Entry::Separator,
            row(FileCmd::Save, "Save", "Ctrl+S", has_doc),
            row(FileCmd::SaveAs, "Save as\u{2026}", "Ctrl+Shift+S", has_doc),
            row(FileCmd::SaveAll, "Save all", "", has_doc),
            Entry::Separator,
            row(FileCmd::Rename, "Rename\u{2026}", "F2", has_item),
            row(FileCmd::Delete, "Delete", "Del", has_item),
            Entry::Separator,
            row(FileCmd::CloseFile, "Close file", "Ctrl+W", has_doc),
            row(FileCmd::CloseAll, "Close all files", "", has_doc),
            Entry::Separator,
            row(FileCmd::Exit, "Exit", "Alt+F4", true),
        ];
        let panels = PaneId::ALL
            .into_iter()
            .map(|p| {
                let ws = self.workspace_of(p);
                Entry::Item {
                    cmd: MenuCmd::TogglePane(p),
                    label: p.title().into(),
                    mark: Mark::Dot(ws.is_some()),
                    hint: Some(ws.map_or("closed", TopTab::title).into()),
                    enabled: true,
                }
            })
            .collect();
        let build = vec![
            Entry::item(
                MenuCmd::Build,
                "Build",
                "",
                has_folder && self.debug.phase() == Phase::Idle && !self.build.is_building(),
            ),
            Entry::item(
                MenuCmd::CompilerOptions,
                "Compiler options\u{2026}",
                "",
                has_folder,
            ),
        ];
        let current = self.panes.visualizer.graph_mode();
        let modes = GraphMode::ALL
            .into_iter()
            .map(|m| Entry::Item {
                cmd: MenuCmd::GraphMode(m),
                label: m.title().into(),
                mark: Mark::Radio(m == current),
                hint: Some(
                    match m {
                        GraphMode::Abstracted => "your code only",
                        GraphMode::Verbose => "everything",
                    }
                    .into(),
                ),
                enabled: true,
            })
            .collect();
        let visualizers = vec![
            Entry::Header("Call graph".into()),
            Entry::Submenu {
                label: "Mode".into(),
                hint: Some(current.title().into()),
                entries: modes,
            },
        ];
        [file, panels, build, visualizers]
    }

    fn run_menu(&mut self, cmd: MenuCmd, cx: &mut Cx) {
        match cmd {
            MenuCmd::File(c) => self.file_cmd(c, cx),
            MenuCmd::TogglePane(p) => self.toggle_pane(p, cx),
            MenuCmd::GraphMode(m) => {
                self.panes.visualizer.set_graph_mode(m, cx);
                cx.log(format!("Call graph mode: {}", m.title()));
            }
            MenuCmd::Build => self.debug_action(DebugAction::Build, cx),
            MenuCmd::CompilerOptions => self.open_compiler_options(cx),
        }
        cx.repaint();
    }

    /// Opens the folder's compiler options in the editor, first writing the
    /// defaults if it has none.
    fn open_compiler_options(&mut self, cx: &mut Cx) {
        let Some(root) = self.panes.explorer.root_path() else {
            cx.log_error("Open a folder first (Ctrl+O), then set its compiler options");
            return;
        };
        match build::ensure_options_file(root) {
            Ok(path) => cx.act(Action::OpenFile(path)),
            Err(e) => cx.log_error(format!("Could not create the compiler options: {e}")),
        }
    }

    fn file_cmd(&mut self, cmd: FileCmd, cx: &mut Cx) {
        match cmd {
            FileCmd::NewFile => self.new_entry(false, None, cx),
            FileCmd::NewFolder => self.new_entry(true, None, cx),
            FileCmd::OpenFile => match dialog::pick_file(cx.window, "All files (*.*)", "*.*") {
                Ok(Some(path)) => cx.act(Action::OpenFile(path)),
                Ok(None) => {}
                Err(e) => cx.log_error(format!("File picker failed: {e}")),
            },
            FileCmd::OpenFolder => cx.act(Action::PickFolder),
            FileCmd::Save => self.panes.editor.save_active(cx),
            FileCmd::SaveAs => self.save_as(cx),
            FileCmd::SaveAll => self.panes.editor.save_all(cx),
            FileCmd::Rename => {
                if let Some(p) = self.panes.explorer.selected_path() {
                    cx.act(Action::BeginRename(p.to_path_buf()));
                }
            }
            FileCmd::Delete => {
                if let Some(p) = self.panes.explorer.selected_path() {
                    cx.act(Action::DeletePath(p.to_path_buf()));
                }
            }
            FileCmd::CloseFile => self.panes.editor.close_active(cx),
            FileCmd::CloseAll => self.panes.editor.close_tabs_except(None, cx),
            FileCmd::Exit => cx.window.close(),
        }
        cx.repaint();
    }

    /// Asks for a new item's name in the Explorer. With no folder open there
    /// is no tree to put it in, so a new file picks its place in a dialog.
    fn new_entry(&mut self, folder: bool, dir: Option<PathBuf>, cx: &mut Cx) {
        if self.panes.explorer.root_path().is_some() {
            self.panes.explorer.begin_new(folder, dir);
            self.reveal_pane(PaneId::Explorer, cx);
        } else if folder {
            cx.log_error("Open a folder first (Ctrl+O), then add a folder to it");
        } else {
            match dialog::pick_save(cx.window, "untitled.cpp", None) {
                Ok(Some(path)) => {
                    // The dialog already asked about replacing a file; this
                    // never empties one, it just opens what is there.
                    if let Err(e) = fileops::create(&path, false) {
                        if e.kind() != std::io::ErrorKind::AlreadyExists {
                            cx.log_error(format!("Could not create {}: {e}", path.display()));
                            return;
                        }
                    }
                    cx.act(Action::OpenFile(path));
                }
                Ok(None) => {}
                Err(e) => cx.log_error(format!("Save dialog failed: {e}")),
            }
        }
    }

    fn save_as(&mut self, cx: &mut Cx) {
        let Some(path) = self.panes.editor.active_path() else {
            return;
        };
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let folder = path.parent().map(Path::to_path_buf);
        match dialog::pick_save(cx.window, &name, folder.as_deref()) {
            Ok(Some(to)) => self.panes.editor.save_active_as(&to, cx),
            Ok(None) => {}
            Err(e) => cx.log_error(format!("Save dialog failed: {e}")),
        }
    }

    fn create_path(&mut self, path: &Path, folder: bool, cx: &mut Cx) {
        if let Err(e) = fileops::create(path, folder) {
            self.panes.explorer.name_edit_failed(e.to_string());
            cx.repaint();
            return;
        }
        cx.log(format!("Created {}", path.display()));
        let explorer = &mut self.panes.explorer;
        explorer.finish_name_edit();
        explorer.refresh(None);
        explorer.reveal(path);
        if !folder {
            cx.act(Action::OpenFile(path.to_path_buf()));
        }
        cx.repaint();
    }

    fn rename_path(&mut self, from: &Path, to: &Path, cx: &mut Cx) {
        if let Err(e) = fileops::rename(from, to) {
            self.panes.explorer.name_edit_failed(e.to_string());
            cx.repaint();
            return;
        }
        cx.log(format!("Renamed {} to {}", from.display(), to.display()));
        self.panes.editor.path_renamed(from, to, cx);
        let explorer = &mut self.panes.explorer;
        explorer.finish_name_edit();
        explorer.refresh(None);
        explorer.reveal(to);
        cx.repaint();
    }

    /// Open files follow the move; if something of that name is already
    /// there, nothing happens but the message.
    fn move_path(&mut self, from: &Path, to: &Path, cx: &mut Cx) {
        if let Err(e) = fileops::rename(from, to) {
            cx.log_error(format!("Could not move {}: {e}", from.display()));
            return;
        }
        cx.log(format!("Moved {} to {}", from.display(), to.display()));
        self.panes.editor.path_renamed(from, to, cx);
        let explorer = &mut self.panes.explorer;
        explorer.refresh(None);
        explorer.reveal(to);
        cx.repaint();
    }

    /// Asks first; on Yes moves the item to the Recycle Bin and closes any of
    /// its files that are open.
    fn delete_path(&mut self, path: &Path, cx: &mut Cx) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let unsaved = self.panes.editor.has_unsaved_under(path);
        if !dialog::confirm_delete(cx.window, &name, unsaved) {
            return;
        }
        match dialog::recycle(cx.window, path) {
            Ok(true) => {
                cx.log(format!("Moved {} to the Recycle Bin", path.display()));
                self.panes.editor.forget_under(path, cx);
                self.panes.explorer.refresh(None);
            }
            Ok(false) => cx.log_error(format!("{} was not deleted", path.display())),
            Err(e) => cx.log_error(format!("Could not delete {}: {e}", path.display())),
        }
        cx.repaint();
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme) {
        let title = match self.panes.explorer.root_path() {
            Some(root) => format!("{} \u{2014} Lattice", root.display()),
            None => "Lattice".to_string(),
        };
        self.title_bar.paint(d, theme, &title, self.menu_end);
        self.menu_bar.paint_bar(d, theme);
        d.fill_rect(self.strip, theme.chrome_bg);
        for t in TopTab::ALL {
            let i = t.index();
            let r = self.tab_rects[i];
            let active = t == self.tab;
            if !active && self.tab_hover == Some(i) {
                d.fill_rect(r, theme.hover_bg);
            }
            let color = if active {
                theme.text_bright
            } else {
                theme.text_dim
            };
            d.text(
                TextStyle::Ui,
                t.title(),
                RectF::new(r.x + TOP_TAB_PAD, r.y, r.w - TOP_TAB_PAD, r.h),
                color,
            );
            if active {
                d.fill_rect(
                    RectF::new(r.x + 4.0, r.bottom() - 2.0, r.w - 8.0, 2.0),
                    theme.accent,
                );
            }
        }
        d.fill_rect(
            RectF::new(self.strip.x, self.strip.bottom() - 1.0, self.strip.w, 1.0),
            theme.border,
        );

        let debug = &self.debug;
        let p = &mut self.panes;
        p.editor.set_marks(|path| debug.marks_for(path));
        let stale = debug.phase() != Phase::Stopped && self.replay.is_none();
        for source in [&mut p.debug_view.source, &mut p.vis_source] {
            let lines = source.path().map(|path| debug.marks_for(path).0);
            source.set_breakpoints(lines.unwrap_or_default());
            source.set_stale(stale);
        }
        let frame = p.debug_view.selected_frame();
        p.stack.set_view(frame, stale);
        p.vis_stack.set_view(frame, stale);
        p.variables.set_view(frame, stale);

        let i = self.tab.index();
        self.areas[i].paint(d, theme, &mut self.panes, self.window_focused);
        self.menu_bar.paint_menus(d, theme);
        self.popup.paint_menus(d, theme);
    }

    /// What the window procedure should treat `p` as: menu titles are
    /// the UI's, the rest of the title bar drags the window.
    fn hit_test(&self, p: PointF) -> WindowHit {
        match self.title_bar.hit(p) {
            WindowHit::Caption if self.menu_bar.over_title(p) => WindowHit::Client,
            hit => hit,
        }
    }

    /// The Visualizer canvas is on screen.
    fn visualizer_shown(&self) -> bool {
        self.areas[self.tab.index()].is_shown(PaneId::Visualizer)
    }

    /// The Data canvas is on screen and mid-transition.
    fn data_animating(&self) -> bool {
        self.areas[self.tab.index()].is_shown(PaneId::Data) && self.panes.data.is_animating()
    }

    fn switch_tab(&mut self, tab: TopTab, cx: &mut Cx) {
        if self.tab != tab {
            let old = self.tab.index();
            self.areas[old].leave(&mut self.panes, cx);
            self.areas[old].clear_preview();
            self.tab = tab;
            cx.repaint();
        }
    }

    /// The open right-click menu: returns whether it took the event.
    fn popup_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if !self.popup.is_open() {
            return false;
        }
        // Typing closes the menu and reaches the panel.
        let typed = !matches!(
            e,
            Event::KeyDown {
                key: Key::Escape,
                ..
            }
        ) && matches!(e, Event::KeyDown { .. } | Event::Char(_));
        if typed {
            self.popup.close();
            cx.repaint();
            return false;
        }
        match self.popup.on_event(e, cx.text, |_| Vec::new()) {
            Outcome::Ignored => {
                cx.repaint();
                false
            }
            Outcome::Consumed => {
                if e.pos().is_some() {
                    self.areas[self.tab.index()].leave(&mut self.panes, cx);
                }
                cx.repaint();
                true
            }
            Outcome::Picked(action) => {
                cx.act(action);
                cx.repaint();
                true
            }
        }
    }

    /// The menu bar and its open menus: returns whether they took the event.
    fn menu_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let menus = self.menus();
        let before = self.menu_bar.state();
        match self.menu_bar.on_event(e, cx.text, |i| menus[i].clone()) {
            Outcome::Ignored => {
                if before != self.menu_bar.state() {
                    cx.repaint();
                }
                false
            }
            Outcome::Consumed => {
                if let Some(pos) = e.pos() {
                    self.areas[self.tab.index()].leave(&mut self.panes, cx);
                    if self.menu_bar.over_title(pos) {
                        cx.cursor = CursorShape::Hand;
                    }
                }
                cx.repaint();
                true
            }
            Outcome::Picked(cmd) => {
                self.run_menu(cmd, cx);
                true
            }
        }
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) {
        if let Event::Focus(focused) = *e {
            self.window_focused = focused;
            self.popup.close();
            cx.repaint();
            return;
        }
        if let (
            Event::KeyDown {
                key: Key::Escape, ..
            },
            Some(_),
        ) = (e, self.drag)
        {
            self.drag = None;
            self.area().clear_preview();
            cx.repaint();
            return;
        }
        if self.popup_event(e, cx) || self.menu_event(e, cx) {
            return;
        }
        if self.global_key(e, cx) {
            return;
        }
        // The top strip is shared chrome; below it is the current
        // workspace's dock area.
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if self.strip.contains(pos) => {
                self.owner = Some(MouseOwner::TopStrip);
                if let Some(i) = self.tab_rects.iter().position(|r| r.contains(pos)) {
                    self.switch_tab(TopTab::ALL[i], cx);
                }
                return;
            }
            Event::MouseDown { pos, .. } if self.strip.contains(pos) => return,
            Event::MouseDown { .. } => self.owner = Some(MouseOwner::Dock),
            Event::MouseMove { pos, .. } => {
                let over_strip = self.strip.contains(pos);
                if self.drag.is_some_and(|d| d.active) {
                    // Dragging a tab onto the other workspace's title
                    // switches to it, so the panel can be dropped there.
                    if let Some(i) = self.tab_rects.iter().position(|r| r.contains(pos)) {
                        self.switch_tab(TopTab::ALL[i], cx);
                    }
                } else if self.owner.is_none() {
                    let hover = self.tab_rects.iter().position(|r| r.contains(pos));
                    if hover != self.tab_hover {
                        self.tab_hover = hover;
                        cx.repaint();
                    }
                    if hover.is_some() {
                        cx.cursor = CursorShape::Hand;
                    }
                    if over_strip {
                        self.areas[self.tab.index()].leave(&mut self.panes, cx);
                        return;
                    }
                }
                if self.owner == Some(MouseOwner::TopStrip) {
                    return;
                }
            }
            Event::MouseLeave if self.tab_hover.take().is_some() => cx.repaint(),
            Event::MouseUp { .. } => {
                let strip = self.owner.take() == Some(MouseOwner::TopStrip);
                if strip && self.drag.is_none() {
                    return;
                }
            }
            _ => {}
        }
        let i = self.tab.index();
        if let Some(r) = self.areas[i].on_event(e, &mut self.panes, &mut self.drag, cx) {
            self.apply_request(r, cx);
        }
    }

    /// Shortcuts that work regardless of focus. Returns `true` if handled.
    fn global_key(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let Event::KeyDown { key, mods } = *e else {
            return false;
        };
        // Debugger keys (Visual Studio's), in every tab.
        if !mods.ctrl && !mods.alt {
            let action = match key {
                Key::F(5) if mods.shift => Some(DebugAction::Stop),
                Key::F(6) => Some(DebugAction::Play),
                Key::F(5) => Some(DebugAction::Run),
                Key::F(10) => Some(DebugAction::Step(debugger::Command::Next)),
                Key::F(11) if mods.shift => Some(DebugAction::Step(debugger::Command::Finish)),
                Key::F(11) => Some(DebugAction::Step(debugger::Command::Step)),
                Key::F(9) => {
                    if let Some((path, line)) = self.panes.editor.caret_location() {
                        self.debug.toggle_breakpoint(&path, line, cx);
                    }
                    return true;
                }
                _ => None,
            };
            if let Some(a) = action {
                self.debug_action(a, cx);
                return true;
            }
        }
        if !mods.ctrl || mods.alt {
            return false;
        }
        let in_terminal = self.area().focused() == Some(PaneId::Terminal);
        // Never closes a tab the user can't see.
        let editor_here = self.area().contains(PaneId::Editor);
        match key {
            Key::Letter('O') if !mods.shift => cx.act(Action::PickFolder),
            Key::Letter('O') => self.file_cmd(FileCmd::OpenFile, cx),
            Key::Letter('N') if mods.shift => self.file_cmd(FileCmd::NewFolder, cx),
            Key::Letter('S') if mods.shift => self.file_cmd(FileCmd::SaveAs, cx),
            // Without Shift these also mean something to programs in the
            // terminal, which keeps them while it has the keyboard.
            Key::Letter('N') if !in_terminal => self.file_cmd(FileCmd::NewFile, cx),
            Key::Letter('S') if !in_terminal => self.file_cmd(FileCmd::Save, cx),
            Key::Letter('W') if !in_terminal && editor_here => {
                self.file_cmd(FileCmd::CloseFile, cx);
            }
            Key::Digit(1) => self.switch_tab(TopTab::Code, cx),
            Key::Digit(2) => self.switch_tab(TopTab::Visualizer, cx),
            Key::Digit(3) => self.switch_tab(TopTab::Debugger, cx),
            Key::Backquote => {
                self.reveal_pane(PaneId::Terminal, cx);
                if !self.panes.terminal.is_running() {
                    self.panes.terminal.start(cx);
                }
            }
            _ => return false,
        }
        true
    }

    fn debug_action(&mut self, a: DebugAction, cx: &mut Cx) {
        match a {
            DebugAction::Play => self.replay_action(ReplayAction::Toggle, cx),
            DebugAction::Run => match self.debug.phase() {
                Phase::Stopped => self.debug.command(debugger::Command::Continue, cx),
                Phase::Idle => {
                    if !self.ensure_program(cx) {
                        return;
                    }
                    self.clear_data_view(cx);
                    self.debug.start(cx);
                }
                Phase::Starting | Phase::Running => {}
            },
            DebugAction::Pause => self.debug.command(debugger::Command::Interrupt, cx),
            DebugAction::Step(c) => self.debug.command(c, cx),
            DebugAction::Stop => {
                if self.debug.is_active() {
                    self.debug.stop(cx);
                    self.panes.data.set_stale(true);
                }
            }
            DebugAction::PickProgram => {
                self.pick_program(cx);
            }
            DebugAction::Build => match self.panes.explorer.root_path() {
                Some(_) if self.debug.phase() != Phase::Idle => {
                    cx.log_error("Stop the program before building it");
                }
                Some(root) => self.build.start(root.to_path_buf()),
                None => cx.log_error("Open a folder first (Ctrl+O), then build it"),
            },
        }
        cx.repaint();
    }

    /// Makes sure a program is chosen: `foo.exe` next to the open
    /// `foo.cpp`, else ask. `false` if cancelled.
    fn ensure_program(&mut self, cx: &mut Cx) -> bool {
        if self.debug.program().is_some() {
            return true;
        }
        let guess = self
            .panes
            .editor
            .caret_location()
            .and_then(|(p, _)| debug::program_for_source(&p));
        match guess {
            Some(p) => {
                self.debug.set_program(p);
                true
            }
            None => self.pick_program(cx),
        }
    }

    /// Empties the Data view, Source and Call stack for a new run, and
    /// drops the replay.
    fn clear_data_view(&mut self, cx: &mut Cx) {
        self.replay = None;
        // The Code tab stays put: its editor follows the stops. The call graph
        // shows nothing of a run, so go to where it can be seen.
        if self.tab == TopTab::Visualizer {
            self.switch_tab(TopTab::Debugger, cx);
        }
        let p = &mut self.panes;
        p.data.clear();
        p.debug_view.clear_snapshot();
        p.variables.clear();
        p.stack.clear();
        p.debug_view.source.clear();
        p.vis_stack.clear();
        p.vis_source.clear();
    }

    /// Play bar commands. With no replay (or on Record), records the
    /// program first; playback starts when the recording is done.
    fn replay_action(&mut self, a: ReplayAction, cx: &mut Cx) {
        if self.debug.recording().is_some() {
            return;
        }
        let now = Instant::now();
        match (&mut self.replay, a) {
            (Some(r), a) if a != ReplayAction::Record => {
                if r.apply(a, now) {
                    self.show_replay_step();
                }
                self.schedule_replay(now);
            }
            _ => {
                if !self.ensure_program(cx) {
                    return;
                }
                self.clear_data_view(cx);
                self.debug.record(cx);
            }
        }
        cx.repaint();
    }

    /// Shows the replay's current step in the Data view, Source and Call
    /// stack.
    fn show_replay_step(&mut self) {
        let Some(r) = &self.replay else {
            return;
        };
        let snap = r.snapshot().clone();
        self.show_snapshot(snap);
    }

    /// A stop's data to every panel that shows it.
    fn show_snapshot(&mut self, snap: debugger::Snapshot) {
        // shortcut: one deep copy per stop for the panels that share it;
        // make the Data view hold the Arc too if large snapshots get slow.
        let shared = Arc::new(snap.clone());
        let p = &mut self.panes;
        p.stack.set_frames(&snap.frames);
        p.vis_stack.set_frames(&snap.frames);
        p.data.set_snapshot(snap);
        p.variables.set_snapshot(shared.clone());
        p.debug_view.set_snapshot(shared.clone());
        let target = p.debug_view.code_target();
        p.debug_view.source.show(target.clone());
        p.debug_view.source.stopped();
        p.debug_view.source.set_snapshot(shared.clone());
        p.vis_source.show(target);
        p.vis_source.stopped();
        p.vis_source.set_snapshot(shared.clone());
        p.vis_memory.set_snapshot(shared.clone());
        p.editor.set_snapshot(shared.clone());
        p.memory.set_snapshot(shared);
    }

    /// Wakes the UI thread when the replay's next step is due.
    fn schedule_replay(&self, now: Instant) {
        if let Some(d) = self.replay.as_ref().and_then(|r| r.next_due(now)) {
            replay::wake_after(&self.waker, d);
        }
    }

    /// Advances a playing replay whose step is due.
    fn tick_replay(&mut self, cx: &mut Cx) {
        let now = Instant::now();
        if self.replay.as_mut().is_some_and(|r| r.tick(now)) {
            self.show_replay_step();
            self.schedule_replay(now);
            cx.repaint();
        }
    }

    /// Asks for the program to debug. `false` if cancelled.
    fn pick_program(&mut self, cx: &mut Cx) -> bool {
        match dialog::pick_file(cx.window, "Programs (*.exe)", "*.exe") {
            Ok(Some(p)) => {
                cx.log(format!("Program to debug: {}", p.display()));
                self.debug.set_program(p);
                cx.repaint();
                true
            }
            Ok(None) => false,
            Err(e) => {
                cx.log_error(format!("File picker failed: {e}"));
                false
            }
        }
    }

    /// Logs build output; a built program becomes the one to debug.
    fn drain_build(&mut self, cx: &mut Cx) {
        while let Some(event) = self.build.try_recv() {
            match event {
                BuildEvent::Log { error, text } => cx.act(Action::Log { error, text }),
                BuildEvent::Done(exe) => {
                    if let Some(exe) = exe {
                        self.debug.set_program(exe);
                    }
                    cx.repaint();
                }
            }
        }
    }

    /// Applies the debugger's events.
    fn drain_debug(&mut self, cx: &mut Cx) {
        for u in self.debug.drain(cx) {
            match u {
                Update::Running => self.panes.data.set_stale(true),
                Update::Stopped { file, line } => {
                    // With the editor in view, follow the stop like any
                    // debugger.
                    let here = self.areas[self.tab.index()].contains(PaneId::Editor);
                    if let (true, Some(file), Some(line)) = (here, file, line) {
                        if file.is_file() {
                            self.panes.editor.open_at(&file, line, cx);
                        }
                    }
                }
                Update::Snapshot(snap) => self.show_snapshot(*snap),
                Update::Ended => self.panes.data.set_stale(true),
                Update::Recording => {}
                Update::Recorded(steps) => {
                    let now = Instant::now();
                    self.replay = Replay::new(steps, now);
                    if self.replay.is_some() {
                        self.show_replay_step();
                        self.schedule_replay(now);
                    } else {
                        cx.log_error("Nothing to replay: no line of the program was recorded");
                    }
                }
            }
        }
    }

    fn open_folder(&mut self, path: &Path, cx: &mut Cx) {
        match self.panes.explorer.set_root(path) {
            Ok(()) => cx.log(format!("Opened folder {}", path.display())),
            Err(e) => cx.log_error(format!("Could not read folder: {e}")),
        }
        if let Some(warning) = self.folders.set_root(path) {
            cx.log_error(warning);
        }
        let p = &mut self.panes;
        p.visualizer
            .set_root(p.explorer.root_path().map(Path::to_path_buf));
        p.terminal.set_cwd(Some(path.to_path_buf()));
        if p.terminal.is_running() {
            p.terminal
                .run_command(&format!("cd \"{}\"", path.display()), cx);
        }
        self.focus_if_here(PaneId::Explorer, cx);
        cx.repaint();
    }

    /// Brings the Explorer up to date with changes in the open folder.
    fn drain_folders(&mut self, cx: &mut Cx) {
        while let Some(changes) = self.folders.try_recv() {
            let dirs = match &changes {
                Changes::Dirs(dirs) => Some(dirs),
                Changes::All => None,
            };
            if self.panes.explorer.refresh(dirs) {
                cx.repaint();
            }
        }
    }

    /// Shows finished object loads in the Explorer, the Visualizer and the
    /// Output panel.
    fn drain_objects(&mut self, cx: &mut Cx) {
        while let Some(event) = self.objects.try_recv() {
            let LoadResult {
                path,
                reload,
                quiet,
                elapsed,
                graph,
            } = match event {
                ObjectEvent::Loaded(result) => result,
                ObjectEvent::FolderDone {
                    root,
                    loaded,
                    failed,
                    elapsed,
                } => {
                    self.panes.visualizer.set_folder_loading(false);
                    let failed = if failed > 0 {
                        format!(", {failed} failed to load")
                    } else {
                        String::new()
                    };
                    cx.log(format!(
                        "Loaded {loaded} object files from {}{failed} in {:.1} s",
                        root.display(),
                        elapsed.as_secs_f32()
                    ));
                    continue;
                }
            };
            let file = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let shown = self
                .panes
                .explorer
                .apply_object(&path, graph.as_deref().map_err(|e| e.reason.as_str()));
            if let Ok(g) = &graph {
                self.panes.visualizer.add_graph(g.clone());
            }
            if quiet {
                if shown {
                    cx.repaint();
                }
                continue;
            }
            match &graph {
                Ok(g) => {
                    // Same classification as the Explorer groups.
                    let locals: std::collections::HashSet<engine::NodeId> = g
                        .edges()
                        .iter()
                        .filter(|e| e.kind == engine::EdgeKind::Contains)
                        .map(|e| e.to)
                        .collect();
                    let top = || g.nodes().iter().filter(|n| !locals.contains(&n.id));
                    let user = |k| {
                        top()
                            .filter(|n| n.kind == k && !n.artificial && !n.library)
                            .count()
                    };
                    let symbol = |n: &&engine::Node| {
                        matches!(
                            n.kind,
                            engine::NodeKind::Function
                                | engine::NodeKind::Variable
                                | engine::NodeKind::External
                        )
                    };
                    let library = top()
                        .filter(symbol)
                        .filter(|n| n.library && !n.artificial)
                        .count();
                    let generated = top().filter(symbol).filter(|n| n.artificial).count();
                    cx.log(format!(
                        "{} {file}: {} functions, {} variables, {} locals, {} externals \
                         (+{library} library, +{generated} compiler-generated; debug: {:?}) in {} ms",
                        if reload { "Reloaded" } else { "Loaded" },
                        user(engine::NodeKind::Function),
                        user(engine::NodeKind::Variable),
                        locals.len(),
                        user(engine::NodeKind::External),
                        g.debug,
                        elapsed.as_millis()
                    ));
                    for w in &g.warnings {
                        cx.log_error(format!("{file}: {w}"));
                    }
                }
                Err(e) => cx.log_error(format!("Load failed: {}", e.message)),
            }
            if shown {
                cx.repaint();
            }
        }
    }

    /// Applies queued actions until none remain (actions may queue more).
    fn process_actions(&mut self, cx: &mut Cx) {
        loop {
            let actions = cx.take_actions();
            if actions.is_empty() {
                return;
            }
            for action in actions {
                match action {
                    Action::OpenFile(path) => {
                        self.panes.editor.open(&path, cx);
                        if self.panes.editor.has_documents() {
                            self.reveal_pane(PaneId::Editor, cx);
                        }
                    }
                    Action::LoadObject(path) => {
                        if let Some(warning) = self.objects.load(&path) {
                            cx.log_error(warning);
                        }
                    }
                    Action::LoadFolderObjects => match self.panes.explorer.root_path() {
                        Some(root) => {
                            let root = root.to_path_buf();
                            cx.log(format!(
                                "Loading every object file under {}…",
                                root.display()
                            ));
                            self.panes.visualizer.set_folder_loading(true);
                            if let Some(warning) = self.objects.load_folder(&root) {
                                cx.log_error(warning);
                            }
                            cx.repaint();
                        }
                        None => cx.log_error("Open a folder first (Ctrl+O), then add its objects"),
                    },
                    Action::OpenSource { path, line } => {
                        self.panes.editor.open_at(&path, line, cx);
                        if self.panes.editor.has_documents() {
                            self.reveal_pane(PaneId::Editor, cx);
                        }
                    }
                    Action::Debug(a) => self.debug_action(a, cx),
                    Action::Replay(a) => self.replay_action(a, cx),
                    Action::ToggleBreakpoint { path, line } => {
                        self.debug.toggle_breakpoint(&path, line, cx);
                    }
                    Action::PickFolder => match dialog::pick_folder(cx.window) {
                        Ok(Some(path)) => self.open_folder(&path, cx),
                        Ok(None) => {}
                        Err(e) => cx.log_error(format!("Folder picker failed: {e}")),
                    },
                    Action::Log { error, text } => {
                        self.panes.output.push(error, &text);
                        cx.repaint();
                    }
                    Action::ShowMemory(target) => {
                        // The memory view of the tab the request came from.
                        if self.area().contains(PaneId::VisMemory) {
                            self.panes.vis_memory.show(target);
                            self.reveal_pane(PaneId::VisMemory, cx);
                        } else {
                            self.panes.memory.show(target);
                            self.reveal_pane(PaneId::Memory, cx);
                        }
                        cx.repaint();
                    }
                    Action::RevealFrame(i) => {
                        self.panes.debug_view.select_frame(i);
                        self.panes.data.reveal_frame(i);
                        cx.repaint();
                    }
                    Action::ContextMenu { pos, items } => {
                        self.menu_bar.close();
                        self.popup.popup(items, pos, self.bounds, cx.text);
                        cx.repaint();
                    }
                    Action::Key(key, mods) => {
                        let e = Event::KeyDown { key, mods };
                        let i = self.tab.index();
                        self.areas[i].on_event(&e, &mut self.panes, &mut self.drag, cx);
                        cx.repaint();
                    }
                    Action::CopyText(text) => {
                        if let Err(e) = clipboard::set_text(cx.window, &text) {
                            cx.log_error(format!("Copy failed: {e}"));
                        }
                    }
                    Action::Reveal(path) => {
                        // explorer.exe wants the quotes around the path only,
                        // so std's argument quoting cannot be used.
                        let spawned = std::process::Command::new("explorer")
                            .raw_arg(format!("/select,\"{}\"", path.display()))
                            .spawn();
                        if let Err(e) = spawned {
                            cx.log_error(format!("Could not open File Explorer: {e}"));
                        }
                    }
                    Action::CloseTab(i) => self.panes.editor.close_tab(i, cx),
                    Action::CloseTabsExcept(keep) => {
                        self.panes.editor.close_tabs_except(keep, cx);
                        cx.repaint();
                    }
                    Action::ClearOutput => {
                        self.panes.output.clear();
                        cx.repaint();
                    }
                    Action::NewEntry { folder, dir } => self.new_entry(folder, dir, cx),
                    Action::BeginRename(path) => {
                        self.panes.explorer.begin_rename(&path);
                        self.reveal_pane(PaneId::Explorer, cx);
                        cx.repaint();
                    }
                    Action::CreatePath { path, folder } => self.create_path(&path, folder, cx),
                    Action::RenamePath { from, to } => self.rename_path(&from, &to, cx),
                    Action::MovePath { from, to } => self.move_path(&from, &to, cx),
                    Action::DeletePath(path) => self.delete_path(&path, cx),
                }
            }
        }
    }
}

/// The whole IDE UI for one window.
pub struct Shell {
    window: NativeWindow,
    waker: Waker,
    gfx: Graphics,
    text: TextSystem,
    theme: Theme,
    views: Views,
    cursor: CursorShape,
    terminal_started: bool,
    // Last: COM must outlive every COM object above.
    _com: ComGuard,
}

impl Shell {
    /// Builds the UI for `window` at `dpi`, optionally opening a folder.
    pub fn new(window: NativeWindow, dpi: u32, folder: Option<PathBuf>) -> Result<Self, Error> {
        let com = ComGuard::init()?;
        let waker = Waker::new(window);
        let (objects, objects_error) = ObjectService::new(waker.clone());
        let (visualizer, layout_error) = Visualizer::new(waker.clone());
        let (folders, folders_error) = FolderWatcher::new(waker.clone());
        let mut shell = Self {
            window,
            gfx: Graphics::new(window, dpi)?,
            text: TextSystem::new()?,
            theme: Theme::dark(),
            views: Views {
                tab: TopTab::Code,
                strip: RectF::default(),
                tab_rects: [RectF::default(); 3],
                tab_hover: None,
                menu_bar: MenuBar::new(&MENUS),
                popup: MenuBar::new(&[]),
                bounds: RectF::default(),
                title_bar: TitleBar::default(),
                menu_end: 0.0,
                areas: TopTab::ALL.map(|t| DockArea::new(default_tree(t))),
                closed: HashMap::new(),
                drag: None,
                panes: Panes {
                    explorer: Explorer::default(),
                    editor: EditorArea::default(),
                    output: Output::new(),
                    terminal: Terminal::new(waker.clone()),
                    visualizer,
                    data: DataPanel::default(),
                    debug_view: DebugView::default(),
                    variables: Variables::default(),
                    stack: CallStack::default(),
                    memory: Memory::default(),
                    vis_source: CodePane::default(),
                    vis_stack: CallStack::default(),
                    vis_memory: Memory::default(),
                },
                owner: None,
                window_focused: true,
                objects,
                folders,
                debug: Debug::new(waker.clone()),
                build: Builder::new(waker.clone()),
                replay: None,
                waker: waker.clone(),
            },
            waker,
            cursor: CursorShape::Arrow,
            terminal_started: false,
            _com: com,
        };
        let mut cx = Cx::new(&shell.text, window);
        cx.log(format!(
            "Lattice started (editor font: {})",
            shell.text.mono_family()
        ));
        for e in [objects_error, layout_error, folders_error]
            .into_iter()
            .flatten()
        {
            cx.log_error(e);
        }
        if let Some(folder) = folder {
            shell.views.open_folder(&folder, &mut cx);
        }
        shell.views.process_actions(&mut cx);
        Ok(shell)
    }

    /// Appends a line to the Output panel.
    pub fn log(&mut self, error: bool, text: &str) -> Response {
        self.views.panes.output.push(error, text);
        Response { repaint: true }
    }

    /// Cursor for the pointer's current position.
    pub fn cursor(&self) -> CursorShape {
        self.cursor
    }

    /// Window client area resized, in pixels.
    pub fn resize(&mut self, width: u32, height: u32) -> Response {
        let mut cx = Cx::new(&self.text, self.window);
        if let Err(e) = self.gfx.resize(width, height) {
            cx.log_error(format!("Resize failed: {e}"));
        }
        let size = self.gfx.size();
        if size.w > 0.0 && size.h > 0.0 {
            self.views.layout(size, &self.text);
            if !self.terminal_started {
                self.terminal_started = true;
                self.views.panes.terminal.start(&mut cx);
            }
        }
        self.views.process_actions(&mut cx);
        Response { repaint: true }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.gfx.set_dpi(dpi);
        self.views.panes.editor.clear_caches();
    }

    pub fn on_event(&mut self, e: Event) -> Response {
        let mut cx = Cx::new(&self.text, self.window);
        if let Event::KeyDown {
            key: Key::F(12),
            mods,
        } = e
        {
            if cfg!(debug_assertions) && mods.ctrl && mods.shift {
                self.gfx.simulate_device_loss();
                cx.log("Debug: simulated GPU device loss; rebuilding device resources");
                self.views.process_actions(&mut cx);
                return Response { repaint: true };
            }
        }
        self.views.on_event(&e, &mut cx);
        if matches!(
            e,
            Event::MouseMove { .. } | Event::MouseDown { .. } | Event::MouseUp { .. }
        ) {
            self.cursor = cx.cursor;
        }
        self.views.process_actions(&mut cx);
        Response {
            repaint: cx.needs_repaint(),
        }
    }

    /// Opens the Visualizer and, once the graph has settled, measures frame
    /// times while panning and zooming; the result goes to the Output panel.
    /// `synthetic` picks a generated graph of that many nodes; `None` loads
    /// every object file under the open folder instead.
    pub fn benchmark(&mut self, synthetic: Option<usize>) -> Response {
        let mut cx = Cx::new(&self.text, self.window);
        self.views.reveal_pane(PaneId::Visualizer, &mut cx);
        let synthetic = match synthetic {
            None if self.views.panes.explorer.root_path().is_some() => {
                cx.act(Action::LoadFolderObjects);
                None
            }
            None => Some(crate::visualizer::SYNTHETIC_NODES),
            n => n,
        };
        self.views
            .panes
            .visualizer
            .queue_benchmark(synthetic, &mut cx);
        self.views.process_actions(&mut cx);
        Response { repaint: true }
    }

    /// Background data is ready (see `WAKE_MESSAGE`).
    pub fn on_wake(&mut self) -> Response {
        self.waker.reset();
        let mut cx = Cx::new(&self.text, self.window);
        self.views.panes.terminal.pump(&mut cx);
        self.views.drain_folders(&mut cx);
        self.views.drain_objects(&mut cx);
        self.views.drain_debug(&mut cx);
        self.views.drain_build(&mut cx);
        self.views.tick_replay(&mut cx);
        let v = &mut self.views.panes.visualizer;
        let moved = v.update() | v.poll();
        if moved && self.views.visualizer_shown() {
            cx.repaint();
        }
        self.views.panes.visualizer.start_queued_benchmark(&mut cx);
        self.views.process_actions(&mut cx);
        Response {
            repaint: cx.needs_repaint(),
        }
    }

    /// What the window procedure should treat a client point (in DIPs) as.
    pub fn hit_test(&self, p: PointF) -> WindowHit {
        self.views.hit_test(p)
    }

    /// The caption button under the pointer and the one held down.
    pub fn set_caption_state(
        &mut self,
        hover: Option<WindowHit>,
        pressed: Option<WindowHit>,
    ) -> Response {
        let t = &mut self.views.title_bar;
        let changed = (t.hover, t.pressed) != (hover, pressed);
        t.hover = hover;
        t.pressed = pressed;
        Response { repaint: changed }
    }

    /// The window was maximized or restored (the maximize glyph follows).
    pub fn set_maximized(&mut self, maximized: bool) -> Response {
        let t = &mut self.views.title_bar;
        let changed = t.maximized != maximized;
        t.maximized = maximized;
        Response { repaint: changed }
    }

    /// A press on the title bar outside the UI (dragging, caption buttons)
    /// closes any open menu.
    pub fn close_menus(&mut self) -> Response {
        let open = self.views.menu_bar.is_open() || self.views.popup.is_open();
        self.views.menu_bar.close();
        self.views.popup.close();
        Response { repaint: open }
    }

    /// Asks about unsaved files. `false` means the user cancelled closing.
    pub fn query_close(&mut self) -> bool {
        let mut cx = Cx::new(&self.text, self.window);
        let ok = self.views.panes.editor.query_close(&mut cx);
        self.views.process_actions(&mut cx);
        ok
    }

    pub fn paint(&mut self) -> Response {
        let size = self.gfx.size();
        self.views.layout(size, &self.text);
        let views = &mut self.views;
        let theme = &self.theme;
        let frame = self
            .gfx
            .render(&self.text, theme.chrome_bg, |d| views.paint(d, theme));
        match frame {
            Ok(Frame::Presented) => {
                let visualizer = self.views.visualizer_shown();
                if visualizer {
                    let draw = self.gfx.last_draw_time();
                    for line in self.views.panes.visualizer.frame_done(draw) {
                        self.views.panes.output.push(false, &line);
                    }
                }
                Response {
                    repaint: (visualizer && self.views.panes.visualizer.wants_frame())
                        || self.views.data_animating(),
                }
            }
            Ok(Frame::DeviceLost) => {
                self.views
                    .panes
                    .output
                    .push(false, "GPU device lost; recreating device resources");
                Response { repaint: true }
            }
            Err(e) => {
                self.views
                    .panes
                    .output
                    .push(true, &format!("Render failed: {e}"));
                Response::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_open_in_their_own_tabs() {
        let areas = TopTab::ALL.map(|t| DockArea::new(default_tree(t)));
        for pane in PaneId::ALL {
            let homes: Vec<TopTab> = TopTab::ALL
                .into_iter()
                .filter(|t| areas[t.index()].contains(pane))
                .collect();
            // Reopening a closed panel puts it back in its default workspace.
            assert_eq!(homes, [default_spot(pane).0], "{pane:?}");
        }
        for pane in [
            PaneId::Debugger,
            PaneId::Variables,
            PaneId::CallStack,
            PaneId::Memory,
        ] {
            assert_eq!(default_spot(pane).0, TopTab::Debugger, "{pane:?}");
        }
        // The data visualization is not part of the debugger.
        assert_eq!(default_spot(PaneId::Data).0, TopTab::Visualizer);
    }
}
