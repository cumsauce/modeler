//! The MCP server: tool args, the tool router and the Bevy bridge.

mod args;
pub(crate) use args::*;
mod bridge;
pub(crate) use bridge::*;
mod tools;
pub(crate) use tools::*;
