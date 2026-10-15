//! Root of the UI: the top-level Code/Visualizer tabs, the Code tab layout
//! (Explorer │ Editor ─ Output/Terminal), focus and input routing.

use std::path::{Path, PathBuf};

use crate::bottom::{BottomArea, BottomTab};
use crate::debug::{self, Debug, Phase, Update};
use crate::editor::EditorArea;
use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::explorer::Explorer;
use crate::geom::{PointF, RectF, SizeF};
use crate::objects::{LoadResult, ObjectEvent, ObjectService};
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::split::{Anchor, Axis, Splitter};
use crate::sys::dialog::{self, ComGuard};
use crate::sys::gfx::{DrawCtx, Frame, Graphics};
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::{NativeWindow, Waker};
use crate::theme::Theme;
use crate::visualizer::{DebugStatus, Mode, Visualizer};
use crate::Error;

const TOP_STRIP_H: f32 = 34.0;
const TOP_TAB_PAD: f32 = 16.0;

/// What the app should do after a call into the shell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Response {
    /// Invalidate the window so `paint` runs.
    pub repaint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TopTab {
    Code,
    Visualizer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Explorer,
    Editor,
    Bottom,
}

/// Who receives mouse events between a press and its release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseOwner {
    Region(Region),
    Splitter(Axis),
    TopStrip,
}

struct CodeView {
    vsplit: Splitter,
    hsplit: Splitter,
    explorer: Explorer,
    editor: EditorArea,
    bottom: BottomArea,
    focus: Region,
    hover: Option<Region>,
    rects: [RectF; 3],
}

impl CodeView {
    fn new(waker: Waker) -> Self {
        Self {
            vsplit: Splitter::new(Axis::Horizontal, Anchor::Start, 260.0, 160.0, 300.0),
            hsplit: Splitter::new(Axis::Vertical, Anchor::End, 240.0, 90.0, 120.0),
            explorer: Explorer::default(),
            editor: EditorArea::default(),
            bottom: BottomArea::new(waker),
            focus: Region::Explorer,
            hover: None,
            rects: [RectF::default(); 3],
        }
    }

    fn panel(&mut self, r: Region) -> &mut dyn Panel {
        match r {
            Region::Explorer => &mut self.explorer,
            Region::Editor => &mut self.editor,
            Region::Bottom => &mut self.bottom,
        }
    }

    fn region_at(&self, p: PointF) -> Option<Region> {
        [Region::Explorer, Region::Editor, Region::Bottom]
            .into_iter()
            .zip(self.rects)
            .find(|(_, r)| r.contains(p))
            .map(|(region, _)| region)
    }

    fn splitter(&mut self, axis: Axis) -> &mut Splitter {
        match axis {
            Axis::Horizontal => &mut self.vsplit,
            Axis::Vertical => &mut self.hsplit,
        }
    }

    fn layout(&mut self, r: RectF, text: &TextSystem) {
        let (left, right) = self.vsplit.layout(r);
        let (top, bottom) = self.hsplit.layout(right);
        self.rects = [left, top, bottom];
        self.explorer.layout(left, text);
        self.editor.layout(top, text);
        self.bottom.layout(bottom, text);
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, window_focused: bool) {
        let focus = self.focus;
        let f = |r: Region| window_focused && focus == r;
        self.explorer.paint(d, theme, f(Region::Explorer));
        self.editor.paint(d, theme, f(Region::Editor));
        self.bottom.paint(d, theme, f(Region::Bottom));
        self.vsplit.paint(d, theme);
        self.hsplit.paint(d, theme);
    }

    fn set_focus(&mut self, r: Region, cx: &mut Cx) {
        if self.focus != r {
            self.focus = r;
            cx.repaint();
        }
    }

    fn on_event(&mut self, e: &Event, owner: &mut Option<MouseOwner>, cx: &mut Cx) {
        match *e {
            Event::KeyDown { .. } | Event::Char(_) => {
                let focus = self.focus;
                self.panel(focus).on_event(e, cx);
            }
            Event::MouseDown { pos, .. } => {
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    if self.splitter(axis).on_event(e, cx) {
                        *owner = Some(MouseOwner::Splitter(axis));
                        return;
                    }
                }
                if let Some(r) = self.region_at(pos) {
                    self.set_focus(r, cx);
                    *owner = Some(MouseOwner::Region(r));
                    self.panel(r).on_event(e, cx);
                }
            }
            Event::MouseMove { pos, .. } => match *owner {
                Some(MouseOwner::Splitter(axis)) => {
                    self.splitter(axis).on_event(e, cx);
                }
                Some(MouseOwner::Region(r)) => {
                    self.panel(r).on_event(e, cx);
                }
                _ => {
                    let on_splitter = self.vsplit.on_event(e, cx) | self.hsplit.on_event(e, cx);
                    let region = if on_splitter {
                        None
                    } else {
                        self.region_at(pos)
                    };
                    if region != self.hover {
                        if let Some(old) = self.hover {
                            self.panel(old).on_event(&Event::MouseLeave, cx);
                        }
                        self.hover = region;
                    }
                    if let Some(r) = region {
                        self.panel(r).on_event(e, cx);
                    }
                }
            },
            Event::MouseUp { .. } => match owner.take() {
                Some(MouseOwner::Splitter(axis)) => {
                    self.splitter(axis).on_event(e, cx);
                }
                Some(MouseOwner::Region(r)) => {
                    self.panel(r).on_event(e, cx);
                }
                _ => {}
            },
            Event::Wheel { pos, .. } => {
                if let Some(r) = self.region_at(pos) {
                    self.panel(r).on_event(e, cx);
                }
            }
            Event::MouseLeave => {
                self.vsplit.on_event(e, cx);
                self.hsplit.on_event(e, cx);
                if let Some(old) = self.hover.take() {
                    self.panel(old).on_event(e, cx);
                }
            }
            Event::Focus(_) => cx.repaint(),
        }
    }
}

/// Everything except the device-independent resources the context borrows.
struct Views {
    tab: TopTab,
    strip: RectF,
    tab_rects: [RectF; 2],
    tab_hover: Option<usize>,
    code: CodeView,
    visualizer: Visualizer,
    owner: Option<MouseOwner>,
    window_focused: bool,
    objects: ObjectService,
    debug: Debug,
}

impl Views {
    fn layout(&mut self, size: SizeF, text: &TextSystem) {
        self.visualizer.set_debug_status(DebugStatus {
            phase: self.debug.phase(),
            program: self
                .debug
                .program()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            stopped: self.debug.stop_description(),
        });
        let full = RectF::new(0.0, 0.0, size.w, size.h);
        let (strip, body) = full.split_top(TOP_STRIP_H);
        self.strip = strip;
        let mut x = strip.x + 8.0;
        for (i, label) in ["Code", "Visualizer"].into_iter().enumerate() {
            let w = text.measure(TextStyle::Ui, label) + 2.0 * TOP_TAB_PAD;
            self.tab_rects[i] = RectF::new(x, strip.y, w, strip.h);
            x += w;
        }
        self.code.layout(body, text);
        self.visualizer.layout(body, text);
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme) {
        d.fill_rect(self.strip, theme.chrome_bg);
        for (i, label) in ["Code", "Visualizer"].into_iter().enumerate() {
            let r = self.tab_rects[i];
            let active = i == self.tab_index();
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
                label,
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
        if let Some(root) = self.code.explorer.root_path() {
            let label = root.display().to_string();
            let w = d.text_system().measure(TextStyle::UiSmall, &label);
            let after_tabs = self.tab_rects[1].right() + 16.0;
            let x = (self.strip.right() - w - 12.0).max(after_tabs);
            d.text(
                TextStyle::UiSmall,
                &label,
                RectF::new(x, self.strip.y, self.strip.right() - x - 8.0, self.strip.h),
                theme.text_dim,
            );
        }
        d.fill_rect(
            RectF::new(self.strip.x, self.strip.bottom() - 1.0, self.strip.w, 1.0),
            theme.border,
        );
        let debug = &self.debug;
        self.code.editor.set_marks(|p| debug.marks_for(p));
        match self.tab {
            TopTab::Code => self.code.paint(d, theme, self.window_focused),
            TopTab::Visualizer => self.visualizer.paint(d, theme, self.window_focused),
        }
    }

    fn tab_index(&self) -> usize {
        match self.tab {
            TopTab::Code => 0,
            TopTab::Visualizer => 1,
        }
    }

    fn switch_tab(&mut self, tab: TopTab, cx: &mut Cx) {
        if self.tab != tab {
            self.tab = tab;
            self.owner = None;
            cx.repaint();
        }
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) {
        if let Event::Focus(focused) = *e {
            self.window_focused = focused;
            cx.repaint();
            return;
        }
        if self.global_key(e, cx) {
            return;
        }
        // The top strip is shared chrome; everything below belongs to the
        // active tab only.
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } if self.strip.contains(pos) => {
                self.owner = Some(MouseOwner::TopStrip);
                if self.tab_rects[0].contains(pos) {
                    self.switch_tab(TopTab::Code, cx);
                } else if self.tab_rects[1].contains(pos) {
                    self.switch_tab(TopTab::Visualizer, cx);
                }
                return;
            }
            Event::MouseMove { pos, .. } if self.owner.is_none() => {
                let hover = self.tab_rects.iter().position(|r| r.contains(pos));
                if hover != self.tab_hover {
                    self.tab_hover = hover;
                    cx.repaint();
                }
                if hover.is_some() {
                    cx.cursor = CursorShape::Hand;
                }
            }
            Event::MouseLeave if self.tab_hover.is_some() => {
                self.tab_hover = None;
                cx.repaint();
            }
            Event::MouseUp { .. } if self.owner == Some(MouseOwner::TopStrip) => {
                self.owner = None;
                return;
            }
            _ => {}
        }
        match self.tab {
            TopTab::Code => self.code.on_event(e, &mut self.owner, cx),
            TopTab::Visualizer => {
                self.visualizer.on_event(e, cx);
            }
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
                Key::F(5) => Some(DebugAction::Run),
                Key::F(10) => Some(DebugAction::Step(debugger::Command::Next)),
                Key::F(11) if mods.shift => Some(DebugAction::Step(debugger::Command::Finish)),
                Key::F(11) => Some(DebugAction::Step(debugger::Command::Step)),
                Key::F(9) => {
                    if let Some((path, line)) = self.code.editor.caret_location() {
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
                self.switch_tab(TopTab::Code, cx);
                self.code.bottom.show(BottomTab::Terminal, cx);
                self.code.set_focus(Region::Bottom, cx);
            }
            _ => return false,
        }
        true
    }

    fn debug_action(&mut self, a: DebugAction, cx: &mut Cx) {
        match a {
            DebugAction::Run => match self.debug.phase() {
                Phase::Stopped => self.debug.command(debugger::Command::Continue, cx),
                Phase::Idle => {
                    if self.debug.program().is_none() {
                        // `foo.exe` next to the open `foo.cpp`, else ask.
                        let guess = self
                            .code
                            .editor
                            .caret_location()
                            .and_then(|(p, _)| debug::program_for_source(&p));
                        match guess {
                            Some(p) => self.debug.set_program(p),
                            None => {
                                if !self.pick_program(cx) {
                                    return;
                                }
                            }
                        }
                    }
                    self.visualizer.clear_data();
                    self.visualizer.set_mode(Mode::Data, cx);
                    self.debug.start(cx);
                }
                Phase::Starting | Phase::Running => {}
            },
            DebugAction::Pause => self.debug.command(debugger::Command::Interrupt, cx),
            DebugAction::Step(c) => self.debug.command(c, cx),
            DebugAction::Stop => {
                if self.debug.is_active() {
                    self.debug.stop(cx);
                    self.visualizer.set_data_stale(true);
                }
            }
            DebugAction::PickProgram => {
                self.pick_program(cx);
            }
        }
        cx.repaint();
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
                Update::Running => self.visualizer.set_data_stale(true),
                Update::Stopped { file, line } => {
                    // In the Code tab, follow the stop like any debugger.
                    if let (TopTab::Code, Some(file), Some(line)) = (self.tab, file, line) {
                        if file.is_file() {
                            self.code.editor.open_at(&file, line, cx);
                        }
                    }
                }
                Update::Snapshot(snap) => self.visualizer.set_snapshot(*snap),
                Update::Ended => self.visualizer.set_data_stale(true),
            }
        }
    }

    fn open_folder(&mut self, path: &Path, cx: &mut Cx) {
        match self.code.explorer.set_root(path) {
            Ok(()) => cx.log(format!("Opened folder {}", path.display())),
            Err(e) => cx.log_error(format!("Could not read folder: {e}")),
        }
        self.visualizer
            .set_root(self.code.explorer.root_path().map(Path::to_path_buf));
        self.code.bottom.terminal.set_cwd(Some(path.to_path_buf()));
        if self.code.bottom.terminal.is_running() {
            self.code
                .bottom
                .terminal
                .run_command(&format!("cd \"{}\"", path.display()), cx);
        }
        self.code.set_focus(Region::Explorer, cx);
        cx.repaint();
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
                    self.visualizer.set_folder_loading(false);
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
                .code
                .explorer
                .apply_object(&path, graph.as_deref().map_err(|e| e.reason.as_str()));
            if let Ok(g) = &graph {
                self.visualizer.add_graph(g.clone());
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
                        self.code.editor.open(&path, cx);
                        if self.code.editor.has_documents() {
                            self.code.set_focus(Region::Editor, cx);
                        }
                    }
                    Action::LoadObject(path) => {
                        if let Some(warning) = self.objects.load(&path) {
                            cx.log_error(warning);
                        }
                    }
                    Action::LoadFolderObjects => match self.code.explorer.root_path() {
                        Some(root) => {
                            let root = root.to_path_buf();
                            cx.log(format!(
                                "Loading every object file under {}…",
                                root.display()
                            ));
                            self.visualizer.set_folder_loading(true);
                            if let Some(warning) = self.objects.load_folder(&root) {
                                cx.log_error(warning);
                            }
                            cx.repaint();
                        }
                        None => cx.log_error("Open a folder first (Ctrl+O), then add its objects"),
                    },
                    Action::OpenSource { path, line } => {
                        self.switch_tab(TopTab::Code, cx);
                        self.code.editor.open_at(&path, line, cx);
                        if self.code.editor.has_documents() {
                            self.code.set_focus(Region::Editor, cx);
                        }
                    }
                    Action::Debug(a) => self.debug_action(a, cx),
                    Action::ToggleBreakpoint { path, line } => {
                        self.debug.toggle_breakpoint(&path, line, cx);
                    }
                    Action::PickFolder => match dialog::pick_folder(cx.window) {
                        Ok(Some(path)) => self.open_folder(&path, cx),
                        Ok(None) => {}
                        Err(e) => cx.log_error(format!("Folder picker failed: {e}")),
                    },
                    Action::Log { error, text } => {
                        self.code.bottom.output.push(error, &text);
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
                code: CodeView::new(waker.clone()),
                visualizer,
                owner: None,
                window_focused: true,
                objects,
                debug: Debug::new(waker.clone()),
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
        for e in [objects_error, layout_error].into_iter().flatten() {
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
        self.views.code.bottom.output.push(error, text);
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
                self.views.code.bottom.terminal.start(&mut cx);
            }
        }
        self.views.process_actions(&mut cx);
        Response { repaint: true }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.gfx.set_dpi(dpi);
        self.views.code.editor.clear_caches();
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
        self.views.switch_tab(TopTab::Visualizer, &mut cx);
        let synthetic = match synthetic {
            None if self.views.code.explorer.root_path().is_some() => {
                cx.act(Action::LoadFolderObjects);
                None
            }
            None => Some(crate::visualizer::SYNTHETIC_NODES),
            n => n,
        };
        self.views.visualizer.queue_benchmark(synthetic, &mut cx);
        self.views.process_actions(&mut cx);
        Response { repaint: true }
    }

    /// Background data is ready (see `WAKE_MESSAGE`).
    pub fn on_wake(&mut self) -> Response {
        self.waker.reset();
        let mut cx = Cx::new(&self.text, self.window);
        self.views.code.bottom.terminal.pump(&mut cx);
        self.views.drain_objects(&mut cx);
        self.views.drain_debug(&mut cx);
        let moved = self.views.visualizer.update() | self.views.visualizer.poll();
        if moved && self.views.tab == TopTab::Visualizer {
            cx.repaint();
        }
        self.views.visualizer.start_queued_benchmark(&mut cx);
        self.views.process_actions(&mut cx);
        Response {
            repaint: cx.needs_repaint(),
        }
    }

    /// Asks about unsaved files. `false` means the user cancelled closing.
    pub fn query_close(&mut self) -> bool {
        let mut cx = Cx::new(&self.text, self.window);
        let ok = self.views.code.editor.query_close(&mut cx);
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
                let visualizer = self.views.tab == TopTab::Visualizer;
                if visualizer {
                    let draw = self.gfx.last_draw_time();
                    for line in self.views.visualizer.frame_done(draw) {
                        self.views.code.bottom.output.push(false, &line);
                    }
                }
                Response {
                    repaint: visualizer && self.views.visualizer.wants_frame(),
                }
            }
            Ok(Frame::DeviceLost) => {
                self.views
                    .code
                    .bottom
                    .output
                    .push(false, "GPU device lost; recreating device resources");
                Response { repaint: true }
            }
            Err(e) => {
                self.views
                    .code
                    .bottom
                    .output
                    .push(true, &format!("Render failed: {e}"));
                Response::default()
            }
        }
    }
}
