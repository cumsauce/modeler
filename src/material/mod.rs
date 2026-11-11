//! Procedural PBR materials: node graphs of coordinates, 2D/3D SDFs, noise,
//! maths and colour, evaluated in parallel over a sample set (a tileable
//! grid, or the texels of a part's atlas) and baked into PBR channels.

pub mod atlas;
pub mod bake;
pub mod doc;
pub mod eval;
pub mod node;
pub mod noise;
pub mod pbr;
pub mod sdf;

pub use bake::{Channels, bake};
pub use doc::{Library, MatDoc, MatExpect};
pub use eval::{Samples, evaluate};
pub use node::{Buf, In, NODE_TYPES};
pub use pbr::{MatGraph, PbrRef};
