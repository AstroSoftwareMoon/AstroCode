//! # astrodev-hardware
//!
//! Detecta CPU, RAM, GPU/VRAM y disco; estima cuánta memoria necesita cada modelo
//! (pesos GGUF + KV cache + margen) y recomienda los que caben, con velocidad estimada.

pub mod bandwidth;
pub mod catalog;
pub mod detect;
pub mod memory;
pub mod recommend;

pub use catalog::{Catalog, CatalogModel, QuantVariant};
pub use detect::{detect, Backend, CpuInfo, GpuInfo, GpuVendor, HardwareInfo};
pub use memory::MemoryEstimate;
pub use recommend::{recommend, Fit, Placement, RecommendOptions, Recommendation};

pub const GB: f64 = 1e9;
