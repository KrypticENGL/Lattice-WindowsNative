//! Data mode: the stopped program's frames and the objects they reach,
//! drawn by shape (see [`layout`]).

pub mod layout;
mod view;

pub use layout::{BoxKind, DataBox, DataEdge, DataLayout, EdgeKind, Link, Measure, Slot};
pub use view::{DataResponse, DataStyle, DataView};
