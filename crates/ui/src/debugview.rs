//! The Debugger tab's main panel: the toolbar that runs and steps the program,
//! over the source of the selected frame, with a play bar under it while
//! recording or replaying a run. **Play** records the whole run under GDB.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use debugger::Snapshot;

use crate::codepane::{CodePane, Target};
use crate::debug::Phase;
use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Action, Cx, DebugAction, Panel};
use crate::playbar::{BarState, PlayBar, PLAYBAR_H};
use crate::replay::{ReplayAction, ReplayStatus};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;
use crate::visualizer::{
    paint_tool_button, ButtonLook, BUTTON_GAP, BUTTON_H, BUTTON_PAD, TOOLBAR_H,
};

/// Debugger state shown by the toolbar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DebugStatus {
    pub phase: Phase,
    /// File name of the chosen program.
    pub program: Option<String>,
    /// `func at file:line` while stopped.
    pub stopped: Option<String>,
    /// Steps recorded so far, while recording a run for replay.
    pub recording: Option<usize>,
    /// The recorded run being replayed.
    pub replay: Option<ReplayStatus>,
    /// The open folder is being compiled.
    pub building: bool,
}

impl DebugStatus {
    /// What the play bar shows, if it shows.
    fn bar(&self) -> Option<BarState> {
        match (self.recording, self.replay) {
            (Some(n), _) => Some(BarState::Recording(n)),
            (None, Some(s)) => Some(BarState::Replay(s)),
            (None, None) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    Build,
    Program,
    Run,
    Play,
    Pause,
    StepOver,
    StepInto,
    StepOut,
    Stop,
}

impl Button {
    /// Everything but Run, which `layout` pins to the far right. A narrow
    /// panel drops from the end, and the steps have shortcuts; Stop stays.
    const LEFT: [Self; 8] = [
        Self::Build,
        Self::Program,
        Self::Play,
        Self::Pause,
        Self::Stop,
        Self::StepOver,
        Self::StepInto,
        Self::StepOut,
    ];

    fn label(self, debug: &DebugStatus) -> String {
        match self {
            Self::Build if debug.building => "Building\u{2026}".into(),
            Self::Build => "Build".into(),
            Self::Program => match &debug.program {
                Some(p) => format!("Program: {p}"),
                None => "Choose program\u{2026}".into(),
            },
            Self::Run => match debug.phase {
                Phase::Stopped => "Continue".into(),
                _ => "Run".into(),
            },
            Self::Play if debug.recording.is_some() => "Recording\u{2026}".into(),
            Self::Play => "Play".into(),
            Self::Pause => "Pause".into(),
            Self::StepOver => "Step over".into(),
            Self::StepInto => "Step into".into(),
            Self::StepOut => "Step out".into(),
            Self::Stop => "Stop".into(),
        }
    }

    fn enabled(self, debug: &DebugStatus) -> bool {
        match self {
            Self::Run => matches!(debug.phase, Phase::Idle | Phase::Stopped),
            Self::Play => {
                debug.recording.is_none() && matches!(debug.phase, Phase::Idle | Phase::Stopped)
            }
            Self::Pause => debug.phase == Phase::Running,
            Self::StepOver | Self::StepInto | Self::StepOut => debug.phase == Phase::Stopped,
            Self::Stop => debug.phase != Phase::Idle,
            Self::Program => debug.phase == Phase::Idle,
            // The running program's exe cannot be relinked.
            Self::Build => debug.phase == Phase::Idle && !debug.building,
        }
    }
}

#[derive(Default)]
pub struct DebugView {
    pub source: CodePane,
    snap: Option<Arc<Snapshot>>,
    /// The frame picked in the call stack; `None` is the innermost.
    selected: Option<usize>,
    debug: DebugStatus,
    toolbar: RectF,
    buttons: Vec<(Button, RectF)>,
    hover_button: Option<Button>,
    /// Under the source while recording or replaying.
    bar: PlayBar,
}

impl DebugView {
    /// Debugger state for the toolbar (set before each layout).
    pub fn set_status(&mut self, status: DebugStatus) {
        self.debug = status;
    }

    /// A new stop: the view goes back to the innermost frame.
    pub fn set_snapshot(&mut self, snap: Arc<Snapshot>) {
        self.snap = Some(snap);
        self.selected = None;
    }

    pub fn clear_snapshot(&mut self) {
        self.snap = None;
        self.selected = None;
    }

    pub fn select_frame(&mut self, i: usize) {
        self.selected = Some(i);
    }

    pub fn selected_frame(&self) -> Option<usize> {
        self.selected
    }

    /// The line the source shows: the selected frame, else the innermost.
    pub fn code_target(&self) -> Option<Target> {
        let frames = &self.snap.as_ref()?.frames;
        let innermost = frames.len().checked_sub(1)?;
        let i = self
            .selected
            .filter(|&i| i < frames.len())
            .unwrap_or(innermost);
        let f = &frames[i];
        Some(Target {
            file: PathBuf::from(&f.file),
            line: f.line,
            innermost: i == innermost,
            func: f.func.clone(),
        })
    }

    fn toggled(&self, b: Button) -> bool {
        b == Button::Play
            && (self.debug.recording.is_some() || self.debug.replay.is_some_and(|r| r.playing))
    }

    fn press(&mut self, b: Button, cx: &mut Cx) {
        if !b.enabled(&self.debug) {
            return;
        }
        let action = match b {
            Button::Program => DebugAction::PickProgram,
            Button::Build => DebugAction::Build,
            Button::Run => DebugAction::Run,
            Button::Play => DebugAction::Play,
            Button::Pause => DebugAction::Pause,
            Button::StepOver => DebugAction::Step(debugger::Command::Next),
            Button::StepInto => DebugAction::Step(debugger::Command::Step),
            Button::StepOut => DebugAction::Step(debugger::Command::Finish),
            Button::Stop => DebugAction::Stop,
        };
        cx.act(Action::Debug(action));
        cx.repaint();
    }

    /// Media player keys while a replay is loaded.
    fn replay_key(&self, key: Key, mods: crate::event::Modifiers) -> Option<ReplayAction> {
        if mods.ctrl || mods.alt || self.debug.recording.is_some() {
            return None;
        }
        let s = self.debug.replay?;
        match key {
            Key::Left => Some(ReplayAction::Step(-1)),
            Key::Right => Some(ReplayAction::Step(1)),
            Key::Space => Some(ReplayAction::Toggle),
            Key::Home => Some(ReplayAction::Seek(0)),
            Key::End => Some(ReplayAction::Seek(s.len - 1)),
            _ => None,
        }
    }

    fn status(&self) -> String {
        let replay_at = self.snap.as_ref().and_then(|s| s.frames.last()).map(|f| {
            let file = Path::new(&f.file)
                .file_name()
                .map_or_else(|| f.file.clone(), |n| n.to_string_lossy().into_owned());
            format!("{} at {file}:{}", f.func, f.line)
        });
        match (self.debug.phase, &self.debug.stopped) {
            _ if self.debug.recording.is_some() => format!(
                "recording\u{2026} {} steps",
                self.debug.recording.unwrap_or(0)
            ),
            (Phase::Idle, _) if self.debug.replay.is_some() => match &replay_at {
                Some(at) => format!("replay \u{b7} {at}"),
                None => "replay".to_string(),
            },
            (Phase::Idle, _) => "not running".to_string(),
            (Phase::Starting, _) => "starting\u{2026}".to_string(),
            (Phase::Running, _) => "running\u{2026}".to_string(),
            (Phase::Stopped, Some(at)) => format!("stopped in {at}"),
            (Phase::Stopped, None) => "stopped".to_string(),
        }
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
                enabled: b.enabled(&self.debug),
                primary: b == Button::Run,
                on: self.toggled(b),
                hover: self.hover_button == Some(b),
            };
            paint_tool_button(d, theme, r, &b.label(&self.debug), look);
        }
        let status = self.status();
        let w = d.text_system().measure(TextStyle::UiSmall, &status);
        // Between the left-hand buttons and Run, which is always last.
        let end = self
            .buttons
            .last()
            .map_or(self.toolbar.right(), |(_, r)| r.x);
        let after = self
            .buttons
            .iter()
            .rev()
            .nth(1)
            .map_or(self.toolbar.x, |(_, r)| r.right())
            + 16.0;
        let x = (end - w - 14.0).max(after);
        // A crowded toolbar leaves too little room for any readable status.
        if end - x - 8.0 < 40.0 {
            return;
        }
        d.text(
            TextStyle::UiSmall,
            &status,
            RectF::new(x, self.toolbar.y, end - x - 8.0, self.toolbar.h),
            theme.text_dim,
        );
    }

    fn button_at(&self, p: PointF) -> Option<Button> {
        self.buttons
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|&(b, _)| b)
    }
}

impl Panel for DebugView {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        let (toolbar, mut body) = rect.split_top(TOOLBAR_H);
        self.toolbar = toolbar;
        if let Some(state) = self.debug.bar() {
            let h = PLAYBAR_H.min(body.h);
            let bar = RectF::new(body.x, body.bottom() - h, body.w, h);
            body.h -= h;
            self.bar.layout(bar, state, text);
        }
        self.source.layout(body, text);
        self.buttons.clear();
        let y = toolbar.y + (toolbar.h - 1.0 - BUTTON_H) / 2.0;
        let mut x = toolbar.x + 10.0;
        // Run/Continue stays at the far right; what doesn't fit before it is
        // dropped, so nothing sits clickable underneath.
        let run_w = text.measure(TextStyle::Ui, "Continue") + 2.0 * BUTTON_PAD;
        let run_x = (toolbar.right() - 10.0 - run_w).max(x);
        for b in Button::LEFT {
            if matches!(b, Button::Play | Button::StepOver) {
                x += 14.0; // separates groups
            }
            // Labels that change keep one width so the buttons after stay put.
            let label = match b {
                Button::Play => "Recording\u{2026}".to_string(),
                Button::Build => "Building\u{2026}".to_string(),
                _ => b.label(&self.debug),
            };
            let w = text.measure(TextStyle::Ui, &label) + 2.0 * BUTTON_PAD;
            if x + w + BUTTON_GAP > run_x {
                break;
            }
            self.buttons.push((b, RectF::new(x, y, w, BUTTON_H)));
            x += w + BUTTON_GAP;
        }
        self.buttons
            .push((Button::Run, RectF::new(run_x, y, run_w, BUTTON_H)));
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool) {
        self.source.paint(d, theme, focused);
        if let Some(state) = self.debug.bar() {
            self.bar.paint(d, theme, state);
        }
        self.paint_toolbar(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        if let Some(state) = self.debug.bar() {
            if self.bar.on_event(e, state, cx) {
                self.source.on_event(&Event::MouseLeave, cx);
                if self.hover_button.take().is_some() {
                    cx.repaint();
                }
                return true;
            }
            if matches!(e, Event::MouseMove { .. } | Event::MouseLeave) && self.bar.leave() {
                cx.repaint();
            }
        }
        match *e {
            Event::KeyDown { key, mods } => {
                if let Some(a) = self.replay_key(key, mods) {
                    cx.act(Action::Replay(a));
                    cx.repaint();
                    return true;
                }
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(b) = self.button_at(pos) {
                    self.press(b, cx);
                    return true;
                }
            }
            Event::MouseMove { pos, .. } => {
                let hover = self.button_at(pos);
                if hover != self.hover_button {
                    self.hover_button = hover;
                    cx.repaint();
                }
                if self.toolbar.contains(pos) {
                    self.source.on_event(&Event::MouseLeave, cx);
                    if hover.is_some() {
                        cx.cursor = CursorShape::Hand;
                    }
                    return true;
                }
            }
            Event::MouseLeave if self.hover_button.take().is_some() => cx.repaint(),
            _ => {}
        }
        self.source.on_event(e, cx)
    }
}

/// Opens a GDB frame's source (absolute paths from DWARF).
pub(crate) fn open_frame_source(file: &str, line: u32, cx: &mut Cx) {
    let path = PathBuf::from(file);
    if path.is_file() {
        cx.act(Action::OpenSource { path, line });
    } else {
        cx.log_error(format!("Source file not found: {file}"));
    }
}
