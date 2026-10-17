//! The menu bar along the top of the window: a row of titles, each opening
//! a dropdown whose rows may open one level of submenu to their right.
//!
//! The bar holds no app state. The shell describes each menu's entries
//! (with the current checks) and the bar reports which command was picked.

use crate::event::{Event, Key, MouseButton};
use crate::geom::{PointF, RectF};
use crate::sys::gfx::DrawCtx;
use crate::sys::text::{TextStyle, TextSystem};
use crate::theme::Theme;

const TITLE_PAD: f32 = 10.0;
const ROW_H: f32 = 26.0;
const HEADER_H: f32 = 24.0;
const SEPARATOR_H: f32 = 9.0;
const PAD: f32 = 4.0;
const MIN_W: f32 = 200.0;
/// Room left of a row's label for its mark.
const MARK_W: f32 = 26.0;
const HINT_GAP: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// On/off: a lime dot when on, a ring and dim text when off.
    Dot(bool),
    /// One choice of several: a lime dot on the chosen one.
    Radio(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry<C> {
    /// A section title.
    Header(String),
    /// Dim text that cannot be picked.
    Note(String),
    Separator,
    Item {
        cmd: C,
        label: String,
        mark: Mark,
        /// Dim text at the right edge.
        hint: Option<String>,
        enabled: bool,
    },
    /// Opens `entries` to the right of the row.
    Submenu {
        label: String,
        hint: Option<String>,
        entries: Vec<Entry<C>>,
    },
}

impl<C> Entry<C> {
    fn height(&self) -> f32 {
        match self {
            Self::Header(_) => HEADER_H,
            Self::Note(_) => ROW_H,
            Self::Separator => SEPARATOR_H,
            Self::Item { .. } | Self::Submenu { .. } => ROW_H,
        }
    }

    /// Hover and clicks land on it.
    fn active(&self) -> bool {
        match self {
            Self::Item { enabled, .. } => *enabled,
            Self::Submenu { .. } => true,
            Self::Header(_) | Self::Note(_) | Self::Separator => false,
        }
    }
}

/// What an event did to the bar.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<C> {
    /// Not the bar's: pass it on.
    Ignored,
    /// The bar used it.
    Consumed,
    /// A row was picked. The menu is closed.
    Picked(C),
}

/// One open dropdown or submenu.
struct Level<C> {
    rect: RectF,
    rows: Vec<RectF>,
    entries: Vec<Entry<C>>,
    hover: Option<usize>,
}

impl<C: Clone> Level<C> {
    fn new(entries: Vec<Entry<C>>, x: f32, y: f32, text: &TextSystem) -> Self {
        let mut w = MIN_W;
        for e in &entries {
            let (label, hint) = match e {
                Entry::Header(s) | Entry::Note(s) => (s.as_str(), None),
                Entry::Item { label, hint, .. } => (label.as_str(), hint.as_deref()),
                Entry::Submenu { label, hint, .. } => (label.as_str(), hint.as_deref()),
                Entry::Separator => continue,
            };
            // Submenus show a chevron where an item's hint would go.
            let hint = match e {
                Entry::Submenu { .. } => text.measure(TextStyle::UiSmall, "\u{25b8}") + HINT_GAP,
                _ => hint.map_or(0.0, |h| text.measure(TextStyle::UiSmall, h) + HINT_GAP),
            };
            let label = text.measure(TextStyle::Ui, label);
            w = w.max(MARK_W + label + hint + 12.0 + 2.0 * PAD);
        }
        let mut rows = Vec::with_capacity(entries.len());
        let mut ry = y + PAD;
        for e in &entries {
            let h = e.height();
            rows.push(RectF::new(x + PAD, ry, w - 2.0 * PAD, h));
            ry += h;
        }
        Self {
            rect: RectF::new(x, y, w, ry + PAD - y),
            rows,
            entries,
            hover: None,
        }
    }

    fn row_at(&self, p: PointF) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.contains(p))
            .filter(|&i| self.entries[i].active())
    }
}

struct Open<C> {
    title: usize,
    /// The dropdown, then the submenu if one is open.
    levels: Vec<Level<C>>,
}

pub struct MenuBar<C> {
    bar: RectF,
    titles: Vec<(String, RectF)>,
    hover_title: Option<usize>,
    open: Option<Open<C>>,
}

impl<C: Clone> MenuBar<C> {
    pub fn new(titles: &[&str]) -> Self {
        Self {
            bar: RectF::default(),
            titles: titles
                .iter()
                .map(|t| (t.to_string(), RectF::default()))
                .collect(),
            hover_title: None,
            open: None,
        }
    }

    pub fn close(&mut self) {
        self.open = None;
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// What decides the bar's look: the open menu and the hovered title.
    pub fn state(&self) -> (Option<usize>, Option<usize>) {
        (self.open.as_ref().map(|o| o.title), self.hover_title)
    }

    /// Lays out the titles from the bar's left edge; returns where they end.
    pub fn layout(&mut self, bar: RectF, text: &TextSystem) -> f32 {
        self.bar = bar;
        let mut x = bar.x + 6.0;
        for (title, r) in &mut self.titles {
            let w = text.measure(TextStyle::Ui, title) + 2.0 * TITLE_PAD;
            *r = RectF::new(x, bar.y + 3.0, w, bar.h - 6.0);
            x += w;
        }
        x
    }

    /// Rebuilds the open menu from fresh entries, keeping which submenu is
    /// open and what is hovered. `entries(i)` describes title `i`'s menu.
    pub fn refresh(&mut self, text: &TextSystem, entries: impl Fn(usize) -> Vec<Entry<C>>) {
        let Some(open) = &self.open else {
            return;
        };
        let title = open.title;
        let hovers: Vec<Option<usize>> = open.levels.iter().map(|l| l.hover).collect();
        self.open_title(title, text, &entries);
        if let (Some(&Some(sub)), Some(open)) = (hovers.first(), &mut self.open) {
            open.levels[0].hover = Some(sub);
            Self::open_submenu(open, sub, text);
            for (l, h) in open.levels.iter_mut().zip(&hovers) {
                l.hover = h.filter(|&i| i < l.entries.len());
            }
        }
    }

    fn open_title(
        &mut self,
        i: usize,
        text: &TextSystem,
        entries: &impl Fn(usize) -> Vec<Entry<C>>,
    ) {
        let r = self.titles[i].1;
        let level = Level::new(entries(i), r.x, self.bar.bottom() + 1.0, text);
        self.open = Some(Open {
            title: i,
            levels: vec![level],
        });
    }

    /// Shows row `row`'s submenu (if it has one) in place of any other.
    fn open_submenu(open: &mut Open<C>, row: usize, text: &TextSystem) {
        open.levels.truncate(1);
        let top = &open.levels[0];
        if let Some(Entry::Submenu { entries, .. }) = top.entries.get(row) {
            let r = top.rows[row];
            let level = Level::new(entries.clone(), top.rect.right() - 2.0, r.y - PAD, text);
            open.levels.push(level);
        }
    }

    pub fn on_event(
        &mut self,
        e: &Event,
        text: &TextSystem,
        entries: impl Fn(usize) -> Vec<Entry<C>>,
    ) -> Outcome<C> {
        let title_at = |p: PointF| self.titles.iter().position(|(_, r)| r.contains(p));
        match *e {
            Event::MouseDown {
                pos,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = title_at(pos) {
                    if self.open.as_ref().is_some_and(|o| o.title == i) {
                        self.open = None;
                    } else {
                        self.open_title(i, text, &entries);
                    }
                    return Outcome::Consumed;
                }
                let Some(open) = &mut self.open else {
                    return if self.bar.contains(pos) {
                        Outcome::Consumed
                    } else {
                        Outcome::Ignored
                    };
                };
                // Deepest level first: a submenu may overlap its parent.
                for depth in (0..open.levels.len()).rev() {
                    let level = &open.levels[depth];
                    if !level.rect.contains(pos) {
                        continue;
                    }
                    let Some(row) = level.row_at(pos) else {
                        return Outcome::Consumed;
                    };
                    match &level.entries[row] {
                        Entry::Item { cmd, .. } => {
                            let cmd = cmd.clone();
                            self.open = None;
                            return Outcome::Picked(cmd);
                        }
                        Entry::Submenu { .. } => {
                            open.levels[0].hover = Some(row);
                            Self::open_submenu(open, row, text);
                            return Outcome::Consumed;
                        }
                        _ => return Outcome::Consumed,
                    }
                }
                // Outside: close, and let the click through unless it was
                // on the bar.
                self.open = None;
                if self.bar.contains(pos) {
                    Outcome::Consumed
                } else {
                    Outcome::Ignored
                }
            }
            Event::MouseDown { pos, .. } if self.open.is_some() || self.bar.contains(pos) => {
                self.open = None;
                Outcome::Consumed
            }
            Event::MouseMove { pos, .. } => {
                let title = title_at(pos);
                self.hover_title = title;
                let Some(open) = &mut self.open else {
                    return if self.bar.contains(pos) {
                        Outcome::Consumed
                    } else {
                        Outcome::Ignored
                    };
                };
                // Sliding along the bar switches menus.
                if let Some(i) = title.filter(|&i| i != open.title) {
                    self.open_title(i, text, &entries);
                    return Outcome::Consumed;
                }
                // Deepest level first: a submenu may overlap its parent.
                let inside = (0..open.levels.len())
                    .rev()
                    .find(|&d| open.levels[d].rect.contains(pos));
                match inside {
                    Some(0) => {
                        match open.levels[0].row_at(pos) {
                            Some(r) if open.levels[0].hover != Some(r) => {
                                open.levels[0].hover = Some(r);
                                Self::open_submenu(open, r, text);
                            }
                            Some(_) => {}
                            // Off the rows: an open submenu stays open.
                            None if open.levels.len() == 1 => open.levels[0].hover = None,
                            None => {}
                        }
                        Outcome::Consumed
                    }
                    Some(depth) => {
                        open.levels[depth].hover = open.levels[depth].row_at(pos);
                        Outcome::Consumed
                    }
                    None => {
                        // Keep the submenu's parent row lit; clear the rest.
                        if let Some(sub) = open.levels.get_mut(1) {
                            sub.hover = None;
                        } else {
                            open.levels[0].hover = None;
                        }
                        if self.bar.contains(pos) {
                            Outcome::Consumed
                        } else {
                            Outcome::Ignored
                        }
                    }
                }
            }
            Event::MouseLeave => {
                self.hover_title = None;
                Outcome::Ignored
            }
            Event::MouseUp { pos, .. } if self.open.is_some() || self.bar.contains(pos) => {
                Outcome::Consumed
            }
            Event::Wheel { pos, .. } if self.contains(pos) => Outcome::Consumed,
            Event::KeyDown {
                key: Key::Escape, ..
            } if self.open.is_some() => {
                let open = self.open.as_mut().expect("checked");
                if open.levels.len() > 1 {
                    open.levels.truncate(1);
                } else {
                    self.open = None;
                }
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }

    /// The point is on the bar or an open menu.
    pub fn contains(&self, p: PointF) -> bool {
        self.bar.contains(p)
            || self
                .open
                .as_ref()
                .is_some_and(|o| o.levels.iter().any(|l| l.rect.contains(p)))
    }

    /// The point is on a title (for the hand cursor).
    pub fn over_title(&self, p: PointF) -> bool {
        self.titles.iter().any(|(_, r)| r.contains(p))
    }

    /// The titles; the title bar paints the background.
    pub fn paint_bar(&self, d: &mut DrawCtx, theme: &Theme) {
        let open = self.open.as_ref().map(|o| o.title);
        for (i, (title, r)) in self.titles.iter().enumerate() {
            let lit = open == Some(i);
            if lit || self.hover_title == Some(i) {
                d.fill_round_rect(*r, 4.0, theme.hover_bg);
            }
            let color = if lit { theme.text_bright } else { theme.text };
            d.text(
                TextStyle::Ui,
                title,
                RectF::new(r.x + TITLE_PAD, r.y, r.w - TITLE_PAD, r.h),
                color,
            );
        }
    }

    /// The open menus; painted last so they cover the panels.
    pub fn paint_menus(&self, d: &mut DrawCtx, theme: &Theme) {
        let Some(open) = &self.open else {
            return;
        };
        for (depth, level) in open.levels.iter().enumerate() {
            d.fill_round_rect(level.rect, 6.0, theme.panel_bg);
            d.stroke_rect(level.rect, theme.border, 1.0);
            // The row whose submenu is open stays lit.
            let parent = (depth == 0 && open.levels.len() > 1)
                .then_some(level.hover)
                .flatten();
            for (i, (e, &row)) in level.entries.iter().zip(&level.rows).enumerate() {
                paint_entry(
                    d,
                    theme,
                    e,
                    row,
                    level.hover == Some(i) || parent == Some(i),
                );
            }
        }
    }
}

fn paint_entry<C>(d: &mut DrawCtx, theme: &Theme, e: &Entry<C>, row: RectF, hover: bool) {
    let label_rect = RectF::new(row.x + MARK_W, row.y, row.w - MARK_W, row.h);
    match e {
        Entry::Header(s) => {
            d.text(
                TextStyle::UiSmall,
                s,
                RectF::new(row.x + 8.0, row.y + 2.0, row.w - 8.0, row.h - 2.0),
                theme.text_dim,
            );
        }
        Entry::Note(s) => d.text(TextStyle::Ui, s, label_rect, theme.text_dim),
        Entry::Separator => {
            let y = row.y + row.h / 2.0;
            d.fill_rect(RectF::new(row.x + 4.0, y, row.w - 8.0, 1.0), theme.border);
        }
        Entry::Item {
            label,
            mark,
            hint,
            enabled,
            ..
        } => {
            if hover && *enabled {
                d.fill_round_rect(row, 4.0, theme.hover_bg);
            }
            let c = PointF::new(row.x + 12.0, row.y + row.h / 2.0);
            match *mark {
                Mark::Dot(true) | Mark::Radio(true) => d.fill_round_rect(
                    RectF::new(c.x - 3.5, c.y - 3.5, 7.0, 7.0),
                    3.5,
                    theme.accent,
                ),
                Mark::Dot(false) => d.stroke_rect(
                    RectF::new(c.x - 3.0, c.y - 3.0, 6.0, 6.0),
                    theme.text_dim,
                    1.0,
                ),
                Mark::Radio(false) => {}
            }
            let on = *enabled && *mark != Mark::Dot(false);
            let color = if on { theme.text } else { theme.text_dim };
            d.text(TextStyle::Ui, label, label_rect, color);
            if let Some(h) = hint {
                paint_hint(d, theme, h, row);
            }
        }
        Entry::Submenu { label, hint, .. } => {
            if hover {
                d.fill_round_rect(row, 4.0, theme.hover_bg);
            }
            d.text(TextStyle::Ui, label, label_rect, theme.text);
            let chevron = "\u{25b8}";
            let w = d.text_system().measure(TextStyle::UiSmall, chevron);
            let right = RectF::new(row.x, row.y, row.w - w - 10.0, row.h);
            if let Some(h) = hint {
                paint_hint(d, theme, h, right);
            }
            d.text(
                TextStyle::UiSmall,
                chevron,
                RectF::new(row.right() - w - 8.0, row.y, w + 4.0, row.h),
                theme.text_dim,
            );
        }
    }
}

fn paint_hint(d: &mut DrawCtx, theme: &Theme, hint: &str, row: RectF) {
    let w = d.text_system().measure(TextStyle::UiSmall, hint);
    d.text(
        TextStyle::UiSmall,
        hint,
        RectF::new(row.right() - w - 8.0, row.y, w + 4.0, row.h),
        theme.text_dim,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    fn down(x: f32, y: f32) -> Event {
        Event::MouseDown {
            pos: PointF::new(x, y),
            button: MouseButton::Left,
            mods: Modifiers::default(),
            clicks: 1,
        }
    }

    fn hover(x: f32, y: f32) -> Event {
        Event::MouseMove {
            pos: PointF::new(x, y),
            mods: Modifiers::default(),
            left_down: false,
        }
    }

    fn menus(i: usize) -> Vec<Entry<u32>> {
        let item = |cmd, label: &str| Entry::Item {
            cmd,
            label: label.into(),
            mark: Mark::Radio(cmd == 10),
            hint: None,
            enabled: true,
        };
        match i {
            0 => vec![item(1, "One"), item(2, "Two")],
            _ => vec![
                Entry::Header("Call graph".into()),
                Entry::Submenu {
                    label: "Mode".into(),
                    hint: None,
                    entries: vec![item(10, "Abstracted"), item(11, "Verbose")],
                },
                Entry::Separator,
                Entry::Note("No settings yet".into()),
            ],
        }
    }

    fn centre(r: RectF) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn setup() -> (MenuBar<u32>, TextSystem) {
        let text = TextSystem::new().expect("DirectWrite");
        let mut bar = MenuBar::new(&["Panels", "Visualizers"]);
        bar.layout(RectF::new(0.0, 0.0, 800.0, 28.0), &text);
        (bar, text)
    }

    #[test]
    fn submenu_opens_on_hover_and_its_items_are_picked() {
        let (mut bar, text) = setup();
        let (x, y) = centre(bar.titles[1].1);
        assert_eq!(bar.on_event(&down(x, y), &text, menus), Outcome::Consumed);
        assert_eq!(bar.state().0, Some(1));
        // Header and note rows are inert.
        let rows = bar.open.as_ref().unwrap().levels[0].rows.clone();
        let (hx, hy) = centre(rows[0]);
        assert_eq!(bar.on_event(&down(hx, hy), &text, menus), Outcome::Consumed);
        let (nx, ny) = centre(rows[3]);
        assert_eq!(bar.on_event(&down(nx, ny), &text, menus), Outcome::Consumed);
        assert!(bar.is_open());

        let (mx, my) = centre(rows[1]);
        bar.on_event(&hover(mx, my), &text, menus);
        let open = bar.open.as_ref().unwrap();
        assert_eq!(open.levels.len(), 2, "Mode submenu open");
        let sub = &open.levels[1];
        assert!(sub.rect.x >= open.levels[0].rect.right() - 4.0);
        let (vx, vy) = centre(sub.rows[1]);
        assert_eq!(
            bar.on_event(&down(vx, vy), &text, menus),
            Outcome::Picked(11)
        );
        assert!(!bar.is_open());
    }

    #[test]
    fn escape_backs_out_and_outside_clicks_pass_through() {
        let (mut bar, text) = setup();
        let (x, y) = centre(bar.titles[1].1);
        bar.on_event(&down(x, y), &text, menus);
        let rows = bar.open.as_ref().unwrap().levels[0].rows.clone();
        let (mx, my) = centre(rows[1]);
        bar.on_event(&hover(mx, my), &text, menus);
        let esc = Event::KeyDown {
            key: Key::Escape,
            mods: Modifiers::default(),
        };
        assert_eq!(bar.on_event(&esc, &text, menus), Outcome::Consumed);
        assert_eq!(bar.open.as_ref().unwrap().levels.len(), 1);
        assert_eq!(bar.on_event(&esc, &text, menus), Outcome::Consumed);
        assert!(!bar.is_open());
        assert_eq!(bar.on_event(&esc, &text, menus), Outcome::Ignored);

        // Sliding along the bar switches menus; a click below closes it and
        // reaches the panel underneath.
        bar.on_event(&down(x, y), &text, menus);
        let (px, py) = centre(bar.titles[0].1);
        bar.on_event(&hover(px, py), &text, menus);
        assert_eq!(bar.state().0, Some(0));
        assert_eq!(
            bar.on_event(&down(700.0, 500.0), &text, menus),
            Outcome::Ignored
        );
        assert!(!bar.is_open());
        // A second click on an open title closes it.
        bar.on_event(&down(px, py), &text, menus);
        bar.on_event(&down(px, py), &text, menus);
        assert!(!bar.is_open());
    }
}
