//! RustRomM — a cross-platform desktop client for [RomM](https://github.com/rommapp/romm).
//!
//! Exposed as a library so the integration tests in `tests/` can drive the same
//! code the binary runs. Nothing here is intended as a stable public API.

pub mod api;
pub mod app;
pub mod config;
pub mod input;
pub mod launch;
pub mod libretro;
pub mod logging;
pub mod models;
