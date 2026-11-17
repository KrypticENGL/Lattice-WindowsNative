//! Panel trait and the per-event context panels report through.

use std::path::PathBuf;

use crate::event::{CursorShape, Event, Key, Modifiers};
use crate::geom::{PointF, RectF};
use crate::memory::MemTarget;
use crate::menubar::Entry;
use crate::replay::ReplayAction;
use crate::sys::gfx::DrawCtx;
use crate::sys::text::TextSystem;
use crate::sys::NativeWindow;
use crate::theme::Theme;

/// Requests a panel cannot fulfil itself; the shell handles them after the
/// event returns.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    OpenFile(PathBuf),
    /// Load (or retry) an object file with the engine.
    LoadObject(PathBuf),
    /// Load every object file under the open folder (Visualizer).
    LoadFolderObjects,
    /// Show a source file in the Code tab with the caret on `line` (1-based).
    OpenSource {
        path: PathBuf,
        line: u32,
    },
    PickFolder,
    /// Debugger controls (Debugger toolbar, F5/F10/F11).
    Debug(DebugAction),
    /// Add or remove a breakpoint (gutter click).
    ToggleBreakpoint {
        path: PathBuf,
        line: u32,
    },
    Log {
        error: bool,
        text: String,
    },
    /// Select a frame in the debugger, and its box in the Data view (call stack
    /// click).
    RevealFrame(usize),
    /// A play bar command.
    Replay(ReplayAction),
    /// Open the Memory panel on a variable (hover card click).
    ShowMemory(MemTarget),
    /// Open a right-click menu; a picked row queues its action.
    ContextMenu {
        pos: PointF,
        items: Vec<Entry<Action>>,
    },
    /// Press a key in the focused panel (menu rows reuse its shortcuts).
    Key(Key, Modifiers),
    /// Open the editor's Find or Find/Replace bar.
    Find(bool),
    /// Open the editor's Go to Line prompt.
    GoToLine,
    CopyText(String),
    /// Show a file or folder in Windows File Explorer.
    Reveal(PathBuf),
    CloseTab(usize),
    /// Close every editor tab but this one, or all of them.
    CloseTabsExcept(Option<usize>),
    ClearOutput,
    /// Ask for the name of a new file or folder, in `dir` or else in the
    /// folder of the Explorer's selection.
    NewEntry {
        folder: bool,
        dir: Option<PathBuf>,
    },
    /// Ask for a new name for a file or folder.
    BeginRename(PathBuf),
    CreatePath {
        path: PathBuf,
        folder: bool,
    },
    RenamePath {
        from: PathBuf,
        to: PathBuf,
    },
    /// Move a file or folder to another folder (dragged in the Explorer).
    MovePath {
        from: PathBuf,
        to: PathBuf,
    },
    /// Confirm, then move a file or folder to the Recycle Bin.
    DeletePath(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugAction {
    /// Start the program, or continue it when stopped.
    Run,
    /// Play or pause the replay, recording the program first if there is
    /// none.
    Play,
    Pause,
    Step(debugger::Command),
    Stop,
    PickProgram,
    /// Compile the open folder into `debug\` and link `target\<name>.exe`.
    Build,
}

/// Context for one event dispatch.
pub struct Cx<'a> {
    pub text: &'a TextSystem,
    pub window: NativeWindow,
    /// Cursor to show; panels set it on mouse moves.
    pub cursor: CursorShape,
    repaint: bool,
    actions: Vec<Action>,
}

impl<'a> Cx<'a> {
    pub fn new(text: &'a TextSystem, window: NativeWindow) -> Self {
        Self {
            text,
            window,
            cursor: CursorShape::Arrow,
            repaint: false,
            actions: Vec::new(),
        }
    }

    pub fn repaint(&mut self) {
        self.repaint = true;
    }

    pub fn needs_repaint(&self) -> bool {
        self.repaint
    }

    pub fn act(&mut self, action: Action) {
        self.actions.push(action);
    }

    pub fn log(&mut self, text: impl Into<String>) {
        self.act(Action::Log {
            error: false,
            text: text.into(),
        });
    }

    pub fn log_error(&mut self, text: impl Into<String>) {
        self.act(Action::Log {
            error: true,
            text: text.into(),
        });
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }
}

pub trait Panel {
    /// Assigns the panel its rect. Called on resize and before each paint.
    fn layout(&mut self, rect: RectF, text: &TextSystem);
    /// Draws inside the rect from the last `layout`.
    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, focused: bool);
    /// Returns `true` if the event was consumed.
    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool;
}
