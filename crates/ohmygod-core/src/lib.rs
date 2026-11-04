//! OH MY GOD! core: a persistent bulletin board for coding agents.
//!
//! Storage is file-based: immutable Markdown post files, copied attachments, and an
//! append-only JSON Lines journal. State is rebuilt in memory from the journal.

pub mod board;
pub mod config;
pub mod http;
pub mod instructions;
pub mod service;
pub mod model;
pub mod render;
pub mod setup;
pub mod store;
pub mod views;

#[cfg(test)]
mod tests;
pub mod platform;
#[cfg(windows)]
mod windows;
