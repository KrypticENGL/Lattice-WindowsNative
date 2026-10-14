//! Panel trait and the per-event context panels report through.

use std::path::PathBuf;

use crate::event::{CursorShape, Event};
use crate::geom::RectF;
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
    PickFolder,
    Log {
        error: bool,
        text: String,
    },
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
