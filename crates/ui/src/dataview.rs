//! The Data tab beside the call graph: the live data of a program stopped
//! under GDB, drawn by shape (trees as trees, arrays as cells). It only shows
//! what the debugger stopped on; it has no debugging controls of its own.

use renderer::data::{DataStyle, DataView};
use renderer::{Cursor, Vec2};

use crate::debugview::open_frame_source;
use crate::event::{CursorShape, Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::panel::{Cx, Panel};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;
use crate::visualizer::{
    group_digits, paint_tool_button, rgba, to_rect, ButtonLook, BUTTON_H, BUTTON_PAD, KEY_PAN,
    TOOLBAR_H,
};

#[derive(Default)]
pub struct DataPanel {
    data: DataView,
    toolbar: RectF,
    canvas: RectF,
    fit: RectF,
    hover_fit: bool,
    /// The canvas owns the mouse between a press and its release.
    canvas_capture: bool,
}

impl DataPanel {
    /// A new stop's data.
    pub fn set_snapshot(&mut self, snap: debugger::Snapshot) {
        self.data.set_snapshot(snap);
    }

    /// Focuses and centres a frame's box (a call stack click).
    pub fn reveal_frame(&mut self, i: usize) {
        self.data.reveal_frame(i);
    }

    /// The program is running (or ended): the data shown is from the last
    /// stop.
    pub fn set_stale(&mut self, stale: bool) {
        self.data.set_stale(stale);
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// A transition is under way: wants another frame right away.
    pub fn is_animating(&self) -> bool {
        self.data.is_animating()
    }

    fn apply(&mut self, r: renderer::data::DataResponse, cx: &mut Cx) {
        if r.repaint {
            cx.repaint();
        }
        cx.cursor = match r.cursor {
            Cursor::Arrow => CursorShape::Arrow,
            Cursor::Hand => CursorShape::Hand,
            Cursor::Move => CursorShape::Move,
        };
        if let Some((file, line)) = r.open {
            open_frame_source(&file, line, cx);
        }
    }

    fn on_key(&mut self, key: Key, mods: crate::event::Modifiers, cx: &mut Cx) -> bool {
        if mods.ctrl || mods.alt {
            return false;
        }
        match key {
            Key::Letter('F') => self.data.fit(),
            Key::Escape => {
                self.data.clear_focus();
            }
            Key::Left => self.data.pan_by(Vec2::new(KEY_PAN, 0.0)),
            Key::Right => self.data.pan_by(Vec2::new(-KEY_PAN, 0.0)),
            Key::Up => self.data.pan_by(Vec2::new(0.0, KEY_PAN)),
            Key::Down => self.data.pan_by(Vec2::new(0.0, -KEY_PAN)),
            _ => return false,
        }
        cx.repaint();
        true
    }

    fn status(&self) -> String {
        let mut s = String::new();
        if let Some(snap) = self.data.snapshot() {
            s = format!("{} objects", group_digits(snap.objects.len()));
            if snap.truncated {
                s.push_str(" (capped)");
            }
            s.push_str(" \u{b7} ");
        }
        s.push_str(&format!("{:.0}%", self.data.zoom() * 100.0));
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
        let look = ButtonLook {
            enabled: true,
            primary: false,
            on: false,
            hover: self.hover_fit,
        };
        paint_tool_button(d, theme, self.fit, "Fit", look);
        let status = self.status();
        let w = d.text_system().measure(TextStyle::UiSmall, &status);
        let x = (self.toolbar.right() - w - 14.0).max(self.fit.right() + 16.0);
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
        let r = self.canvas;
        let mut y = r.y + r.h / 2.0 - 24.0;
        for (text, style, color) in [
            ("No program data", TextStyle::UiBold, theme.text),
            (
                "Run a program from the Debugger tab; its data shows here at each stop.",
                TextStyle::Ui,
                theme.text_dim,
            ),
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

    fn leave_canvas(&mut self, cx: &mut Cx) {
        let r = self.data.pointer_leave();
        self.apply(r, cx);
    }
}

impl Panel for DataPanel {
    fn layout(&mut self, rect: RectF, text: &TextSystem) {
        let (toolbar, canvas) = rect.split_top(TOOLBAR_H);
        self.toolbar = toolbar;
        self.canvas = canvas;
        let y = toolbar.y + (toolbar.h - 1.0 - BUTTON_H) / 2.0;
        let w = text.measure(TextStyle::Ui, "Fit") + 2.0 * BUTTON_PAD;
        self.fit = RectF::new(toolbar.x + 10.0, y, w, BUTTON_H);
        self.data.set_viewport(to_rect(canvas));
    }

    fn paint(&mut self, d: &mut DrawCtx, theme: &Theme, _focused: bool) {
        d.fill_rect(self.canvas, theme.graph_bg);
        // Canvas first: it lays out a new stop, which the status reads.
        if self.data.is_empty() {
            self.paint_empty(d, theme);
        } else {
            let style = data_style(theme);
            let data = &mut self.data;
            d.clip(self.canvas, |d| data.paint(d, &style));
        }
        self.paint_toolbar(d, theme);
    }

    fn on_event(&mut self, e: &Event, cx: &mut Cx) -> bool {
        let at = |p: PointF| Vec2::new(p.x, p.y);
        match *e {
            Event::KeyDown { key, mods } => return self.on_key(key, mods, cx),
            Event::Char(ch @ ('+' | '=' | '-')) => {
                self.data.zoom_by(if ch == '-' { 1.0 / 1.25 } else { 1.25 });
                cx.repaint();
            }
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                clicks,
                ..
            } => {
                if self.fit.contains(pos) {
                    self.data.fit();
                    cx.repaint();
                } else if self.canvas.contains(pos) {
                    self.canvas_capture = true;
                    let r = self.data.pointer_down(at(pos), clicks);
                    self.apply(r, cx);
                }
            }
            Event::MouseMove { pos, left_down, .. } => {
                let hover = !self.canvas_capture && self.fit.contains(pos);
                if hover != self.hover_fit {
                    self.hover_fit = hover;
                    cx.repaint();
                }
                if self.canvas_capture || self.canvas.contains(pos) {
                    let down = left_down && self.canvas_capture;
                    let r = self.data.pointer_move(at(pos), down);
                    self.apply(r, cx);
                } else {
                    self.leave_canvas(cx);
                    if hover {
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
                let r = self.data.pointer_up(at(pos));
                self.apply(r, cx);
            }
            Event::Wheel {
                pos,
                lines,
                horizontal: false,
                ..
            } if self.canvas.contains(pos) => {
                let r = self.data.wheel(at(pos), lines);
                self.apply(r, cx);
            }
            Event::MouseLeave => {
                self.leave_canvas(cx);
                if std::mem::take(&mut self.hover_fit) {
                    cx.repaint();
                }
            }
            _ => return false,
        }
        true
    }
}

fn data_style(t: &Theme) -> DataStyle {
    DataStyle {
        canvas_bg: rgba(t.graph_bg),
        frame_bg: rgba(t.data_frame_bg),
        header_bg: rgba(t.data_header_bg),
        box_bg: rgba(t.data_box_bg),
        border: rgba(t.data_border),
        grid_line: rgba(t.data_grid),
        text: rgba(t.text),
        text_dim: rgba(t.text_dim),
        pointer: rgba(t.data_pointer),
        cross: rgba(t.data_cross),
        changed: rgba(t.data_changed),
        highlight: rgba(t.accent),
        hover: rgba(t.graph_hover),
        chip_bg: rgba(t.graph_card_bg),
        chip_border: rgba(t.border),
        dim: 0.3,
    }
}
