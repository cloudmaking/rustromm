//! Embedded libretro emulation.
//!
//! RustRomM began as a launcher that shelled out to RetroArch. That worked, but
//! it made the user find, install and configure an emulator before they could
//! play anything — and it made save sync impossible, because we did not control
//! the emulator. This module is the replacement.
//!
//! Every claim about how cores behave here was measured against seven real
//! cores rather than read out of the header. See `docs/libretro-spike.md`.

pub mod audio;
pub mod content;
pub mod core;
pub mod cores;
pub mod emu;
pub mod saves;
pub mod sys;
pub mod video;
