//! Caching utilities for memory-efficient data storage.

mod model_intern;

pub use model_intern::{ModelKey, intern_model, resolve_model};
