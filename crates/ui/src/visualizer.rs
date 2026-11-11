//! Visualizer tab, drawn by `crates/renderer`: every loaded object file,
//! merged and shown a level at a time: the source files joined by `#include`;
//! one file's classes, functions and globals; a class's members or a
//! function's locals. Clicking a file, class or function opens its level; a
//! breadcrumb over the canvas (and `Backspace`) goes back up. At the innermost
//! level a click opens a details window (see `infomodal.rs`). The canvas pans,
//! zooms and drags nodes as before. It is a separate root view: it owns its
//! whole area and never shares layout or input with the other tabs.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{CodeGraph, NodeId};
use renderer::{
    Cursor, EdgeClass, Entity, GraphMode, GraphModel, GraphView, Level, NodeClass, Program, Rgba,
    Style, Vec2,
};

use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::infomodal::{InfoModal, ModalAction};
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::sys::Waker;
use crate::theme::{Color, Theme};

pub(crate) const TOOLBAR_H: f32 = 40.0;
const CRUMB_H: f32 = 26.0;
const CRUMB_PAD: f32 = 10.0;
pub(crate) const BUTTON_H: f32 = 26.0;
pub(crate) const BUTTON_PAD: f32 = 12.0;
pub(crate) const BUTTON_GAP: f32 = 6.0;
/// While a folder loads, rebuild the model at most this often.
const LOADING_REBUILD_INTERVAL: Duration = Duration::from_millis(1000);
pub(crate) const KEY_PAN: f32 = 80.0;
const BENCH_FRAMES: usize = 300;
pub const SYNTHETIC_NODES: usize = 10_000;
const SYNTHETIC_EDGES_PER_NODE: f32 = 2.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Build,
    AddFolder,
    Fit,
    Clear,
}

impl Button {
    const ALL: [Self; 4] = [Self::Build, Self::AddFolder, Self::Fit, Self::Clear];

    fn label(self, building: bool) -> &'static str {
        match self {
            Self::AddFolder => "Add folder objects",
            Self::Fit => "Fit",
            Self::Clear => "Clear",
            Self::Build if building => "Building\u{2026}",
            Self::Build => "Build",
        }
    }
}

/// How a toolbar button looks right now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ButtonLook {
    pub enabled: bool,
    pub primary: bool,
    pub on: bool,
    pub hover: bool,
}

/// A toolbar button (shared with the Debugger's toolbar).
pub(crate) fn paint_tool_button(
    d: &mut DrawCtx,
    theme: &Theme,
    r: RectF,
    label: &str,
    look: ButtonLook,
) {
    let ButtonLook {
        enabled,
        primary,
        on,
        hover,
    } = look;
    let (fill, text, border) = match () {
        _ if !enabled => (None, theme.text_dim.with_alpha(0.45), Some(theme.border)),
        _ if primary => (Some(theme.button_bg), theme.button_text, None),
        _ if on => (
            Some(theme.selection_bg),
            theme.accent,
            Some(theme.accent.with_alpha(0.6)),
        ),
        _ => (
            hover.then_some(theme.hover_bg),
            if hover { theme.text } else { theme.text_dim },
            Some(theme.border),
        ),
    };
    let radius = 5.0;
    if let Some(fill) = fill {
        let fill = if hover && primary {
            theme.accent.with_alpha(0.85)
        } else {
            fill
        };
        d.fill_round_rect(r, radius, fill);
    }
    if let Some(border) = border {
        renderer::Canvas::stroke_rect(d, to_rect(r), radius, rgba(border), 1.0);
    }
    // Centred: Run/Continue is sized for its longer label.
    let w = d.text_system().measure(TextStyle::Ui, label);
    let x = r.x + (r.w - w) / 2.0;
    d.text(
        TextStyle::Ui,
        label,
        RectF::new(x, r.y, r.right() - x, r.h),
        text,
    );
}

/// Scripted camera motion that measures frame times.
struct Bench {
    frame: usize,
    last: Option<Instant>,
    intervals: Vec<f32>,
    draws: Vec<f32>,
    /// Time spent issuing the graph's draw calls (the rest of the draw
    /// time is Direct2D flushing them in EndDraw).
    records: Vec<f32>,
}

pub struct Visualizer {
    /// The open folder is being compiled.
    building: bool,
    /// Build is available: compiling and no running program holding its exe.
    can_build: bool,
    rect: RectF,
    toolbar: RectF,
    canvas: RectF,
    buttons: Vec<(Button, RectF)>,
    hover_button: Option<Button>,
    view: GraphView,
    graphs: BTreeMap<PathBuf, Arc<CodeGraph>>,
    /// The loaded objects regrouped by file, class and function.
    program: Program,
    /// Which part of the program the canvas shows.
    level: Level,
    /// Node positions of the levels visited, restored on return.
    saved: HashMap<Level, HashMap<NodeId, Vec2>>,
    /// Breadcrumb segments over the canvas (laid out when painted).
    crumbs: Vec<(String, Level, RectF)>,
    hover_crumb: Option<usize>,
    /// The details window, while open.
    modal: Option<InfoModal>,
    /// A click just opened a level; the second click of a double-click
    /// must not act on the new level.
    swallow_double: bool,
    graph_mode: GraphMode,
    dirty: bool,
    folder_loading: bool,
    last_build: Option<Instant>,
    /// Showing the synthetic benchmark graph instead of loaded objects.
    synthetic: bool,
    /// The folder open in the Explorer, for resolving relative source paths.
    root: Option<PathBuf>,
    /// The canvas owns the mouse between a press and its release.
    canvas_capture: bool,
    last_draw: Duration,
    bench: Option<Bench>,
    /// Start a benchmark once loading and layout have finished.
    bench_queued: bool,
}

impl Visualizer {
    /// Returns the panel and, if the layout thread could not start, why.
    pub fn new(waker: Waker) -> (Self, Option<String>) {
        let (view, error) = GraphView::new(Box::new(move || waker.wake()));
        let v = Self {
            building: false,
            can_build: true,
            rect: RectF::default(),
            toolbar: RectF::default(),
            canvas: RectF::default(),
            buttons: Vec::new(),
            hover_button: None,
            view,
            graphs: BTreeMap::new(),
            program: Program::default(),
            level: Level::Files,
            saved: HashMap::new(),
            crumbs: Vec::new(),
            hover_crumb: None,
            modal: None,
            swallow_double: false,
            graph_mode: GraphMode::default(),
            dirty: false,
            folder_loading: false,
            last_build: None,
            synthetic: false,
            root: None,
            canvas_capture: false,
            last_draw: Duration::ZERO,
            bench: None,
            bench_queued: false,
        };
        (v, error)
    }

    pub fn graph_mode(&self) -> GraphMode {
        self.graph_mode
    }

    /// Abstracted (user code only) or Verbose (everything) call graph.
    pub fn set_graph_mode(&mut self, mode: GraphMode, cx: &mut Cx) {
        if mode == self.graph_mode {
            return;
        }
        self.graph_mode = mode;
        if !self.synthetic {
            self.view
                .set_model(self.program.model(self.level, mode.filter()));
        }
        cx.repaint();
    }

    /// What the Build button shows (set before each layout).
    pub fn set_build_state(&mut self, building: bool, can_build: bool) {
        self.building = building;
        self.can_build = can_build;
    }

    pub fn set_root(&mut self, root: Option<PathBuf>) {
        if self.root != root {
            self.root = root;
            // Sources and headers resolve against the open folder.
            self.dirty = !self.graphs.is_empty();
        }
    }

    /// Adds or replaces (on reload) one object's graph.
    pub fn add_graph(&mut self, graph: Arc<CodeGraph>) {
        if self.synthetic {
            self.synthetic = false;
            self.graphs.clear();
        }
        self.graphs.insert(graph.path.clone(), graph);
        self.dirty = true;
    }

    pub fn set_folder_loading(&mut self, loading: bool) {
        self.folder_loading = loading;
    }

    /// Rebuilds the model if objects or filters changed (throttled while a
    /// folder loads). Returns `true` if the view changed.
    pub fn update(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        if self.folder_loading
            && self
                .last_build
                .is_some_and(|t| t.elapsed() < LOADING_REBUILD_INTERVAL)
        {
            return false;
        }
        self.dirty = false;
        self.last_build = Some(Instant::now());
        let graphs: Vec<Arc<CodeGraph>> = self.graphs.values().cloned().collect();
        self.program = Program::build(&graphs, self.root.as_deref());
        if !self.program.has_level(self.level) {
            self.saved.clear();
            self.go(Level::Files);
        } else {
            self.view
                .set_model(self.program.model(self.level, self.graph_mode.filter()));
        }
        true
    }

    /// Shows another level, restoring where its nodes were last time.
    fn go(&mut self, level: Level) {
        self.saved.insert(self.level, self.view.positions_by_id());
        self.level = level;
        self.modal = None;
        self.hover_crumb = None;
        let seeds = self.saved.get(&level).cloned().unwrap_or_default();
        self.view
            .switch_model(self.program.model(level, self.graph_mode.filter()), &seeds);
    }

    fn go_up(&mut self) -> bool {
        match self.program.parent(self.level) {
            Some(parent) => {
                self.go(parent);
                true
            }
            None => false,
        }
    }

    /// A click on a node: files, classes and functions open their level
    /// (from the outer two levels); anything else, and the level's own
    /// subject, opens the details window.
    fn click_node(&mut self, i: u32, cx: &mut Cx) {
        let Some(e) = self.view.model().node(i).and_then(|n| n.entity) else {
            return;
        };
        let outer = matches!(self.level, Level::Files | Level::File(_));
        let hub = self.view.model().hub == Some(i);
        match self.program.level_of(e) {
            Some(level) if outer && !hub => {
                self.go(level);
                self.swallow_double = true;
            }
            _ => self.show_details(e),
        }
        cx.repaint();
    }

    fn show_details(&mut self, e: Entity) {
        self.modal = Some(InfoModal::new(self.program.details(e)));
    }

    /// Takes new layout positions. Returns `true` if the view moved.
    pub fn poll(&mut self) -> bool {
        self.view.poll()
    }

    /// Wants another frame right away (benchmark running).
    pub fn wants_frame(&self) -> bool {
        self.bench.is_some()
    }

    /// Called after each presented frame with its BeginDraw..EndDraw time.
    /// Returns the report lines when a benchmark finishes.
    pub fn frame_done(&mut self, draw: Duration) -> Vec<String> {
        self.last_draw = draw;
        let Some(bench) = self.bench.as_mut() else {
            return Vec::new();
        };
        let now = Instant::now();
        if let Some(last) = bench.last {
            bench.intervals.push((now - last).as_secs_f32() * 1000.0);
            bench.draws.push(draw.as_secs_f32() * 1000.0);
        }
        bench.last = Some(now);
        if bench.frame < BENCH_FRAMES {
            return Vec::new();
        }
        let Some(bench) = self.bench.take() else {
            return Vec::new();
        };
        let model = self.view.model();
        // Interval k ends frame k + 1: the first third pans with the whole
        // graph on screen, the rest zooms in and back out.
        let split = (BENCH_FRAMES / 3)
            .saturating_sub(1)
            .min(bench.intervals.len());
        let phase = |name: &str, intervals: &[f32], draws: &[f32], records: &[f32]| {
            format!(
                "  {name}: {:.1} fps; frame interval median {:.1} ms, p95 {:.1} ms; \
                 draw median {:.1} ms, p95 {:.1} ms (issuing calls {:.1} ms)",
                1000.0 / mean(intervals).max(0.001),
                percentile(intervals, 0.5),
                percentile(intervals, 0.95),
                percentile(draws, 0.5),
                percentile(draws, 0.95),
                percentile(records, 0.5),
            )
        };
        // Records are per frame (one more than intervals).
        let r = &bench.records;
        let r_split = (split + 1).min(r.len());
        vec![
            format!(
                "Visualizer benchmark: {} nodes, {} edges, {} frames",
                group_digits(model.nodes().len()),
                group_digits(model.edges().len()),
                bench.intervals.len()
            ),
            phase("all frames", &bench.intervals, &bench.draws, r),
            phase(
                "whole graph on screen",
                &bench.intervals[..split],
                &bench.draws[..split],
                &r[..r_split],
            ),
            phase(
                "zooming in and out",
                &bench.intervals[split..],
                &bench.draws[split..],
                &r[r_split..],
            ),
        ]
    }

    /// Benchmarks once the graph has loaded and the layout has settled:
    /// the synthetic graph with `nodes` nodes, or (for `None`) whatever the
    /// caller is loading.
    pub fn queue_benchmark(&mut self, synthetic: Option<usize>, cx: &mut Cx) {
        if let Some(nodes) = synthetic {
            self.load_synthetic(nodes, cx);
        }
        self.bench_queued = true;
    }

    /// Starts a queued benchmark when the view is ready.
    pub fn start_queued_benchmark(&mut self, cx: &mut Cx) {
        if self.bench_queued
            && !self.folder_loading
            && !self.dirty
            && !self.view.is_animating()
            && !self.view.model().is_empty()
        {
            self.bench_queued = false;
            self.start_bench(cx);
        }
    }

    fn start_bench(&mut self, cx: &mut Cx) {
        if self.view.model().is_empty() {
            cx.log_error("Benchmark: nothing to draw; load objects or the synthetic graph first");
            return;
        }
        self.view.fit();
        self.bench = Some(Bench {
            frame: 0,
            last: None,
            intervals: Vec::with_capacity(BENCH_FRAMES),
            draws: Vec::with_capacity(BENCH_FRAMES),
            records: Vec::with_capacity(BENCH_FRAMES),
        });
        cx.log(format!(
            "Benchmark: {BENCH_FRAMES} frames of panning and zooming…"
        ));
        cx.repaint();
    }

    /// Moves the camera for the next benchmark frame: a circle at the
    /// fitted zoom (everything visible), then zoom in, then back out.
    fn bench_step(&mut self) {
        let Some(bench) = self.bench.as_mut() else {
            return;
        };
        let k = bench.frame;
        bench.frame += 1;
        let third = BENCH_FRAMES / 3;
        if k < third {
            let a = k as f32 / third as f32 * std::f32::consts::TAU;
            self.view.pan_by(Vec2::new(a.cos() * 6.0, a.sin() * 6.0));
        } else if k < 2 * third {
            self.view.zoom_by(1.03);
        } else {
            self.view.zoom_by(1.0 / 1.03);
        }
    }

    fn load_synthetic(&mut self, nodes: usize, cx: &mut Cx) {
        self.graphs.clear();
        self.synthetic = true;
        self.dirty = false;
        self.view
            .set_model(GraphModel::synthetic(nodes, SYNTHETIC_EDGES_PER_NODE));
        let m = self.view.model();
        cx.log(format!(
            "Visualizer: synthetic graph with {} nodes and {} edges",
            m.nodes().len(),
            m.edges().len()
        ));
        cx.repaint();
    }

    fn clear(&mut self, cx: &mut Cx) {
        self.graphs.clear();
        self.synthetic = false;
        self.saved.clear();
        self.level = Level::Files;
        self.modal = None;
        self.dirty = true;
        self.update();
        cx.repaint();
    }

    fn press(&mut self, b: Button, cx: &mut Cx) {
        match b {
            Button::Build if self.can_build => cx.act(Action::Debug(DebugAction::Build)),
            Button::Build => {}
            Button::AddFolder => cx.act(Action::LoadFolderObjects),
            Button::Fit => self.view.fit_and_follow(),
            Button::Clear => self.clear(cx),
        }
        cx.repaint();
    }

    fn apply(&mut self, r: renderer::Response, cx: &mut Cx) {
        if r.repaint {
            cx.repaint();
        }
        cx.cursor = match r.cursor {
            Cursor::Arrow => CursorShape::Arrow,
            Cursor::Hand => CursorShape::Hand,
            Cursor::Move => CursorShape::Move,
        };
        if let Some(i) = r.click {
            self.click_node(i, cx);
        }
        if let Some(i) = r.open {
            self.open_source(i, cx);
        }
    }

    fn open_source(&mut self, i: u32, cx: &mut Cx) {
        let Some(node) = self.view.model().node(i) else {
            return;
        };
        let Some(e) = node.entity else {
            return;
        };
        self.modal = None;
        match self.program.source(e) {
            Some((path, line)) => cx.act(Action::OpenSource { path, line }),
            None => cx.log_error(format!(
                "{}: no source on disk (needs debug info and the file it was built from; \
                 externals have none)",
                node.label
            )),
        }
    }

    fn on_key(&mut self, key: Key, mods: crate::event::Modifiers, cx: &mut Cx) -> bool {
        match key {
            Key::Letter('G') if mods.ctrl && mods.shift => {
                self.load_synthetic(SYNTHETIC_NODES, cx);
            }
            Key::Letter('B') if mods.ctrl && mods.shift => self.start_bench(cx),
            _ if mods.ctrl || mods.alt => return false,
            Key::Letter('F') => self.view.fit_and_follow(),
            Key::Escape => {
                if !self.view.clear_selection() && !self.go_up() {
                    return false;
                }
            }
            Key::Backspace => {
                if !self.go_up() {
                    return false;
                }
            }
            Key::Space => self.view.relayout(),
            Key::Left => self.view.pan_by(Vec2::new(KEY_PAN, 0.0)),
            Key::Right => self.view.pan_by(Vec2::new(-KEY_PAN, 0.0)),
            Key::Up => self.view.pan_by(Vec2::new(0.0, KEY_PAN)),
            Key::Down => self.view.pan_by(Vec2::new(0.0, -KEY_PAN)),
            _ => return false,
        }
        cx.repaint();
        true
    }

    fn status(&self) -> String {
        let m = self.view.model();
        let mut s = format!(
            "{} nodes · {} edges",
            group_digits(m.nodes().len()),
            group_digits(m.edges().len())
        );
        if self.synthetic {
            s.push_str(" · synthetic");
        } else {
            s.push_str(&format!(" · {} objects", self.graphs.len()));
        }
        if m.hidden > 0 {
            s.push_str(&format!(" · {} hidden", group_digits(m.hidden)));
        }
        if self.folder_loading {
            s.push_str(" · loading…");
        } else if self.view.is_animating() {
            s.push_str(" · layout running");
        }
        s.push_str(&format!(
            " · {:.0}% · {:.1} ms",
            self.view.zoom() * 100.0,
            self.last_draw.as_secs_f32() * 1000.0
        ));
        s
    }

    fn paint_toolbar(&self, d: &mut DrawCtx, theme: &Theme) {
        d.fill_rect(self.toolbar, theme.panel_bg);
        d.fill_rect(
            RectF::new(
                self.toolbar.x,
                self.toolbar.bottom() - 1.0,
                self.toolbar.w,
                1.0,
            ),
            theme.border,
        );
        for &(b, r) in &self.buttons {
            let look = ButtonLook {
                enabled: b != Button::Build || self.can_build,
                primary: b == Button::AddFolder,
                on: false,
                hover: self.hover_button == Some(b),
            };
            paint_tool_button(d, theme, r, b.label(self.building), look);
        }
        let status = self.status();
        let w = d.text_system().measure(TextStyle::UiSmall, &status);
        let after = self
            .buttons
            .last()
            .map_or(self.toolbar.x, |(_, r)| r.right())
            + 16.0;
        let x = (self.toolbar.right() - w - 14.0).max(after);
        d.text(
            TextStyle::UiSmall,
            &status,
            RectF::new(
                x,
                self.toolbar.y,
                self.toolbar.right() - x - 8.0,
                self.toolbar.h,
            ),
            theme.text_dim,
        );
    }

    fn paint_empty(&self, d: &mut DrawCtx, theme: &Theme) {
        let (title, body) = if self.graphs.is_empty() {
            (
                "No object files loaded".to_string(),
                "Click a .obj or .o file in the Explorer, or use Add folder objects to load \
                 every object file under the open folder."
                    .to_string(),
            )
        } else if self.view.model().hidden > 0 {
            (
                "Nothing to show".to_string(),
                format!(
                    "All {} nodes here are hidden in Abstracted mode. Switch to Verbose \
                     under Visualizers \u{203a} Call graph \u{203a} Mode.",
                    self.view.model().hidden
                ),
            )
        } else {
            (
                "Nothing to show".to_string(),
                "The loaded objects contain no symbols.".to_string(),
            )
        };
        let r = self.canvas;
        let mut y = r.y + r.h / 2.0 - 24.0;
        for (style, text, color) in [
            (TextStyle::UiBold, title.as_str(), theme.text),
            (TextStyle::Ui, body.as_str(), theme.text_dim),
        ] {
            let w = d.text_system().measure(style, text).min(r.w - 32.0);
            d.text(
                style,
                text,
                RectF::new(r.x + ((r.w - w) / 2.0).max(16.0), y, w + 4.0, 24.0),
                color,
            );
            y += 26.0;
        }
    }

    /// What a click does here, shown after the breadcrumb.
    fn hint(&self) -> &'static str {
        match self.level {
            Level::Files => "Click a file to see its classes and functions",
            Level::File(_) => {
                "Click a class or function to look inside \u{b7} the file itself for details"
            }
            Level::Class(_) | Level::Function(_) => {
                "Click anything for details \u{b7} Backspace goes back"
            }
        }
    }

    /// `Files › shapes.cpp › Square` over the top left of the canvas: each
    /// segment goes back to its level; the last shows its details.
    fn paint_crumbs(&mut self, d: &mut DrawCtx, theme: &Theme) {
        self.crumbs.clear();
        if self.synthetic || self.graphs.is_empty() {
            return;
        }
        let text = d.text_system();
        let mut x = self.canvas.x + 12.0;
        let y = self.canvas.y + 10.0;
        let sep = 18.0;
        for (name, level) in self.program.breadcrumb(self.level) {
            let w = text.measure(TextStyle::Ui, &name) + 2.0 * CRUMB_PAD;
            self.crumbs
                .push((name, level, RectF::new(x, y, w, CRUMB_H)));
            x += w + sep;
        }
        let hint = self.hint();
        let hint_x = x - sep + 12.0;
        let hint_w = text.measure(TextStyle::UiSmall, hint);
        let all = RectF::new(
            self.canvas.x + 8.0,
            y - 3.0,
            (hint_x + hint_w + 12.0 - self.canvas.x - 8.0).min(self.canvas.w - 16.0),
            CRUMB_H + 6.0,
        );
        d.fill_round_rect(all, 7.0, theme.graph_card_bg);
        let last = self.crumbs.len().saturating_sub(1);
        for (k, (name, _, r)) in self.crumbs.iter().enumerate() {
            let hover = self.hover_crumb == Some(k);
            if k == last {
                d.fill_round_rect(*r, 5.0, theme.selection_bg);
            } else if hover {
                d.fill_round_rect(*r, 5.0, theme.hover_bg);
            }
            let color = if k == last {
                theme.accent
            } else if hover {
                theme.text
            } else {
                theme.text_dim
            };
            d.text(
                TextStyle::Ui,
                name,
                RectF::new(r.x + CRUMB_PAD, r.y, r.w - CRUMB_PAD, r.h),
                color,
            );
            if k < last {
                d.text(
                    TextStyle::Ui,
                    "\u{203a}",
                    RectF::new(r.right() + 5.0, r.y, 10.0, r.h),
                    theme.text_dim,
                );
            }
        }
        d.text(
            TextStyle::UiSmall,
            hint,
            RectF::new(hint_x, y, (all.right() - hint_x).max(0.0), CRUMB_H),
            theme.text_dim,
        );
    }

    /// The colour of each node and edge kind on this level, bottom left.
    fn paint_legend(&self, d: &mut DrawCtx, theme: &Theme) {
        let m = self.view.model();
        // Field and Variable share a colour; list each colour once.
        let nodes = NodeClass::ALL
            .into_iter()
            .filter(|&c| {
                c != NodeClass::Field || !m.nodes().iter().any(|n| n.class == NodeClass::Variable)
            })
            .filter(|&c| m.nodes().iter().any(|n| n.class == c));
        let edges = EdgeClass::ALL
            .into_iter()
            .filter(|&c| m.edges().iter().any(|e| e.class == c));
        let rows: Vec<(&str, Color, bool)> = nodes
            .map(|c| (c.describe(), node_color(theme, c), false))
            .chain(edges.map(|c| (c.describe(), edge_color(theme, c), true)))
            .collect();
        if self.synthetic || rows.is_empty() {
            return;
        }
        const ROW: f32 = 18.0;
        const SWATCH: f32 = 18.0;
        let text = d.text_system();
        let w = rows
            .iter()
            .map(|r| text.measure(TextStyle::UiSmall, r.0))
            .fold(0.0, f32::max)
            + SWATCH
            + 24.0;
        let h = rows.len() as f32 * ROW + 12.0;
        let card = RectF::new(self.canvas.x + 8.0, self.canvas.bottom() - h - 8.0, w, h);
        if card.y < self.canvas.y + CRUMB_H + 16.0 {
            return;
        }
        d.fill_round_rect(card, 7.0, theme.graph_card_bg);
        for (k, &(name, color, edge)) in rows.iter().enumerate() {
            let y = card.y + 6.0 + k as f32 * ROW;
            let mid = y + ROW / 2.0;
            let x = card.x + 10.0;
            if edge {
                d.line(
                    PointF::new(x, mid),
                    PointF::new(x + SWATCH, mid),
                    color,
                    1.5,
                );
            } else {
                let r = 4.0;
                let disc = RectF::new(x + SWATCH / 2.0 - r, mid - r, 2.0 * r, 2.0 * r);
                d.fill_round_rect(disc, r, color);
            }
            let tx = x + SWATCH + 6.0;
            d.text(
                TextStyle::UiSmall,
                name,
                RectF::new(tx, y, card.right() - tx, ROW),
                theme.text_dim,
            );
        }
    }

    fn crumb_at(&self, p: PointF) -> Option<usize> {
        if self.modal.is_some() {
            return None;
        }
        self.crumbs.iter().position(|(_, _, r)| r.contains(p))
    }

    fn press_crumb(&mut self, k: usize, cx: &mut Cx) {
        let Some(&(_, level, _)) = self.crumbs.get(k) else {
            return;
        };
        if k + 1 == self.crumbs.len() {
            let subject = match level {
                Level::Files => None,
                Level::File(id) | Level::Class(id) | Level::Function(id) => self.program.entity(id),
            };
            if let Some(e) = subject {
                self.show_details(e);
            }
        } else {
            self.go(level);
        }
        cx.repaint();
    }

    /// Routes an event to the details window while it is open. Returns
    /// whether it was handled there.
    fn modal_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let Some(modal) = self.modal.as_mut() else {
            return false;
        };
        // The second click of a double-click outside the card goes on to
        // the canvas (it opens the source).
        if let Event::MouseDown { pos, clicks, .. } = *e {
            if clicks >= 2 && !modal.contains(pos) {
                self.modal = None;
                cx.repaint();
                return false;
            }
        }
        let Some(action) = modal.on_event(e, cx) else {
            return false;
        };
        match action {
            ModalAction::None => {}
            ModalAction::Close => {
                self.modal = None;
                cx.repaint();
            }
            ModalAction::Open(path, line) => {
                self.modal = None;
                cx.act(Action::OpenSource { path, line });
                cx.repaint();
            }
        }
        true
    }

    fn leave_canvas(&mut self, cx: &mut Cx) {
        let r = self.view.pointer_leave();
        self.apply(r, cx);
    }

    fn button_at(&self, p: PointF) -> Option<Button> {
        self.buttons
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|&(b, _)| b)
    }
}

impl Panel for Visualizer {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        self.rect = rect;
        let (toolbar, canvas) = rect.split_top(TOOLBAR_H);
        self.toolbar = toolbar;
        self.canvas = canvas;
        self.buttons.clear();
        let y = toolbar.y + (toolbar.h - 1.0 - BUTTON_H) / 2.0;
        let mut x = toolbar.x + 10.0;
        for b in Button::ALL {
            // Build keeps one width so the buttons after it stay put.
            let w = text.measure(TextStyle::Ui, b.label(true)) + 2.0 * BUTTON_PAD;
            self.buttons.push((b, RectF::new(x, y, w, BUTTON_H)));
            x += w + BUTTON_GAP;
        }
        self.view.set_viewport(to_rect(canvas));
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        self.bench_step();
        d.fill_rect(self.canvas, theme.graph_bg);
        self.paint_toolbar(d, theme);
        if self.view.model().is_empty() {
            self.paint_empty(d, theme);
            self.paint_crumbs(d, theme);
            return;
        }
        let style = graph_style(theme);
        let view = &mut self.view;
        let started = Instant::now();
        d.clip(self.canvas, |d| view.paint(d, &style));
        if let Some(bench) = self.bench.as_mut() {
            bench.records.push(started.elapsed().as_secs_f32() * 1000.0);
        }
        self.paint_crumbs(d, theme);
        self.paint_legend(d, theme);
        if let Some(modal) = self.modal.as_mut() {
            modal.layout(self.canvas, d.text_system());
            modal.paint(d, theme);
        }
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let at = |p: PointF| Vec2::new(p.x, p.y);
        let toolbar = match *e {
            Event::MouseDown { pos, .. } | Event::MouseMove { pos, .. } => {
                self.toolbar.contains(pos)
            }
            _ => false,
        };
        if !toolbar && self.modal_event(e, cx) {
            return true;
        }
        if let Event::MouseMove { pos, .. } = *e {
            let hover = self.crumb_at(pos);
            if hover != self.hover_crumb {
                self.hover_crumb = hover;
                cx.repaint();
            }
            if hover.is_some() && !self.canvas_capture {
                self.leave_canvas(cx);
                cx.cursor = CursorShape::Hand;
                return true;
            }
        }
        if let Event::MouseDown {
            pos,
            button: MouseButton::Left,
            clicks,
            ..
        } = *e
        {
            if let Some(k) = self.crumb_at(pos) {
                self.press_crumb(k, cx);
                return true;
            }
            if clicks >= 2 && std::mem::take(&mut self.swallow_double) {
                return true;
            }
            self.swallow_double = false;
        }
        match *e {
            Event::KeyDown { key, mods } => return self.on_key(key, mods, cx),
            Event::Char(ch @ ('+' | '=' | '-')) => {
                self.view.zoom_by(if ch == '-' { 1.0 / 1.25 } else { 1.25 });
                cx.repaint();
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                clicks,
                ..
            } => {
                if let Some(b) = self.button_at(pos) {
                    self.press(b, cx);
                } else if self.canvas.contains(pos) {
                    self.canvas_capture = true;
                    let r = self.view.pointer_down(at(pos), clicks);
                    self.apply(r, cx);
                }
            }
            Event::MouseMove { pos, left_down, .. } => {
                if self.canvas_capture || self.canvas.contains(pos) {
                    let down = left_down && self.canvas_capture;
                    let r = self.view.pointer_move(at(pos), down);
                    self.apply(r, cx);
                    if self.hover_button.take().is_some() {
                        cx.repaint();
                    }
                } else {
                    self.leave_canvas(cx);
                    let hover = self.button_at(pos);
                    if hover != self.hover_button {
                        self.hover_button = hover;
                        cx.repaint();
                    }
                    if hover.is_some() {
                        cx.cursor = CursorShape::Hand;
                    }
                }
            }
            Event::MouseUp {
                pos,
                button: MouseButton::Left,
                ..
            } if self.canvas_capture => {
                self.canvas_capture = false;
                let r = self.view.pointer_up(at(pos));
                self.apply(r, cx);
            }
            Event::Wheel {
                pos,
                lines,
                horizontal: false,
                ..
            } if self.canvas.contains(pos) => {
                let r = self.view.wheel(at(pos), lines);
                self.apply(r, cx);
            }
            Event::MouseLeave => {
                self.leave_canvas(cx);
                if self.hover_button.take().is_some() {
                    cx.repaint();
                }
            }
            _ => return false,
        }
        true
    }
}

pub(crate) fn rgba(c: Color) -> Rgba {
    Rgba::new(c.r, c.g, c.b, c.a)
}

pub(crate) fn to_rect(r: RectF) -> renderer::Rect {
    renderer::Rect::new(r.x, r.y, r.w, r.h)
}

fn node_color(t: &Theme, class: NodeClass) -> Color {
    match class {
        NodeClass::File => t.graph_file,
        NodeClass::Class => t.graph_class,
        NodeClass::Function => t.graph_function,
        NodeClass::Variable | NodeClass::Field => t.graph_variable,
        NodeClass::Local => t.graph_local,
        NodeClass::External => t.graph_external,
        NodeClass::Library => t.graph_library,
        NodeClass::Generated => t.graph_generated,
    }
}

fn edge_color(t: &Theme, class: EdgeClass) -> Color {
    match class {
        EdgeClass::Includes => t.graph_includes,
        EdgeClass::Calls => t.graph_calls,
        EdgeClass::References => t.graph_refs,
        EdgeClass::Structure => t.graph_structure,
    }
}

fn graph_style(t: &Theme) -> Style {
    let node = NodeClass::ALL.map(|c| rgba(node_color(t, c)));
    let edge = EdgeClass::ALL.map(|c| rgba(edge_color(t, c)));
    Style {
        node,
        edge,
        highlight: rgba(t.accent),
        hover: rgba(t.graph_hover),
        label: rgba(t.text),
        label_dim: rgba(t.text_dim),
        card_bg: rgba(t.graph_card_bg),
        card_border: rgba(t.border),
        dim: t.graph_dim,
    }
}

pub(crate) fn group_digits(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

fn percentile(v: &[f32], q: f32) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s.get(((s.len() as f32 - 1.0) * q).round().max(0.0) as usize)
        .copied()
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(10_000), "10,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn percentiles() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&v, 0.5), 3.0);
        assert_eq!(percentile(&v, 1.0), 5.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}
