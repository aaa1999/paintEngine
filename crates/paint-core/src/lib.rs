//! paint-core：平台无关的绘画引擎核心。
//!
//! 职责：瓦片化像素存储、图层栈、撤销历史、视口变换、输入抽象、
//! 笔画生成与引擎骨架。本 crate 不感知屏幕与平台——它只管理像素
//! 数据，通过 [`render::Renderer`] / [`render::Surface`] 两个接口
//! 与外界（软件/GPU 渲染器、平台壳层）协作。

pub mod color;
pub mod document;
pub mod engine;
pub mod geometry;
pub mod history;
pub mod input;
pub mod layer;
pub mod render;
pub mod stroke;
pub mod tile;
pub mod viewport;

pub use color::Color;
pub use document::Document;
pub use engine::{Dirty, Engine};
pub use geometry::Rect;
pub use history::{History, StrokeRecorder, UndoGroup};
pub use input::{PlatformEvent, PointerKind, PointerPhase, PointerSample};
pub use layer::{BlendMode, Layer, LayerId, LayerStack};
pub use render::{EngineConfig, Renderer, Surface};
pub use stroke::{Dab, DabMode, RoundBrush, StrokeGen, StrokeState};
pub use tile::{TileData, TileGrid, TileId, TileRef, TILE, TILE_BYTES};
pub use viewport::Viewport;
