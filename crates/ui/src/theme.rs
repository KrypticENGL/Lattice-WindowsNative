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
    /// Visualizer canvas and graph colours.
    pub graph_bg: Color,
    pub graph_function: Color,
    pub graph_variable: Color,
    pub graph_external: Color,
    pub graph_library: Color,
    pub graph_generated: Color,
    pub graph_local: Color,
    pub graph_type: Color,
    pub graph_calls: Color,
    pub graph_refs: Color,
    pub graph_structure: Color,
    pub graph_hover: Color,
    pub graph_card_bg: Color,
    /// Alpha multiplier for nodes unrelated to the selection.
    pub graph_dim: f32,
    /// Data mode (live program data) boxes and pointers.
    pub data_frame_bg: Color,
    pub data_header_bg: Color,
    pub data_box_bg: Color,
    pub data_border: Color,
    pub data_grid: Color,
    pub data_pointer: Color,
    pub data_cross: Color,
    pub data_changed: Color,
    /// Breakpoint dots in the editor gutter.
    pub breakpoint: Color,
    /// The line the debugger is stopped on.
    pub exec_line_bg: Color,
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
            // Lime accent; selection, focus and button colours derive from it.
            accent: Color::hex(0xA3E635),
            hover_bg: Color::hex(0x2A2D2E),
            row_selected_bg: Color::hex(0x37373D),
            row_selected_focus_bg: Color::hex(0x2F3D17),
            tab_active_bg: Color::hex(0x1E1E1E),
            tab_inactive_bg: Color::hex(0x181818),
            selection_bg: Color::hex(0x3D5220),
            selection_inactive_bg: Color::hex(0x3A3D41),
            current_line_bg: Color::hex(0x282828),
            caret: Color::hex(0xA3E635),
            gutter_text: Color::hex(0x6E7681),
            gutter_text_active: Color::hex(0xCCCCCC),
            scrollbar_thumb: Color::hex(0x797979).with_alpha(0.4),
            button_bg: Color::hex(0xA3E635),
            // Dark text: white on lime is unreadable.
            button_text: Color::hex(0x1A1F12),
            dirty: Color::hex(0xE2C08D),
            error: Color::hex(0xF14C4C),
            // Graph: lime functions on a near-black, faintly green canvas;
            // the other kinds use hues that stay apart from the accent.
            graph_bg: Color::hex(0x141613),
            graph_function: Color::hex(0xA3E635),
            graph_variable: Color::hex(0x2DD4BF),
            graph_external: Color::hex(0xFBBF24),
            graph_library: Color::hex(0x7C8471),
            graph_generated: Color::hex(0x8B7FB0),
            graph_local: Color::hex(0x9AA5B1),
            graph_type: Color::hex(0xE58FB8),
            graph_calls: Color::hex(0x9DB872).with_alpha(0.55),
            graph_refs: Color::hex(0x7E8A96).with_alpha(0.45),
            graph_structure: Color::hex(0x5E646B).with_alpha(0.45),
            graph_hover: Color::hex(0xFFFFFF).with_alpha(0.85),
            graph_card_bg: Color::hex(0x1C1F1A).with_alpha(0.96),
            graph_dim: 0.16,
            data_frame_bg: Color::hex(0x1B1E19),
            data_header_bg: Color::hex(0x26301A),
            data_box_bg: Color::hex(0x1F231C),
            data_border: Color::hex(0x414B34),
            data_grid: Color::hex(0x323A29),
            data_pointer: Color::hex(0x9DB872),
            data_cross: Color::hex(0x8A96A3).with_alpha(0.7),
            data_changed: Color::hex(0xA3E635).with_alpha(0.22),
            breakpoint: Color::hex(0xE5484D),
            exec_line_bg: Color::hex(0xA3E635).with_alpha(0.13),
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
