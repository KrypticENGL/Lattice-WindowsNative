//! Direct2D on a DXGI flip-model swap chain, with device-loss recovery, and
//! the safe [`DrawCtx`] that panels draw through.
//!
//! Resources are split per the rendering conventions: the D2D factory lives
//! in [`Graphics`] for the app's lifetime; everything tied to the GPU device
//! is in [`DeviceResources`] and is dropped and rebuilt on device loss.

use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HMODULE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1DeviceContext3, ID2D1Factory1,
    ID2D1SolidColorBrush, ID2D1SpriteBatch, D2D1_ANTIALIAS_MODE_ALIASED,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_ROUNDED_RECT, D2D1_SPRITE_OPTIONS_NONE, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SCALING_NONE,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::text::{TextLayout, TextStyle, TextSystem};
use super::{win_err, NativeWindow};
use crate::geom::{PointF, RectF, SizeF};
use crate::theme::Color;
use crate::Error;

/// Outcome of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    Presented,
    /// The GPU device was lost; resources were dropped and the caller should
    /// repaint, which rebuilds them.
    DeviceLost,
}

/// GPU objects rebuilt after device loss. Every cached GPU object belongs here.
struct DeviceResources {
    _d3d: ID3D11Device,
    dc: ID2D1DeviceContext,
    swap_chain: IDXGISwapChain1,
    target: Option<ID2D1Bitmap1>,
    brush: ID2D1SolidColorBrush,
    /// `None` if sprite batches are unavailable (before Windows 10 1607);
    /// the graph then falls back to one call per primitive.
    sprites: Option<Sprites>,
}

/// Batched drawing for the graph renderer. One sprite batch per frame is
/// filled in draw order; each batched call appends its sprites and draws
/// just that range, so ordering with ordinary draw calls is preserved.
struct Sprites {
    dc3: ID2D1DeviceContext3,
    batch: ID2D1SpriteBatch,
    /// White antialiased disc (nodes).
    disc: ID2D1Bitmap1,
    /// White square (nodes at dot level of detail).
    square: ID2D1Bitmap1,
    /// One texel long, transparent-opaque-opaque-transparent across: a
    /// rotated, stretched copy is an antialiased line.
    line: ID2D1Bitmap1,
}

/// Per-frame arrays for building sprites, kept to avoid reallocating.
#[derive(Default)]
struct SpriteScratch {
    rects: Vec<D2D_RECT_F>,
    colors: Vec<D2D1_COLOR_F>,
    transforms: Vec<Matrix3x2>,
}

pub struct Graphics {
    window: NativeWindow,
    factory: ID2D1Factory1,
    device: Option<DeviceResources>,
    dpi: f32,
    size_px: (u32, u32),
    /// CPU time of the last frame from BeginDraw to EndDraw.
    last_draw: Duration,
    scratch: SpriteScratch,
}

impl Graphics {
    pub fn new(window: NativeWindow, dpi: u32) -> Result<Self, Error> {
        // SAFETY: creates a single-threaded factory used only on this thread.
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
                .map_err(|e| win_err("D2D1CreateFactory", e))?;
        Ok(Self {
            window,
            factory,
            device: None,
            dpi: dpi as f32,
            size_px: (0, 0),
            last_draw: Duration::ZERO,
            scratch: SpriteScratch::default(),
        })
    }

    /// Client size in DIPs.
    pub fn size(&self) -> SizeF {
        let scale = self.dpi / 96.0;
        SizeF {
            w: self.size_px.0 as f32 / scale,
            h: self.size_px.1 as f32 / scale,
        }
    }

    /// Resizes the swap chain to the new client size in pixels. A 0×0 size
    /// (minimised) is remembered but the buffers are left alone.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Error> {
        self.size_px = (width, height);
        if width == 0 || height == 0 {
            return Ok(());
        }
        let Some(dev) = self.device.as_mut() else {
            return Ok(());
        };
        // SAFETY: the target must be released before ResizeBuffers; the
        // swap chain and context are live device objects.
        unsafe { dev.dc.SetTarget(None) };
        dev.target = None;
        // SAFETY: no outstanding references to the back buffers remain.
        let resized = unsafe {
            dev.swap_chain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        };
        match resized {
            Ok(()) => Ok(()),
            Err(e) if is_device_lost(e.code()) => {
                self.device = None;
                Ok(())
            }
            Err(e) => Err(win_err("ResizeBuffers", e)),
        }
    }

    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi as f32;
        if let Some(dev) = self.device.as_mut() {
            // SAFETY: plain setters on a live device context. The target
            // bitmap carries the DPI too, so it is rebuilt next frame.
            unsafe {
                dev.dc.SetTarget(None);
                dev.dc.SetDpi(self.dpi, self.dpi);
            }
            dev.target = None;
        }
    }

    /// CPU time spent recording and flushing the last frame (BeginDraw to
    /// EndDraw), excluding the wait in Present.
    pub fn last_draw_time(&self) -> Duration {
        self.last_draw
    }

    /// Drops all device resources as if the GPU had been removed. Debug aid
    /// for exercising the recovery path.
    pub fn simulate_device_loss(&mut self) {
        self.device = None;
    }

    /// Draws one frame: clears to `background`, runs `paint`, presents.
    pub fn render(
        &mut self,
        text: &TextSystem,
        background: Color,
        paint: impl FnOnce(&mut DrawCtx),
    ) -> Result<Frame, Error> {
        if self.size_px.0 == 0 || self.size_px.1 == 0 {
            return Ok(Frame::Presented);
        }
        if self.device.is_none() {
            self.device = Some(self.create_device()?);
        }
        let dpi = self.dpi;
        let Some(dev) = self.device.as_mut() else {
            return Ok(Frame::DeviceLost);
        };
        if dev.target.is_none() {
            match create_target(dev, dpi) {
                Ok(target) => dev.target = Some(target),
                Err(e) if is_device_lost(e.code()) => {
                    self.device = None;
                    return Ok(Frame::DeviceLost);
                }
                Err(e) => return Err(win_err("CreateBitmapFromDxgiSurface", e)),
            }
        }

        let started = Instant::now();
        // SAFETY: Begin/EndDraw are paired below; the context has a target.
        unsafe {
            dev.dc.BeginDraw();
            dev.dc.SetTransform(&Matrix3x2::identity());
            dev.dc.Clear(Some(&color_f(background)));
        }
        if let Some(s) = &dev.sprites {
            // SAFETY: the previous frame's sprites were consumed by its
            // EndDraw; the batch is a live device resource.
            unsafe { s.batch.Clear() };
        }
        let mut ctx = DrawCtx {
            dc: &dev.dc,
            brush: &dev.brush,
            text,
            sprites: dev.sprites.as_ref(),
            scratch: &mut self.scratch,
            antialias: true,
        };
        paint(&mut ctx);
        // SAFETY: matches the BeginDraw above.
        let ended = unsafe { dev.dc.EndDraw(None, None) };
        self.last_draw = started.elapsed();
        if let Err(e) = ended {
            if e.code() == D2DERR_RECREATE_TARGET || is_device_lost(e.code()) {
                self.device = None;
                return Ok(Frame::DeviceLost);
            }
            return Err(win_err("EndDraw", e));
        }
        // SAFETY: presents the back buffer just drawn; vsync interval 1.
        let hr = unsafe { dev.swap_chain.Present(1, DXGI_PRESENT(0)) };
        if is_device_lost(hr) {
            self.device = None;
            return Ok(Frame::DeviceLost);
        }
        hr.ok().map_err(|e| win_err("Present", e))?;
        Ok(Frame::Presented)
    }

    fn create_device(&self) -> Result<DeviceResources, Error> {
        let d3d = create_d3d_device(D3D_DRIVER_TYPE_HARDWARE)
            .or_else(|_| create_d3d_device(D3D_DRIVER_TYPE_WARP))
            .map_err(|e| win_err("D3D11CreateDevice", e))?;
        let dxgi_device: IDXGIDevice = d3d.cast().map_err(|e| win_err("IDXGIDevice", e))?;
        // SAFETY: the DXGI device is live; the factory outlives this call.
        let d2d_device = unsafe { self.factory.CreateDevice(&dxgi_device) }
            .map_err(|e| win_err("ID2D1Factory1::CreateDevice", e))?;
        // SAFETY: plain creation call on a live D2D device.
        let dc = unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE) }
            .map_err(|e| win_err("CreateDeviceContext", e))?;

        // SAFETY: queries on live DXGI objects.
        let adapter = unsafe { dxgi_device.GetAdapter() }.map_err(|e| win_err("GetAdapter", e))?;
        // SAFETY: as above.
        let dxgi_factory: IDXGIFactory2 =
            unsafe { adapter.GetParent() }.map_err(|e| win_err("IDXGIFactory2", e))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: self.size_px.0,
            Height: self.size_px.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let hwnd = self.window.hwnd();
        // SAFETY: `hwnd` is the live window owned by this thread; `desc`
        // outlives the call.
        let swap_chain =
            unsafe { dxgi_factory.CreateSwapChainForHwnd(&d3d, hwnd, &desc, None, None) }
                .map_err(|e| win_err("CreateSwapChainForHwnd", e))?;
        // SAFETY: as above. Failure only means Alt+Enter is not suppressed.
        let _ = unsafe { dxgi_factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER) };

        // SAFETY: plain setters on the new context; the colour struct
        // outlives the brush creation call.
        let brush = unsafe {
            dc.SetDpi(self.dpi, self.dpi);
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            dc.CreateSolidColorBrush(&color_f(Color::hex(0)), None)
        }
        .map_err(|e| win_err("CreateSolidColorBrush", e))?;

        let sprites = create_sprites(&dc).ok();
        Ok(DeviceResources {
            _d3d: d3d,
            dc,
            swap_chain,
            target: None,
            brush,
            sprites,
        })
    }
}

fn create_sprites(dc: &ID2D1DeviceContext) -> windows::core::Result<Sprites> {
    let dc3: ID2D1DeviceContext3 = dc.cast()?;
    // SAFETY: plain creation call on a live context.
    let batch = unsafe { dc3.CreateSpriteBatch() }?;
    const DISC: u32 = 32;
    let c = DISC as f32 / 2.0;
    let disc: Vec<u8> = (0..DISC * DISC)
        .flat_map(|i| {
            let (x, y) = ((i % DISC) as f32 + 0.5, (i / DISC) as f32 + 0.5);
            let d = ((x - c).powi(2) + (y - c).powi(2)).sqrt();
            // Premultiplied white: every channel equals the coverage.
            let a = ((c - d + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
            [a; 4]
        })
        .collect();
    let line: Vec<u8> = [0u8, 255, 255, 0].iter().flat_map(|&a| [a; 4]).collect();
    Ok(Sprites {
        disc: white_bitmap(dc, DISC, DISC, &disc)?,
        square: white_bitmap(dc, 2, 2, &[255; 16])?,
        line: white_bitmap(dc, 1, 4, &line)?,
        dc3,
        batch,
    })
}

/// A small premultiplied BGRA bitmap from `pixels` (4 bytes per pixel).
fn white_bitmap(
    dc: &ID2D1DeviceContext,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> windows::core::Result<ID2D1Bitmap1> {
    debug_assert_eq!(pixels.len(), (width * height * 4) as usize);
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        ..Default::default()
    };
    // SAFETY: `pixels` holds `height` rows of `width * 4` bytes and outlives
    // the call, which copies them.
    unsafe {
        dc.CreateBitmap(
            D2D_SIZE_U { width, height },
            Some(pixels.as_ptr().cast()),
            width * 4,
            &props,
        )
    }
}

fn create_d3d_device(driver: D3D_DRIVER_TYPE) -> windows::core::Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: `device` is a valid out-pointer; no adapter or software module.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }?;
    device.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

fn create_target(dev: &DeviceResources, dpi: f32) -> windows::core::Result<ID2D1Bitmap1> {
    // SAFETY: buffer 0 of a live swap chain; no other references exist.
    let surface: IDXGISurface = unsafe { dev.swap_chain.GetBuffer(0) }?;
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: dpi,
        dpiY: dpi,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        ..Default::default()
    };
    // SAFETY: `surface` and `props` outlive the call.
    let bitmap = unsafe { dev.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props)) }?;
    // SAFETY: binds the new bitmap as the context's target.
    unsafe { dev.dc.SetTarget(&bitmap) };
    Ok(bitmap)
}

fn is_device_lost(hr: windows::core::HRESULT) -> bool {
    hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET
}

fn color_f(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn rect_f(r: RectF) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// Safe drawing surface for one frame. Exists only between BeginDraw and
/// EndDraw; exposes rects, colours, text and clip scopes, never COM types.
pub struct DrawCtx<'a> {
    dc: &'a ID2D1DeviceContext,
    brush: &'a ID2D1SolidColorBrush,
    text: &'a TextSystem,
    sprites: Option<&'a Sprites>,
    scratch: &'a mut SpriteScratch,
    /// Current antialias mode, restored after sprite draws.
    antialias: bool,
}

impl<'a> DrawCtx<'a> {
    pub fn text_system(&self) -> &'a TextSystem {
        self.text
    }

    fn set_color(&self, c: Color) {
        // SAFETY: the brush is live for the frame; the colour is a local.
        unsafe { self.brush.SetColor(&color_f(c)) };
    }

    pub fn fill_rect(&mut self, r: RectF, c: Color) {
        if r.is_empty() {
            return;
        }
        self.set_color(c);
        // SAFETY: drawing inside BeginDraw/EndDraw with a live brush.
        unsafe { self.dc.FillRectangle(&rect_f(r), self.brush) };
    }

    pub fn fill_round_rect(&mut self, r: RectF, radius: f32, c: Color) {
        if r.is_empty() {
            return;
        }
        self.set_color(c);
        let rr = D2D1_ROUNDED_RECT {
            rect: rect_f(r),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as in `fill_rect`.
        unsafe { self.dc.FillRoundedRectangle(&rr, self.brush) };
    }

    /// Outline drawn inside `r`.
    pub fn stroke_rect(&mut self, r: RectF, c: Color, width: f32) {
        self.set_color(c);
        let half = width / 2.0;
        let inner = RectF::new(r.x + half, r.y + half, r.w - width, r.h - width);
        // SAFETY: as in `fill_rect`.
        unsafe {
            self.dc
                .DrawRectangle(&rect_f(inner), self.brush, width, None)
        };
    }

    pub fn line(&mut self, a: PointF, b: PointF, c: Color, width: f32) {
        self.set_color(c);
        // SAFETY: as in `fill_rect`.
        unsafe {
            self.dc.DrawLine(
                Vector2 { X: a.x, Y: a.y },
                Vector2 { X: b.x, Y: b.y },
                self.brush,
                width,
                None,
            );
        }
    }

    /// One line of text, vertically centred in `r` and clipped to it.
    pub fn text(&mut self, style: TextStyle, s: &str, r: RectF, c: Color) {
        let lh = self.text.line_height(style);
        let y = r.y + ((r.h - lh) / 2.0).max(0.0);
        let layout = RectF::new(r.x, y, r.w, r.bottom() - y);
        self.draw_text(style, s, layout, c, D2D1_DRAW_TEXT_OPTIONS_CLIP);
    }

    /// Unclipped text with its top-left at `origin`.
    pub fn text_at(&mut self, style: TextStyle, s: &str, origin: PointF, c: Color) {
        let r = RectF::new(origin.x, origin.y, 1.0e6, self.text.line_height(style));
        self.draw_text(style, s, r, c, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }

    fn draw_text(
        &mut self,
        style: TextStyle,
        s: &str,
        r: RectF,
        c: Color,
        options: D2D1_DRAW_TEXT_OPTIONS,
    ) {
        if s.is_empty() || r.w <= 0.0 {
            return;
        }
        self.set_color(c);
        let wide: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: `wide`, the format and the rect outlive the call.
        unsafe {
            self.dc.DrawText(
                &wide,
                self.text.format(style),
                &rect_f(r),
                self.brush,
                options,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    pub fn draw_layout(&mut self, layout: &TextLayout, origin: PointF, c: Color) {
        self.set_color(c);
        // SAFETY: the layout and brush are live for the call.
        unsafe {
            self.dc.DrawTextLayout(
                Vector2 {
                    X: origin.x,
                    Y: origin.y,
                },
                layout.raw(),
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    /// Runs `f` with drawing clipped to `r`; the clip is always popped.
    pub fn clip(&mut self, r: RectF, f: impl FnOnce(&mut Self)) {
        // SAFETY: pushed here and popped below on the same context.
        unsafe {
            self.dc
                .PushAxisAlignedClip(&rect_f(r), D2D1_ANTIALIAS_MODE_ALIASED)
        };
        f(self);
        // SAFETY: matches the push above.
        unsafe { self.dc.PopAxisAlignedClip() };
    }
}

fn rgba_f(c: renderer::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r,
        g: c.g,
        b: c.b,
        a: c.a,
    }
}

fn vec2(p: renderer::Vec2) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}

fn rounded(r: renderer::Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.right(),
            bottom: r.bottom(),
        },
        radiusX: radius,
        radiusY: radius,
    }
}

/// The graph renderer draws through the same context as the panels. Each
/// batch sets the shared brush once, then issues its primitives.
impl renderer::Canvas for DrawCtx<'_> {
    fn set_transform(&mut self, t: renderer::Affine) {
        let m = Matrix3x2 {
            M11: t.scale,
            M12: 0.0,
            M21: 0.0,
            M22: t.scale,
            M31: t.offset.x,
            M32: t.offset.y,
        };
        // SAFETY: plain setter on the live context during the frame.
        unsafe { self.dc.SetTransform(&m) };
    }

    fn reset_transform(&mut self) {
        // SAFETY: as above.
        unsafe { self.dc.SetTransform(&Matrix3x2::identity()) };
    }

    fn set_antialias(&mut self, on: bool) {
        self.antialias = on;
        let mode = if on {
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE
        } else {
            D2D1_ANTIALIAS_MODE_ALIASED
        };
        // SAFETY: as above.
        unsafe { self.dc.SetAntialiasMode(mode) };
    }

    fn stroke_segments(
        &mut self,
        segments: &[[renderer::Vec2; 2]],
        color: renderer::Rgba,
        width: f32,
    ) {
        if let Some(sprites) = self.sprites {
            let sc = &mut *self.scratch;
            sc.rects.clear();
            sc.transforms.clear();
            for [a, b] in segments {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = (dx * dx + dy * dy).sqrt();
                if len <= 0.0 || !len.is_finite() {
                    continue;
                }
                let (cos, sin) = (dx / len, dy / len);
                // The line texture is opaque across its middle half, so the
                // quad is twice the stroke width.
                sc.rects.push(D2D_RECT_F {
                    left: 0.0,
                    top: -width,
                    right: len,
                    bottom: width,
                });
                sc.transforms.push(Matrix3x2 {
                    M11: cos,
                    M12: sin,
                    M21: -sin,
                    M22: cos,
                    M31: a.x,
                    M32: a.y,
                });
            }
            self.draw_sprites(sprites, &sprites.line, color, true);
            return;
        }
        // SAFETY: the brush is live for the frame; the colour is a local.
        unsafe { self.brush.SetColor(&rgba_f(color)) };
        for [a, b] in segments {
            // SAFETY: drawing inside BeginDraw/EndDraw with a live brush.
            unsafe {
                self.dc
                    .DrawLine(vec2(*a), vec2(*b), self.brush, width, None)
            };
        }
    }

    fn fill_discs(&mut self, centers: &[renderer::Vec2], radius: f32, color: renderer::Rgba) {
        if let Some(sprites) = self.sprites {
            self.centered_rects(centers, radius);
            self.draw_sprites(sprites, &sprites.disc, color, false);
            return;
        }
        // SAFETY: as in `stroke_segments`.
        unsafe { self.brush.SetColor(&rgba_f(color)) };
        for c in centers {
            let e = D2D1_ELLIPSE {
                point: vec2(*c),
                radiusX: radius,
                radiusY: radius,
            };
            // SAFETY: as in `stroke_segments`.
            unsafe { self.dc.FillEllipse(&e, self.brush) };
        }
    }

    fn fill_squares(&mut self, centers: &[renderer::Vec2], half: f32, color: renderer::Rgba) {
        if let Some(sprites) = self.sprites {
            self.centered_rects(centers, half);
            self.draw_sprites(sprites, &sprites.square, color, false);
            return;
        }
        // SAFETY: as in `stroke_segments`.
        unsafe { self.brush.SetColor(&rgba_f(color)) };
        for c in centers {
            let r = D2D_RECT_F {
                left: c.x - half,
                top: c.y - half,
                right: c.x + half,
                bottom: c.y + half,
            };
            // SAFETY: as in `stroke_segments`.
            unsafe { self.dc.FillRectangle(&r, self.brush) };
        }
    }

    fn stroke_circle(
        &mut self,
        center: renderer::Vec2,
        radius: f32,
        color: renderer::Rgba,
        width: f32,
    ) {
        let e = D2D1_ELLIPSE {
            point: vec2(center),
            radiusX: radius,
            radiusY: radius,
        };
        // SAFETY: as in `stroke_segments`.
        unsafe {
            self.brush.SetColor(&rgba_f(color));
            self.dc.DrawEllipse(&e, self.brush, width, None);
        }
    }

    fn fill_rect(&mut self, r: renderer::Rect, radius: f32, color: renderer::Rgba) {
        // SAFETY: as in `stroke_segments`.
        unsafe {
            self.brush.SetColor(&rgba_f(color));
            self.dc
                .FillRoundedRectangle(&rounded(r, radius), self.brush);
        }
    }

    fn stroke_rect(&mut self, r: renderer::Rect, radius: f32, color: renderer::Rgba, width: f32) {
        let half = width / 2.0;
        let inner = renderer::Rect::new(r.x + half, r.y + half, r.w - width, r.h - width);
        // SAFETY: as in `stroke_segments`.
        unsafe {
            self.brush.SetColor(&rgba_f(color));
            self.dc
                .DrawRoundedRectangle(&rounded(inner, radius), self.brush, width, None);
        }
    }

    fn text(&mut self, s: &str, origin: renderer::Vec2, color: renderer::Rgba, bold: bool) {
        let style = if bold {
            TextStyle::UiBold
        } else {
            TextStyle::UiSmall
        };
        let c = Color {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a,
        };
        self.text_at(style, s, PointF::new(origin.x, origin.y), c);
    }

    fn measure_text(&self, s: &str, bold: bool) -> f32 {
        let style = if bold {
            TextStyle::UiBold
        } else {
            TextStyle::UiSmall
        };
        self.text.measure(style, s)
    }

    fn line_height(&self) -> f32 {
        self.text.line_height(TextStyle::UiSmall)
    }
}

impl DrawCtx<'_> {
    /// Fills the scratch rects with squares of half-size `half` around
    /// `centers` (no per-sprite transforms).
    fn centered_rects(&mut self, centers: &[renderer::Vec2], half: f32) {
        let sc = &mut *self.scratch;
        sc.transforms.clear();
        sc.rects.clear();
        sc.rects.extend(centers.iter().map(|c| D2D_RECT_F {
            left: c.x - half,
            top: c.y - half,
            right: c.x + half,
            bottom: c.y + half,
        }));
    }

    /// Appends the scratch rects (and transforms, if `transformed`) as
    /// sprites of `bitmap` tinted `color`, and draws that range now.
    fn draw_sprites(
        &mut self,
        sprites: &Sprites,
        bitmap: &ID2D1Bitmap1,
        color: renderer::Rgba,
        transformed: bool,
    ) {
        let sc = &mut *self.scratch;
        let count = sc.rects.len();
        if count == 0 {
            return;
        }
        sc.colors.clear();
        sc.colors.resize(count, rgba_f(color));
        // SAFETY: plain query on the live batch.
        let start = unsafe { sprites.batch.GetSpriteCount() };
        let transforms = transformed.then_some(sc.transforms.as_ptr());
        let transform_stride = if transformed {
            size_of::<Matrix3x2>() as u32
        } else {
            0
        };
        // SAFETY: each array holds `count` elements laid out with the given
        // strides and outlives the call, which copies them.
        let added = unsafe {
            sprites.batch.AddSprites(
                count as u32,
                sc.rects.as_ptr(),
                None,
                Some(sc.colors.as_ptr()),
                transforms,
                size_of::<D2D_RECT_F>() as u32,
                0,
                size_of::<D2D1_COLOR_F>() as u32,
                transform_stride,
            )
        };
        if added.is_err() {
            return;
        }
        // SAFETY: sprite batches must be drawn aliased; the previous mode is
        // restored right after. The batch, range and bitmap are live.
        unsafe {
            self.dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            sprites.dc3.DrawSpriteBatch(
                &sprites.batch,
                start,
                count as u32,
                bitmap,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                D2D1_SPRITE_OPTIONS_NONE,
            );
            if self.antialias {
                self.dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
        }
    }
}
