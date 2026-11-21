//! # astrodev-core
//!
//! Tipos compartidos, traits (`Provider`, `Tool`) y lógica central de AstroDev.
//! Este crate no hace E/S de red: los proveedores y herramientas concretos viven en
//! otros crates y se conectan a través de los traits definidos aquí.

pub mod agent;
pub mod checkpoint;
pub mod config;
pub mod context;
pub mod message;
pub mod permissions;
pub mod project;
pub mod prompt_tools;
pub mod provider;
pub mod session;
pub mod system_prompt;
pub mod tool;

pub use agent::{Agent, AgentEvent};
pub use config::Config;
pub use message::{Message, Role, ToolCall};
pub use project::Project;
pub use provider::{ChatRequest, Provider, ProviderError, StreamEvent, Usage};
pub use tool::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec};
