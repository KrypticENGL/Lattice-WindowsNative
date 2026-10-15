# Graph renderer: design note

## What it does

The Visualizer tab draws the merged code graph of every loaded object file.
It runs a force-directed layout on a worker thread. You can pan, zoom, drag
nodes, hover, and select a node to light up its neighbours. Double-clicking
a node opens its source.

The work is split across two crates:

| Where | What |
|---|---|
| `crates/renderer` (this crate, `forbid(unsafe_code)`, no Windows dependency) | the graph model and object merging, the layout simulation and its thread, the camera, the spatial grid, input handling, culling, level of detail, and the label and info-card logic. It draws through the `Canvas` trait |
| `crates/ui/src/sys/gfx.rs` | `impl renderer::Canvas for DrawCtx`, which runs on the app's single Direct2D device context and batches the drawing through `ID2D1SpriteBatch` |
| `crates/ui/src/visualizer.rs` | the panel: toolbar, filters, status line, empty state, folder loading, jump to source, and the benchmark |

Keeping the renderer free of Win32 code means all of its logic is unit
tested headlessly (model merge, layout convergence, camera maths, hit
testing, culling and level of detail), and the `unsafe` code stays in
`ui/src/sys`.

## Direct2D or Direct3D?

**Decision: Direct2D. Its sprite batch draws a whole style bucket in one
call. A Direct3D 11 instanced pipeline was not needed at the target size.**

Arguments for Direct2D:

- The app already has one D3D11 device, one D2D device context and one
  flip-model swap chain. The graph draws into the same frame as the rest of
  the UI, with the same device-loss recovery, DPI handling and DirectWrite
  labels. A separate Direct3D pass would need its own shaders, its own
  vertex buffers and its own path through device loss, plus either a second
  swap chain or careful interleaving with D2D's batching.
- Text (labels, the info card) is DirectWrite either way. Mixing in raw D3D
  would mean flushing D2D, drawing the D3D geometry, and resuming D2D in
  every frame.
- `ID2D1SpriteBatch` (Windows 10 1607+; the app already requires 1903)
  draws thousands of textured, tinted, transformed quads in one call, the
  same per-instance work an instanced D3D pipeline would do:
  - Nodes are quads showing a 32×32 antialiased disc (or a 2×2 square at
    dot level of detail), tinted per style.
  - Edges are 1×4-texel quads, transparent-opaque-opaque-transparent across
    their width, rotated and stretched between the two endpoints. Linear
    filtering antialiases them.
  - One batch per frame is filled in draw order; each style bucket draws
    its own range, so ordering with ordinary D2D calls (rings, labels, the
    card) is preserved.
- Direct3D would win beyond roughly 100k visible primitives per frame, or
  if edges needed custom shading such as curves or gradients. Neither
  applies yet. If one does, the place to change is the three batched
  `Canvas` methods (`stroke_segments`, `fill_discs`, `fill_squares`).
  Nothing in the renderer would change.

What was measured first: one Direct2D call per primitive (`DrawLine` per
edge, `FillEllipse` or `FillRectangle` per node). On the 10k-node graph,
every frame spent about 11 ms issuing calls, which gave 82–94 fps. That is
smooth on this machine, but too close to the 60 fps floor for slower
hardware. Sprite batching cut the draw time to 3–4 ms. The per-call path is
kept as a fallback in case sprite batches are unavailable.

## Keeping it fast

- **Paint on demand.** The layout thread publishes snapshots at most every
  16 ms and wakes the UI thread (the coalesced `WAKE_MESSAGE`). Repaints
  happen only for new positions or input. Once the layout settles, nothing
  runs.
- **Off-thread layout.** It works like d3-force:
  - velocity Verlet with a decaying `alpha`, about 300 ticks from cold;
  - springs on edges, with strength 1/min(degree), so hubs do not collapse;
  - Barnes–Hut repulsion over a quadtree (θ = 0.9), computed on up to 8
    threads once there are 2,000 or more nodes;
  - a weak pull toward the origin, so disconnected parts stay on screen.

  Dragging pins the node and keeps alpha at 0.3; releasing lets it cool
  again. Node positions are kept by `NodeId`, so a reload or filter change
  only nudges the layout.
- **One transform.** Pan and zoom are a single uniform-scale world-to-screen
  matrix (`SetTransform`). Stroke widths are divided by the zoom so they
  stay constant on screen. Labels and the card are drawn in screen space
  after the transform is reset.
- **Culling.**
  - Nodes: a uniform-grid query of the visible world rect, then an exact
    containment test.
  - Edges: their bounding box is tested against the same rect.
  - Hit testing for hover, click and drag uses the same grid, never a
    linear scan.
- **Level of detail.**
  - Below 1.6 px on-screen radius: nodes are drawn as unantialiased
    squares.
  - At 80 % zoom and above: arrowheads appear, and so do labels, but only
    when at most 250 nodes are on screen.
  - At any zoom: the hovered node, the selection and up to 60 of its
    neighbours are always labelled.
  - Labels are placed greedily (hover, then selection, then neighbours,
    then the rest), and a label that would overlap one already placed is
    skipped.

## Measured frame rate

Run with `lattice --benchmark=10000` (synthetic graph) or
`lattice FOLDER --benchmark` (every object under `FOLDER`), or press
`Ctrl+Shift+B` in the Visualizer. The benchmark waits for the layout to
settle, then draws 300 frames:

- 100 frames panning in a circle with the whole graph on screen (the worst
  case: nothing is culled);
- 100 frames zooming in to about 20× that zoom;
- 100 frames zooming back out.

Frame interval is the time between presents. Draw time is BeginDraw to
EndDraw on the UI thread.

Machine: Intel Core i5-11260H, Intel UHD Graphics (the display adapter),
1920×1080 at 144 Hz, 125 % scaling. Release build. Measured 2026-10-15.

| Graph | Nodes / edges | fps (whole graph on screen) | fps (zooming) | Draw time median / p95 |
|---|---|---|---|---|
| Synthetic | 10,000 / 24,180 | 147 | 143 | 3.4 / 5.8 ms |
| Synthetic, per-call drawing (before sprite batching) | 10,000 / 24,180 | 86 | 95 | 10.9 / 13.8 ms |
| Lattice's own `ui` crate (250 incremental objects), user code | 2,323 / 2,725 | 148 | 144 | 1.3 / 2.7 ms |
| Same objects with Library, Generated and Locals & types on | 23,748 / 45,754 | — | — | 2.6 ms (status line, whole graph on screen) |

144 fps is the display's refresh rate: with sprite batching every case is
vsync-bound, with 3–5× headroom on the draw side. Layout time:
- the 10k synthetic graph settles in about 5 s;
- the 23.7k real graph settles in under 40 s.

The UI keeps rendering at full rate meanwhile.

## Limits

- Edges are straight. Calls and references are told apart by colour and
  width, and direction by an arrowhead when zoomed in; there is no edge
  bundling.
- Merging links an external to the first definition with the same linkage
  name. Object files do not say which definitions the linker would pick, so
  a `static` function that shares a name with a global one in another
  object could be linked wrongly.
- Layout quality is that of a plain force simulation. Very dense graphs
  form a hairball when zoomed out. The filters (Library, Generated, Locals
  & types) and selection highlighting are the tools for reading them.
