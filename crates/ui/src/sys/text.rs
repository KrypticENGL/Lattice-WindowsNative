//! DirectWrite: cached text formats (device-independent) and line layouts
//! with hit testing. All units are DIPs.

use windows::core::{BOOL, HSTRING};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};

use super::win_err;
use crate::geom::SizeF;
use crate::Error;

/// Cached text styles. Each maps to one `IDWriteTextFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextStyle {
    /// Segoe UI, chrome and lists.
    Ui,
    /// Smaller Segoe UI for headers and labels.
    UiSmall,
    /// Semi-bold Segoe UI.
    UiBold,
    /// Monospace for the editor and terminal.
    Mono,
}

const STYLE_COUNT: usize = 4;
const NO_WRAP_WIDTH: f32 = 1.0e6;

impl TextStyle {
    fn index(self) -> usize {
        match self {
            TextStyle::Ui => 0,
            TextStyle::UiSmall => 1,
            TextStyle::UiBold => 2,
            TextStyle::Mono => 3,
        }
    }
}

/// Device-independent text resources, kept for the app's lifetime.
pub struct TextSystem {
    factory: IDWriteFactory,
    formats: [IDWriteTextFormat; STYLE_COUNT],
    line_heights: [f32; STYLE_COUNT],
    mono_cell: SizeF,
    mono_family: &'static str,
}

impl TextSystem {
    pub fn new() -> Result<Self, Error> {
        // SAFETY: creates the shared, process-wide DirectWrite factory.
        let factory: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .map_err(|e| win_err("DWriteCreateFactory", e))?;

        let mono_family = if font_exists(&factory, "Cascadia Mono") {
            "Cascadia Mono"
        } else {
            "Consolas"
        };
        let formats = [
            create_format(&factory, "Segoe UI", 13.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            create_format(&factory, "Segoe UI", 11.0, DWRITE_FONT_WEIGHT_NORMAL)?,
            create_format(&factory, "Segoe UI", 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
            create_format(&factory, mono_family, 13.0, DWRITE_FONT_WEIGHT_NORMAL)?,
        ];

        let mut system = Self {
            factory,
            formats,
            line_heights: [0.0; STYLE_COUNT],
            mono_cell: SizeF::default(),
            mono_family,
        };
        for style in [
            TextStyle::Ui,
            TextStyle::UiSmall,
            TextStyle::UiBold,
            TextStyle::Mono,
        ] {
            system.line_heights[style.index()] = system.layout(style, "Mg")?.height().ceil();
        }
        let sample = system.layout(TextStyle::Mono, "MMMMMMMMMM")?;
        system.mono_cell = SizeF {
            w: sample.width() / 10.0,
            h: system.line_height(TextStyle::Mono),
        };
        let mono = &system.formats[TextStyle::Mono.index()];
        // SAFETY: plain setter on a live format object.
        unsafe { mono.SetIncrementalTabStop(system.mono_cell.w * 4.0) }
            .map_err(|e| win_err("SetIncrementalTabStop", e))?;
        Ok(system)
    }

    pub fn mono_family(&self) -> &'static str {
        self.mono_family
    }

    /// Height of one line of `style`, rounded up to whole DIPs.
    pub fn line_height(&self, style: TextStyle) -> f32 {
        self.line_heights[style.index()]
    }

    /// Monospace cell size (advance width, line height).
    pub fn cell_size(&self) -> SizeF {
        self.mono_cell
    }

    /// Width of `text` in `style`, including trailing whitespace.
    pub fn measure(&self, style: TextStyle, text: &str) -> f32 {
        self.layout(style, text).map_or(0.0, |l| l.width())
    }

    /// Lays out a single unwrapped line.
    pub fn layout(&self, style: TextStyle, text: &str) -> Result<TextLayout, Error> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `wide` and the format are alive for the call; DirectWrite
        // copies the text.
        let inner = unsafe {
            self.factory.CreateTextLayout(
                &wide,
                self.format(style),
                NO_WRAP_WIDTH,
                self.line_heights[style.index()].max(1.0),
            )
        }
        .map_err(|e| win_err("CreateTextLayout", e))?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        // SAFETY: `metrics` is a valid out-pointer.
        unsafe { inner.GetMetrics(&mut metrics) }.map_err(|e| win_err("GetMetrics", e))?;
        Ok(TextLayout {
            inner,
            width: metrics.widthIncludingTrailingWhitespace,
            height: metrics.height,
            utf16_len: u32::try_from(wide.len()).unwrap_or(u32::MAX),
        })
    }

    pub(crate) fn format(&self, style: TextStyle) -> &IDWriteTextFormat {
        &self.formats[style.index()]
    }
}

/// One laid-out line. Positions are UTF-16 code-unit indices, matching
/// DirectWrite.
pub struct TextLayout {
    inner: IDWriteTextLayout,
    width: f32,
    height: f32,
    utf16_len: u32,
}

impl TextLayout {
    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    /// Nearest caret position (UTF-16 index) to horizontal offset `x`.
    pub fn hit_test_x(&self, x: f32) -> usize {
        let mut trailing = BOOL::default();
        let mut inside = BOOL::default();
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: all out-pointers are valid locals.
        let ok = unsafe {
            self.inner
                .HitTestPoint(x, self.height / 2.0, &mut trailing, &mut inside, &mut m)
        };
        if ok.is_err() {
            return 0;
        }
        let pos = m.textPosition + if trailing.as_bool() { m.length } else { 0 };
        pos.min(self.utf16_len) as usize
    }

    /// Horizontal offset of the caret before UTF-16 index `pos`.
    pub fn x_of(&self, pos: usize) -> f32 {
        if self.utf16_len == 0 || pos == 0 {
            return 0.0;
        }
        let pos = u32::try_from(pos).unwrap_or(u32::MAX);
        let (pos, trailing) = if pos >= self.utf16_len {
            (self.utf16_len - 1, true)
        } else {
            (pos, false)
        };
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: all out-pointers are valid locals; `pos` is in range.
        match unsafe {
            self.inner
                .HitTestTextPosition(pos, trailing, &mut x, &mut y, &mut m)
        } {
            Ok(()) => x,
            Err(_) => 0.0,
        }
    }

    pub(crate) fn raw(&self) -> &IDWriteTextLayout {
        &self.inner
    }
}

fn create_format(
    factory: &IDWriteFactory,
    family: &str,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat, Error> {
    let family = HSTRING::from(family);
    // SAFETY: `family` and the locale literal outlive the call.
    let format = unsafe {
        factory.CreateTextFormat(
            &family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            windows::core::w!("en-us"),
        )
    }
    .map_err(|e| win_err("CreateTextFormat", e))?;
    // SAFETY: plain setter on the format just created.
    unsafe { format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP) }
        .map_err(|e| win_err("SetWordWrapping", e))?;
    Ok(format)
}

fn font_exists(factory: &IDWriteFactory, family: &str) -> bool {
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: `collection` is a valid out-pointer.
    if unsafe { factory.GetSystemFontCollection(&mut collection, false) }.is_err() {
        return false;
    }
    let Some(collection) = collection else {
        return false;
    };
    let name = HSTRING::from(family);
    let mut index = 0u32;
    let mut exists = BOOL::default();
    // SAFETY: `name` outlives the call; out-pointers are valid locals.
    let ok = unsafe { collection.FindFamilyName(&name, &mut index, &mut exists) };
    ok.is_ok() && exists.as_bool()
}
