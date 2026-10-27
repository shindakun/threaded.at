//! Reads `at.threaded.*` records from a hydrant indexer and applies them.

pub mod apply;
pub mod client;
pub mod consumer;

pub use consumer::Indexer;
