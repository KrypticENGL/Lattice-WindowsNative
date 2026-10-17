//! The call stack beside the Data canvas: the stopped program's frames,
//! innermost first, rebuilt at every stop. Clicking a frame focuses and
//! centres its box on the canvas; double-clicking opens its source.

use debugger::{Cell, Frame};

use crate::event::{Event, MouseButton};
use crate::geom::{PointF, RectF};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::TextStyle;
use crate::theme::Theme;
use crate::visualizer::rgba;

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 40.0;
const PAD: f32 = 10.0;
/// Room on the left of each row for the current-frame dot.
const GUTTER: f32 = 22.0;
/// Text alpha while the program runs and the stack is from the last stop.
const STALE: f32 = 0.55;

/// What a click in the call stack asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackAction {
    /// Focus and centre the box of `snapshot.frames[i]`.
    Reveal(usize),
    /// Open a frame's source at the line.
    Open(String, u32),
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    /// Index into `snapshot.frames` (outermost is 0).
    frame: usize,
    func: String,
    /// `file.cpp:31`.
    place: String,
    /// `node = 0x4a2e0, key = 5`.
    args: String,
    file: String,
    line: u32,
}

#[derive(Default)]
pub struct CallStack {
    rect: RectF,
    list: RectF,
    /// Innermost frame first.
    rows: Vec<Row>,
    scroll: f32,
    hover: Option<usize>,
}

impl CallStack {
    /// The frames of a new stop (outermost first, as GDB's snapshot has
    /// them).
    pub fn set_frames(&mut self, frames: &[Frame]) {
        self.rows = frames
            .iter()
            .enumerate()
            .rev()
            .map(|(i, f)| Row {
                frame: i,
                func: f.func.clone(),
                place: format!("{}:{}", file_name(&f.file), f.line),
                args: f
                    .vars
                    .iter()
                    .filter(|v| v.arg)
                    .map(|v| format!("{} = {}", v.name, value_text(&v.value)))
                    .collect::<Vec<_>>()
                    .join(", "),
                file: f.file.clone(),
                line: f.line,
            })
            .collect();
        self.hover = None;
        self.clamp_scroll();
    }

    pub fn clear(&mut self) {
        *self = Self {
            rect: self.rect,
            list: self.list,
            ..Self::default()
        };
    }

    pub fn rect(&self) -> RectF {
        self.rect
    }

    pub fn layout(&mut self, rect: RectF) {
        self.rect = rect;
        self.list = rect.split_top(HEADER_H).1;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max = (self.rows.len() as f32 * ROW_H - self.list.h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    fn row_at(&self, p: PointF) -> Option<usize> {
        if !self.list.contains(p) {
            return None;
        }
        let i = ((p.y - self.list.y + self.scroll) / ROW_H) as usize;
        (i < self.rows.len()).then_some(i)
    }

    /// Clears the hover. Returns `true` if there was one.
    pub fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// Handles an event over the panel. Returns whether to repaint and
    /// what the host should do.
    pub fn on_event(&mut self, e: &Event) -> (bool, Option<StackAction>) {
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                clicks,
                ..
            } => {
                let Some(row) = self.row_at(pos).map(|i| &self.rows[i]) else {
                    return (false, None);
                };
                let action = if clicks >= 2 {
                    StackAction::Open(row.file.clone(), row.line)
                } else {
                    StackAction::Reveal(row.frame)
                };
                (true, Some(action))
            }
            Event::MouseMove { pos, .. } => {
                let hover = self.row_at(pos);
                let changed = hover != self.hover;
                self.hover = hover;
                (changed, None)
            }
            Event::Wheel { lines, .. } => {
                let before = self.scroll;
                self.scroll -= lines * 3.0 * ROW_H;
                self.clamp_scroll();
                (self.scroll != before, None)
            }
            _ => (false, None),
        }
    }

    /// `selected` is the frame focused on the canvas; `stale` dims the
    /// text while the program runs.
    pub fn paint(&self, d: &mut DrawCtx, theme: &Theme, selected: Option<usize>, stale: bool) {
        let r = self.rect;
        d.fill_rect(r, theme.panel_bg);
        let a = if stale { STALE } else { 1.0 };

        let header = RectF::new(r.x + PAD, r.y, r.w - 2.0 * PAD, HEADER_H);
        d.text(TextStyle::UiSmall, "CALL STACK", header, theme.text_dim);
        if !self.rows.is_empty() {
            let n = self.rows.len();
            let count = format!("{n} frame{}", if n == 1 { "" } else { "s" });
            let w = d.text_system().measure(TextStyle::UiSmall, &count);
            let at = RectF::new(header.right() - w, header.y, w + 2.0, header.h);
            d.text(TextStyle::UiSmall, &count, at, theme.text_dim);
        }

        let list = self.list;
        let rows = &self.rows;
        let (scroll, hover) = (self.scroll, self.hover);
        d.clip(list, |d| {
            if rows.is_empty() {
                let at = RectF::new(list.x + PAD, list.y + 4.0, list.w - 2.0 * PAD, 22.0);
                d.text(TextStyle::Ui, "No frames", at, theme.text_dim);
                return;
            }
            let lh = d.text_system().line_height(TextStyle::Ui);
            let lh_small = d.text_system().line_height(TextStyle::UiSmall);
            for (i, row) in rows.iter().enumerate() {
                let y = list.y + i as f32 * ROW_H - scroll;
                if y + ROW_H < list.y || y > list.bottom() {
                    continue;
                }
                let rr = RectF::new(list.x, y, list.w - 1.0, ROW_H);
                if selected == Some(row.frame) {
                    d.fill_rect(rr, theme.selection_bg);
                    d.fill_rect(RectF::new(rr.x, rr.y, 2.0, rr.h), theme.accent);
                } else if hover == Some(i) {
                    d.fill_rect(rr, theme.hover_bg);
                }
                let top = i == 0;
                let line1 = y + (ROW_H - lh - lh_small) / 2.0;
                if top {
                    // The frame the program is stopped in.
                    let c = renderer::Vec2::new(rr.x + GUTTER / 2.0 + 1.0, line1 + lh / 2.0);
                    renderer::Canvas::fill_discs(d, &[c], 3.5, rgba(theme.accent.with_alpha(a)));
                }
                let x = rr.x + GUTTER;
                let right = rr.right() - PAD;
                let place_w = d.text_system().measure(TextStyle::UiSmall, &row.place);
                let place_x = (right - place_w).max(x + 40.0);
                let func_style = if top {
                    TextStyle::UiBold
                } else {
                    TextStyle::Ui
                };
                let func_color = if top { theme.text_bright } else { theme.text };
                d.text(
                    func_style,
                    &row.func,
                    RectF::new(x, line1, place_x - x - 6.0, lh),
                    func_color.with_alpha(a),
                );
                d.text(
                    TextStyle::UiSmall,
                    &row.place,
                    RectF::new(place_x, line1, right - place_x + 2.0, lh),
                    theme.text_dim.with_alpha(a),
                );
                if !row.args.is_empty() {
                    d.text(
                        TextStyle::UiSmall,
                        &row.args,
                        RectF::new(x, line1 + lh, right - x, lh_small),
                        theme.text_dim.with_alpha(a),
                    );
                }
            }
        });
    }
}

fn value_text(c: &Cell) -> &str {
    match c {
        Cell::Scalar { v } | Cell::Pointer { v, .. } => v,
        Cell::Embedded { .. } => "{\u{2026}}",
    }
}

fn file_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    const WIDTH: f32 = 260.0;

    fn frames() -> Vec<Frame> {
        debugger::Snapshot::parse(
            r#"{"frames":[
                {"level":2,"func":"main","file":"C:\\src\\bst.cpp","line":52,"vars":[
                    {"name":"root","type":"Node *","value":{"k":"p","to":null,"v":"0x0"}}]},
                {"level":1,"func":"insert","file":"C:\\src\\bst.cpp","line":34,"vars":[
                    {"name":"node","arg":true,"type":"Node *","value":{"k":"p","to":null,"v":"0x4a2e0"}},
                    {"name":"key","arg":true,"type":"int","value":{"k":"s","v":"5"}}]},
                {"level":0,"func":"insert","file":"C:/src/bst.cpp","line":31,"vars":[
                    {"name":"node","arg":true,"type":"Node *","value":{"k":"p","to":null,"v":"0x0"}},
                    {"name":"v","type":"std::vector<int>","value":{"k":"o","to":"x"}}]}],
              "objects":{}}"#,
        )
        .unwrap()
        .frames
    }

    fn stack() -> CallStack {
        let mut s = CallStack::default();
        s.layout(RectF::new(0.0, 0.0, WIDTH, 400.0));
        s.set_frames(&frames());
        s
    }

    fn click(y: f32, clicks: u8) -> Event {
        Event::MouseDown {
            pos: PointF::new(50.0, y),
            button: MouseButton::Left,
            mods: Modifiers::default(),
            clicks,
        }
    }

    #[test]
    fn rows_are_innermost_first_with_arguments() {
        let s = stack();
        let rows: Vec<(usize, &str, &str, &str)> = s
            .rows
            .iter()
            .map(|r| (r.frame, &*r.func, &*r.place, &*r.args))
            .collect();
        assert_eq!(
            rows,
            [
                (2, "insert", "bst.cpp:31", "node = 0x0"),
                (1, "insert", "bst.cpp:34", "node = 0x4a2e0, key = 5"),
                (0, "main", "bst.cpp:52", ""),
            ]
        );
    }

    #[test]
    fn clicks_reveal_and_double_clicks_open() {
        let mut s = stack();
        let second = HEADER_H + ROW_H * 1.5;
        assert_eq!(
            s.on_event(&click(second, 1)),
            (true, Some(StackAction::Reveal(1)))
        );
        assert_eq!(
            s.on_event(&click(second, 2)),
            (true, Some(StackAction::Open(r"C:\src\bst.cpp".into(), 34)))
        );
        // Below the last row and on the header: nothing.
        assert_eq!(s.on_event(&click(HEADER_H + ROW_H * 3.5, 1)), (false, None));
        assert_eq!(s.on_event(&click(5.0, 1)), (false, None));
    }

    #[test]
    fn scrolling_is_clamped_to_the_rows() {
        let mut s = CallStack::default();
        s.layout(RectF::new(0.0, 0.0, WIDTH, HEADER_H + ROW_H * 2.0));
        s.set_frames(&frames());
        let wheel = |lines| Event::Wheel {
            pos: PointF::new(10.0, 50.0),
            lines,
            horizontal: false,
            mods: Modifiers::default(),
        };
        assert_eq!(s.on_event(&wheel(1.0)), (false, None), "already at the top");
        assert!(s.on_event(&wheel(-5.0)).0);
        assert_eq!(s.scroll, ROW_H, "one row past the panel");
        // A shorter stack at the next stop pulls the scroll back.
        s.set_frames(&frames()[..1]);
        assert_eq!(s.scroll, 0.0);
    }
}
