//! Root of the UI: the title bar with its menus, the top strip
//! (Code/Visualizer workspaces), each workspace's dock area, focus and input
//! routing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::callstack::CallStack;
use crate::codepane::CodePane;
use crate::debug::{self, Debug, Phase, Update};
use crate::dock::{Axis, DockArea, Drop, Node, PaneHost, PaneId, Request, Spot, TabDrag, Zone};
use crate::editor::EditorArea;
use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::explorer::Explorer;
use crate::folder_watch::{Changes, FolderWatcher};
use crate::geom::{PointF, RectF, SizeF};
use crate::memory::Memory;
use crate::menubar::{Entry, Mark, MenuBar, Outcome};
use crate::objects::{LoadResult, ObjectEvent, ObjectService};
use crate::output::Output;
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::replay::{self, Replay, ReplayAction};
use crate::sys::dialog::{self, ComGuard};
use crate::sys::gfx::{DrawCtx, Frame, Graphics};
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::{NativeWindow, Waker};
use crate::terminal::Terminal;
use crate::theme::Theme;
use crate::titlebar::{TitleBar, WindowHit, TITLE_BAR_H};
use crate::visualizer::{DebugStatus, Mode, Visualizer};
use crate::Error;
use renderer::GraphMode;

const TOP_STRIP_H: f32 = 34.0;
const TOP_TAB_PAD: f32 = 16.0;

/// What a menu row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuCmd {
    TogglePane(PaneId),
    GraphMode(GraphMode),
}

/// The menu bar's titles, in order.
const MENUS: [&str; 2] = ["Panels", "Visualizers"];

/// What the app should do after a call into the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Response {
    /// Invalidate the window so `paint` runs.
    pub repaint: bool,
}

/// The two workspaces, each with its own dock layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopTab {
    Code,
    Visualizer,
}

impl TopTab {
    const ALL: [Self; 2] = [Self::Code, Self::Visualizer];

    fn index(self) -> usize {
        self as usize
    }

    fn title(self) -> &'static str {
        match self {
            Self::Code => "Code",
            Self::Visualizer => "Visualizer",
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
    source: CodePane,
    stack: CallStack,
    memory: Memory,
}

impl PaneHost for Panes {
    fn pane(&mut self, id: PaneId) -> &mut dyn Panel {
        match id {
            PaneId::Explorer => &mut self.explorer,
            PaneId::Editor => &mut self.editor,
            PaneId::Output => &mut self.output,
            PaneId::Terminal => &mut self.terminal,
            PaneId::Visualizer => &mut self.visualizer,
            PaneId::Source => &mut self.source,
            PaneId::CallStack => &mut self.stack,
            PaneId::Memory => &mut self.memory,
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
                (
                    Node::split(
                        Axis::Vertical,
                        vec![
                            (Node::tabs(&[Source]), 0.65),
                            (Node::tabs(&[CallStack, Memory]), 0.35),
                        ],
                    ),
                    0.3,
                ),
                (Node::tabs(&[Visualizer]), 0.7),
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
        Source | CallStack | Memory => {
            (TopTab::Visualizer, spot(Some(Visualizer), Zone::Left, 0.3))
        }
    }
}

/// Everything except the device-independent resources the context borrows.
struct Views {
    tab: TopTab,
    strip: RectF,
    tab_rects: [RectF; 2],
    tab_hover: Option<usize>,
    title_bar: TitleBar,
    menu_bar: MenuBar<MenuCmd>,
    /// Where the menu titles end.
    menu_end: f32,
    areas: [DockArea; 2],
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
        let v = &mut self.panes.visualizer;
        v.set_debug_status(DebugStatus {
            phase: self.debug.phase(),
            program: self
                .debug
                .program()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            stopped: self.debug.stop_description(),
            recording: self.debug.recording(),
            replay: self.replay.as_ref().map(Replay::status),
        });
        // The Source panel follows the frame focused in the Data view.
        let target = self.panes.visualizer.code_target();
        self.panes.source.show(target);

        let full = RectF::new(0.0, 0.0, size.w, size.h);
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
    fn menus(&self) -> [Vec<Entry<MenuCmd>>; 2] {
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
            Entry::Separator,
            Entry::Header("Data visualizer".into()),
            Entry::Note("No settings yet".into()),
        ];
        [panels, visualizers]
    }

    fn run_menu(&mut self, cmd: MenuCmd, cx: &mut Cx) {
        match cmd {
            MenuCmd::TogglePane(p) => self.toggle_pane(p, cx),
            MenuCmd::GraphMode(m) => {
                self.panes.visualizer.set_graph_mode(m, cx);
                cx.log(format!("Call graph mode: {}", m.title()));
            }
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
        let lines = p.source.path().map(|path| debug.marks_for(path).0);
        p.source.set_breakpoints(lines.unwrap_or_default());
        let stale = debug.phase() != Phase::Stopped && self.replay.is_none();
        p.source.set_stale(stale);
        p.stack.set_view(p.visualizer.focused_frame(), stale);

        let i = self.tab.index();
        self.areas[i].paint(d, theme, &mut self.panes, self.window_focused);
        self.menu_bar.paint_menus(d, theme);
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

    fn switch_tab(&mut self, tab: TopTab, cx: &mut Cx) {
        if self.tab != tab {
            let old = self.tab.index();
            self.areas[old].leave(&mut self.panes, cx);
            self.areas[old].clear_preview();
            self.tab = tab;
            cx.repaint();
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
        if self.menu_event(e, cx) {
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
        // Debugger keys (Visual Studio's), in both tabs.
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
        match key {
            Key::Letter('O') if !mods.shift => cx.act(Action::PickFolder),
            Key::Digit(1) => self.switch_tab(TopTab::Code, cx),
            Key::Digit(2) => self.switch_tab(TopTab::Visualizer, cx),
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
                    self.panes.visualizer.set_data_stale(true);
                }
            }
            DebugAction::PickProgram => {
                self.pick_program(cx);
            }
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
        let p = &mut self.panes;
        p.visualizer.clear_data();
        p.visualizer.set_mode(Mode::Data, cx);
        p.stack.clear();
        p.source.clear();
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
        p.visualizer.set_snapshot(snap);
        p.source.show(p.visualizer.code_target());
        p.source.stopped();
        p.source.set_snapshot(shared.clone());
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

    /// Applies the debugger's events.
    fn drain_debug(&mut self, cx: &mut Cx) {
        for u in self.debug.drain(cx) {
            match u {
                Update::Running => self.panes.visualizer.set_data_stale(true),
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
                Update::Ended => self.panes.visualizer.set_data_stale(true),
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
                        self.panes.memory.show(target);
                        self.reveal_pane(PaneId::Memory, cx);
                        cx.repaint();
                    }
                    Action::RevealFrame(i) => {
                        self.panes.visualizer.reveal_frame(i);
                        cx.repaint();
                    }
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
                tab_rects: [RectF::default(); 2],
                tab_hover: None,
                menu_bar: MenuBar::new(&MENUS),
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
                    source: CodePane::default(),
                    stack: CallStack::default(),
                    memory: Memory::default(),
                },
                owner: None,
                window_focused: true,
                objects,
                folders,
                debug: Debug::new(waker.clone()),
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
        let open = self.views.menu_bar.is_open();
        self.views.menu_bar.close();
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
                    repaint: visualizer && self.views.panes.visualizer.wants_frame(),
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
