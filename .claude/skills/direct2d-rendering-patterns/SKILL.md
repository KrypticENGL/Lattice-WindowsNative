---
name: direct2d-rendering-patterns
description: Conventions for all Direct2D/DirectWrite/DXGI drawing in this project (IDE panels, editor text, and the graph viewport). Use when writing or changing any painting code, swap chain or render target setup, DPI handling, text rendering, device-loss recovery, or the Visualizer renderer.
---

# Direct2D rendering patterns

Pairs with the `win32-unsafe-boundaries` skill: all raw COM/Win32 calls live in `sys` modules; panels and the renderer use safe wrappers.

## 1. Pipeline

- Create one D3D11 device, then a D2D1 factory, D2D device, and one `ID2D1DeviceContext`. Present through a DXGI flip-model swap chain (`DXGI_SWAP_EFFECT_FLIP_DISCARD`) and draw into the back buffer as a D2D bitmap target.
- If Mica/acrylic must show through the client area, the swap chain needs premultiplied alpha and a DirectComposition visual. Add this only if the design needs it, and in one place (the `sys` layer), not per panel.
- One device context, one swap chain, one `BeginDraw`/`EndDraw` per frame. Panels never create their own targets.

## 2. Resource lifetime and device loss

Split resources into two structs:

- **Device-independent** (kept for the app's lifetime): D2D factory, DirectWrite factory, text formats, path geometries.
- **Device-dependent** (rebuilt on loss): swap chain, target bitmap, brushes, bitmaps.

Recovery: if `EndDraw` returns `D2DERR_RECREATE_TARGET`, or `Present` returns `DXGI_ERROR_DEVICE_REMOVED` / `DXGI_ERROR_DEVICE_RESET`, drop all device-dependent resources, recreate the device chain, and repaint. Never treat this as fatal. Any new cached GPU object must be registered in the device-dependent struct so it is rebuilt.

## 3. DPI and units

- Process is per-monitor v2 DPI aware. All layout and drawing use DIPs (1 DIP = 1/96 inch).
- Call `SetDpi(dpi, dpi)` on the device context so D2D scales for you. Convert mouse and window coordinates from pixels to DIPs with `scale = dpi / 96.0` at the input boundary, once.
- `WM_DPICHANGED`: apply the suggested RECT with `SetWindowPos`, store the new DPI, `SetDpi`, rebuild DPI-dependent text layouts, repaint.
- `WM_SIZE`: release the target bitmap, `ResizeBuffers`, recreate the bitmap, relayout. Skip when minimized (0x0).

## 4. Painting model

- Paint on demand: `WM_PAINT` plus `InvalidateRect`. No always-on render loop. The Visualizer may run a frame loop only while layout is animating, and must stop when it settles.
- No draw calls outside `BeginDraw`/`EndDraw`. Every `PushAxisAlignedClip` has a matching `PopAxisAlignedClip`; every `SetTransform` is reset before the next panel.
- Panels implement a safe trait, roughly `layout(&mut self, rect: RectF)`, `paint(&mut self, ctx: &mut DrawCtx)`, `on_event(&mut self, e: &Event) -> EventResult`. `DrawCtx` is a safe wrapper that exposes rects, colors, text, and clip scopes, not COM pointers.
- Colors come from a theme token struct. No hard-coded colors in panels.
- Reuse one solid-color brush and call `SetColor`; do not create brushes per draw call.
- Rendering happens only on the UI thread. Background work (layout, parsing, file watching) sends plain data back via `PostMessageW`.

## 5. Text (editor, tree, terminal)

- Cache `IDWriteTextFormat` per style. Cache `IDWriteTextLayout` per visible line, keyed by (line id, content version, wrap width, DPI). Never rebuild layouts for unchanged lines each frame.
- Monospace default: Cascadia Mono, falling back to Consolas. Compute cell width and line height once per font and DPI.
- Caret and selection use `HitTestPoint` and `HitTestTextPosition` on the layout, not hand-rolled glyph math.
- Use ClearType only when drawing on an opaque background; use grayscale antialiasing over transparency.
- The terminal panel draws a cell grid from the ConPTY screen state, not from per-line layouts.

## 6. Graph viewport (crates/renderer)

- Pan and zoom via a single world-to-screen matrix applied with `SetTransform`. Reset it before drawing UI chrome.
- Cull: skip nodes and edges whose bounds fall outside the visible world rect before issuing any draw call.
- Level of detail: below a zoom threshold, drop labels and arrowheads, then draw nodes as plain dots. Keep stroke widths constant on screen by dividing by the zoom factor.
- Batch: group edges by style, and for large graphs build one `ID2D1PathGeometry` per style bucket (rebuilt only when layout changes) instead of thousands of `DrawLine` calls. If 10k+ nodes is still too slow, evaluate `ID2D1SpriteBatch` or a D3D11 instanced path, and record the decision in the design note.
- Layout runs on a worker thread and posts position arrays; the renderer only reads the latest published snapshot.
- Hit testing uses a spatial index (grid or quadtree), not a linear scan.

## 7. Before finishing a task

1. `cargo check` and `cargo clippy -- -D warnings` pass.
2. Resize, DPI change (move between monitors), and minimize/restore do not crash or leave stale pixels.
3. Device loss is handled (simulate by forcing the recreate path in a debug command).
4. No resource creation inside a per-frame path unless cached; no `windows::` types in panel or renderer public APIs.
