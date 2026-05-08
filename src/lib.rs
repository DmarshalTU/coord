//! `coord` — local coordination primitives for parallel AI agents.
//!
//! Exposes the core primitives, the A2A router, the MCP bridge, and the
//! markdown vault. Embedders can build directly against these modules;
//! the `coord` binary itself just wraps them in a CLI.

pub mod a2a;
pub mod core;
pub mod mcp;
pub mod vault;
