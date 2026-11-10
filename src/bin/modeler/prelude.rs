//! Shared imports for every module.
#![allow(unused_imports)]

pub(crate) use crate::preview::*;
pub(crate) use crate::server::*;
pub(crate) use base64::Engine as _;
pub(crate) use base64::engine::general_purpose::STANDARD as B64;
pub(crate) use bevy::app::AppExit;
pub(crate) use bevy::asset::RenderAssetUsages;
pub(crate) use bevy::camera::{RenderTarget, ScalingMode};
pub(crate) use bevy::input::ButtonState;
pub(crate) use bevy::input::keyboard::{Key, KeyboardInput};
pub(crate) use bevy::input::mouse::AccumulatedMouseScroll;
pub(crate) use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings};
pub(crate) use bevy::prelude::*;
pub(crate) use bevy::render::mesh::{Indices, PrimitiveTopology};
pub(crate) use bevy::render::render_resource::TextureFormat;
pub(crate) use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
pub(crate) use bevy::window::{PrimaryWindow, WindowPlugin};
pub(crate) use modeler::catalog::{self, CatalogEntry};
pub(crate) use modeler::commands::{BatchItem, PendingPin, Session};
pub(crate) use modeler::gltf;
pub(crate) use modeler::{Axis, BoolOp, Model, Op, Part, PrimitiveKind};
pub(crate) use rmcp::handler::server::wrapper::Parameters;
pub(crate) use rmcp::model::{CallToolResult, Content, ErrorData};
pub(crate) use rmcp::{ServiceExt, schemars, tool, tool_handler, tool_router};
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::atomic::{AtomicBool, Ordering};
pub(crate) use std::sync::{Arc, Mutex};
pub(crate) use std::thread;
pub(crate) use tokio::sync::{mpsc, oneshot};
