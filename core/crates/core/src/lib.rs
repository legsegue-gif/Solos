//! The Solos core: everything that is not UI or an operating-system API.
//!
//! Clients drive it through [`engine::Engine`] and learn about changes from
//! its event stream; the types they see are in `solos-api`.

pub mod agent;
pub mod bridge;
pub mod context;
pub mod engine;
pub mod files;
pub mod mirrors;
pub mod prompt;
pub mod skills;
pub mod providers;
pub mod sandbox;
pub mod store;
pub mod title;
pub mod tools;

pub use engine::{Engine, EngineConfig, SecretResolver};
