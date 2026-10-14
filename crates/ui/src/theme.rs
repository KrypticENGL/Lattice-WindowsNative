//! Colour tokens. Panels take every colour from [`Theme`]; none are hard-coded.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// From `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub chrome_bg: Color,
    pub panel_bg: Color,
    pub editor_bg: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_bright: Color,
    pub accent: Color,
    pub hover_bg: Color,
    pub row_selected_bg: Color,
    pub row_selected_focus_bg: Color,
    pub tab_active_bg: Color,
    pub tab_inactive_bg: Color,
    pub selection_bg: Color,
    pub selection_inactive_bg: Color,
    pub current_line_bg: Color,
    pub caret: Color,
    pub gutter_text: Color,
    pub gutter_text_active: Color,
    pub scrollbar_thumb: Color,
    pub button_bg: Color,
    pub button_text: Color,
    pub dirty: Color,
    pub error: Color,
    pub term_fg: Color,
    pub term_bg: Color,
    /// ANSI colours 0–15 (normal then bright).
    pub term_palette: [Color; 16],
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            chrome_bg: Color::hex(0x1F1F1F),
            panel_bg: Color::hex(0x181818),
            editor_bg: Color::hex(0x1E1E1E),
            border: Color::hex(0x2B2B2B),
            text: Color::hex(0xCCCCCC),
            text_dim: Color::hex(0x8B8B8B),
            text_bright: Color::hex(0xFFFFFF),
            accent: Color::hex(0x0078D4),
            hover_bg: Color::hex(0x2A2D2E),
            row_selected_bg: Color::hex(0x37373D),
            row_selected_focus_bg: Color::hex(0x04395E),
            tab_active_bg: Color::hex(0x1E1E1E),
            tab_inactive_bg: Color::hex(0x181818),
            selection_bg: Color::hex(0x264F78),
            selection_inactive_bg: Color::hex(0x3A3D41),
            current_line_bg: Color::hex(0x282828),
            caret: Color::hex(0xAEAFAD),
            gutter_text: Color::hex(0x6E7681),
            gutter_text_active: Color::hex(0xCCCCCC),
            scrollbar_thumb: Color::hex(0x797979).with_alpha(0.4),
            button_bg: Color::hex(0x0078D4),
            button_text: Color::hex(0xFFFFFF),
            dirty: Color::hex(0xE2C08D),
            error: Color::hex(0xF14C4C),
            term_fg: Color::hex(0xCCCCCC),
            term_bg: Color::hex(0x181818),
            term_palette: [
                Color::hex(0x0C0C0C),
                Color::hex(0xC50F1F),
                Color::hex(0x13A10E),
                Color::hex(0xC19C00),
                Color::hex(0x0037DA),
                Color::hex(0x881798),
                Color::hex(0x3A96DD),
                Color::hex(0xCCCCCC),
                Color::hex(0x767676),
                Color::hex(0xE74856),
                Color::hex(0x16C60C),
                Color::hex(0xF9F1A5),
                Color::hex(0x3B78FF),
                Color::hex(0xB4009E),
                Color::hex(0x61D6D6),
                Color::hex(0xF2F2F2),
            ],
        }
    }
}
