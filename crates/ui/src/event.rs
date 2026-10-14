//! Input events in DIPs, translated from window messages by the app.

use crate::geom::PointF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        shift: false,
        alt: false,
    };
    pub const CTRL: Self = Self {
        ctrl: true,
        shift: false,
        alt: false,
    };
    pub const SHIFT: Self = Self {
        ctrl: false,
        shift: true,
        alt: false,
    };
    pub const CTRL_SHIFT: Self = Self {
        ctrl: true,
        shift: true,
        alt: false,
    };
}

/// Non-text keys plus letters/digits for shortcuts. Text input arrives
/// separately as [`Event::Char`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Backspace,
    Tab,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Function key F1–F24.
    F(u8),
    /// `A`–`Z`, always uppercase.
    Letter(char),
    /// `0`–`9` on the main row.
    Digit(u8),
    /// The key left of `1` (backtick on US layouts).
    Backquote,
    Space,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    MouseDown {
        pos: PointF,
        button: MouseButton,
        mods: Modifiers,
        /// 1 for a single click, 2 for a double click.
        clicks: u8,
    },
    MouseUp {
        pos: PointF,
        button: MouseButton,
        mods: Modifiers,
    },
    MouseMove {
        pos: PointF,
        mods: Modifiers,
        left_down: bool,
    },
    MouseLeave,
    /// `lines` is positive when the wheel moves away from the user
    /// (content scrolls toward the start).
    Wheel {
        pos: PointF,
        lines: f32,
        horizontal: bool,
        mods: Modifiers,
    },
    KeyDown {
        key: Key,
        mods: Modifiers,
    },
    /// Text input (never a control character).
    Char(char),
    /// The window gained (`true`) or lost keyboard focus.
    Focus(bool),
}

impl Event {
    /// Mouse position, for events that have one.
    pub fn pos(&self) -> Option<PointF> {
        match *self {
            Event::MouseDown { pos, .. }
            | Event::MouseUp { pos, .. }
            | Event::MouseMove { pos, .. }
            | Event::Wheel { pos, .. } => Some(pos),
            _ => None,
        }
    }
}

/// Mouse cursor a panel wants while the pointer is over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Arrow,
    IBeam,
    Hand,
    SizeWE,
    SizeNS,
}
