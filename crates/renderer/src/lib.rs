//! Visualizer viewport for the code graph: merges loaded objects into a
//! drawable model, lays it out with a force simulation on a worker thread,
//! and paints it with culling and level of detail through the [`Canvas`]
//! trait.
//!
//! Portable and safe: no Win32 types and no `unsafe`. The UI crate
//! implements [`Canvas`] on its Direct2D device context; see `DESIGN.md`
//! for why Direct2D rather than Direct3D.
#![forbid(unsafe_code)]

mod camera;
mod canvas;
mod geom;
mod layout;
mod model;
mod spatial;
mod view;

pub use camera::Camera;
pub use canvas::{Canvas, Rgba, Style};
pub use geom::{Affine, Rect, Vec2};
pub use layout::{LayoutWorker, Simulation, Snapshot};
pub use model::{EdgeClass, Filter, GraphModel, ModelEdge, ModelNode, NodeClass};
pub use view::{Cursor, GraphView, Response, NODE_RADIUS};
