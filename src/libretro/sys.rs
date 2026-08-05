//! Raw libretro ABI.
//!
//! Hand-written rather than taken from a crate. The frontend side of libretro is
//! about thirty symbols, and every Rust binding crate surveyed was either
//! core-side (for *writing* a core, which is the opposite problem) or
//! unmaintained. For a project this size an abandoned dependency in the
//! load-bearing layer is a worse risk than the declarations below, which were
//! validated against seven independently-built cores — see
//! `docs/libretro-spike.md`.
//!
//! Everything here is `#[repr(C)]` and mirrors `libretro.h` exactly. Nothing in
//! this module is safe; `super::core` is the safe boundary.

#![allow(dead_code)]

use std::ffi::{c_char, c_uint, c_void};

/// The ABI revision this frontend speaks. `retro_api_version` returning
/// anything else means the core was built against a different libretro and
/// cannot be trusted — all seven cores measured returned 1.
pub const RETRO_API_VERSION: c_uint = 1;

// ─── Pixel formats ───────────────────────────────────────────────────────────

/// A core that never calls `SET_PIXEL_FORMAT` gets this one. Not observed in
/// the spike, but the specification says it is the default, so it must be
/// handled rather than treated as impossible.
pub const RETRO_PIXEL_FORMAT_0RGB1555: c_uint = 0;
pub const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;
pub const RETRO_PIXEL_FORMAT_RGB565: c_uint = 2;

// ─── Memory regions ──────────────────────────────────────────────────────────

/// Battery-backed cartridge save. Cores never write this out themselves; the
/// frontend must persist it, and must not assume a clean shutdown will happen.
pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_RTC: c_uint = 1;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
pub const RETRO_MEMORY_VIDEO_RAM: c_uint = 3;

// ─── Devices ─────────────────────────────────────────────────────────────────

pub const RETRO_DEVICE_NONE: c_uint = 0;
pub const RETRO_DEVICE_JOYPAD: c_uint = 1;
pub const RETRO_DEVICE_MOUSE: c_uint = 2;
pub const RETRO_DEVICE_KEYBOARD: c_uint = 3;
pub const RETRO_DEVICE_LIGHTGUN: c_uint = 4;
pub const RETRO_DEVICE_ANALOG: c_uint = 5;
pub const RETRO_DEVICE_POINTER: c_uint = 6;

// Joypad button ids. The ordering is libretro's, and it is a SNES layout:
// B is the bottom face button and A is the right one. That is transposed
// relative to an Xbox pad, where A is on the bottom — mapping A-to-A is the
// classic mistake and puts every button in the wrong place.
pub const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
pub const RETRO_DEVICE_ID_JOYPAD_Y: c_uint = 1;
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;
pub const RETRO_DEVICE_ID_JOYPAD_X: c_uint = 9;
pub const RETRO_DEVICE_ID_JOYPAD_L: c_uint = 10;
pub const RETRO_DEVICE_ID_JOYPAD_R: c_uint = 11;
pub const RETRO_DEVICE_ID_JOYPAD_L2: c_uint = 12;
pub const RETRO_DEVICE_ID_JOYPAD_R2: c_uint = 13;
pub const RETRO_DEVICE_ID_JOYPAD_L3: c_uint = 14;
pub const RETRO_DEVICE_ID_JOYPAD_R3: c_uint = 15;

/// Highest joypad id plus one. Used to size the button state array.
pub const JOYPAD_BUTTON_COUNT: usize = 16;

// ─── Environment commands ────────────────────────────────────────────────────
//
// Only the ones the frontend actually answers are named. The spike left
// thirteen commands unhandled — returning false — and every passing core
// carried on regardless, so an exhaustive list is not required to boot a game.

pub const RETRO_ENVIRONMENT_GET_OVERSCAN: c_uint = 2;
pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
pub const RETRO_ENVIRONMENT_SET_MESSAGE: c_uint = 6;
pub const RETRO_ENVIRONMENT_SHUTDOWN: c_uint = 7;
pub const RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL: c_uint = 8;
pub const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
pub const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
pub const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
pub const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
pub const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
pub const RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME: c_uint = 18;
pub const RETRO_ENVIRONMENT_GET_LIBRETRO_PATH: c_uint = 19;
pub const RETRO_ENVIRONMENT_GET_INPUT_DEVICE_CAPABILITIES: c_uint = 24;
pub const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
pub const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
pub const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: c_uint = 32;
pub const RETRO_ENVIRONMENT_SET_GEOMETRY: c_uint = 37;
pub const RETRO_ENVIRONMENT_GET_LANGUAGE: c_uint = 39;
pub const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2: c_uint = 67;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;

/// Commands above this bit are marked experimental in `libretro.h`; the command
/// number itself is the low bits. Matching without masking silently misses
/// every experimental command.
pub const RETRO_ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;

/// Strip the experimental and private bits, leaving the command number.
pub fn env_command(cmd: c_uint) -> c_uint {
    cmd & 0xffff
}

// ─── Log levels ──────────────────────────────────────────────────────────────

pub const RETRO_LOG_DEBUG: c_uint = 0;
pub const RETRO_LOG_INFO: c_uint = 1;
pub const RETRO_LOG_WARN: c_uint = 2;
pub const RETRO_LOG_ERROR: c_uint = 3;

// ─── Structs ─────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug)]
pub struct SystemInfo {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    /// Pipe-separated, no dots: `"gb|gbc|dmg"`.
    pub valid_extensions: *const c_char,
    /// When true the core wants a path on disk and will ignore `data`. Genesis
    /// Plus GX sets this; most cores do not. Both paths must work.
    pub need_fullpath: bool,
    pub block_extract: bool,
}

impl Default for SystemInfo {
    fn default() -> Self {
        Self {
            library_name: std::ptr::null(),
            library_version: std::ptr::null(),
            valid_extensions: std::ptr::null(),
            need_fullpath: false,
            block_extract: false,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GameGeometry {
    /// Nominal size. Treat as a hint only — Stella reports 320×228 here and
    /// then delivers 160×228 frames. Size from the video callback instead.
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    /// Display aspect. Frequently not `width / height`: Genesis Plus GX reports
    /// 1.524 for a 256×224 frame because Mega Drive pixels are not square.
    /// Zero means "assume square pixels".
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTiming {
    /// Rarely a round number: 59.7275 on Game Boy, 60.0988 on SNES.
    pub fps: f64,
    /// Also rarely round: 32040 Hz on Snes9x, 131072 Hz on mGBA.
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemAvInfo {
    pub geometry: GameGeometry,
    pub timing: SystemTiming,
}

#[repr(C)]
pub struct GameInfo {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

#[repr(C)]
pub struct Variable {
    pub key: *const c_char,
    pub value: *const c_char,
}

// ─── Callback types ──────────────────────────────────────────────────────────

pub type EnvironmentFn = unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool;
/// `data` is null when the core is asking to repeat the previous frame, which
/// is only legal because the frontend answered `GET_CAN_DUPE` with true.
/// `pitch` is BYTES per row and is routinely wider than `width * bpp`.
pub type VideoRefreshFn =
    unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize);
pub type AudioSampleFn = unsafe extern "C" fn(left: i16, right: i16);
/// Signed 16-bit stereo, interleaved. `frames` counts stereo pairs, so the
/// buffer holds `frames * 2` samples.
pub type AudioSampleBatchFn = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;
pub type InputPollFn = unsafe extern "C" fn();
/// Called many times per frame after a single `InputPollFn`. Must be cheap and
/// must return a consistent snapshot for the whole frame.
pub type InputStateFn =
    unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experimental_bit_is_masked_off() {
        // A command flagged experimental must still match its base number,
        // otherwise every experimental command silently falls through.
        assert_eq!(env_command(RETRO_ENVIRONMENT_GET_CAN_DUPE), 3);
        assert_eq!(
            env_command(RETRO_ENVIRONMENT_EXPERIMENTAL | RETRO_ENVIRONMENT_GET_CAN_DUPE),
            3
        );
    }

    #[test]
    fn joypad_ids_are_dense_and_within_the_button_count() {
        // The state array is indexed by these ids, so a gap or an id past the
        // end would be an out-of-bounds write on the hot path.
        let ids = [
            RETRO_DEVICE_ID_JOYPAD_B,
            RETRO_DEVICE_ID_JOYPAD_Y,
            RETRO_DEVICE_ID_JOYPAD_SELECT,
            RETRO_DEVICE_ID_JOYPAD_START,
            RETRO_DEVICE_ID_JOYPAD_UP,
            RETRO_DEVICE_ID_JOYPAD_DOWN,
            RETRO_DEVICE_ID_JOYPAD_LEFT,
            RETRO_DEVICE_ID_JOYPAD_RIGHT,
            RETRO_DEVICE_ID_JOYPAD_A,
            RETRO_DEVICE_ID_JOYPAD_X,
            RETRO_DEVICE_ID_JOYPAD_L,
            RETRO_DEVICE_ID_JOYPAD_R,
            RETRO_DEVICE_ID_JOYPAD_L2,
            RETRO_DEVICE_ID_JOYPAD_R2,
            RETRO_DEVICE_ID_JOYPAD_L3,
            RETRO_DEVICE_ID_JOYPAD_R3,
        ];
        assert_eq!(ids.len(), JOYPAD_BUTTON_COUNT);
        for (expected, id) in ids.iter().enumerate() {
            assert_eq!(*id as usize, expected);
        }
    }

    #[test]
    fn structs_match_the_c_layout() {
        // A mismatch here is undefined behaviour that presents as garbled
        // geometry rather than a crash, so it is worth asserting explicitly.
        use std::mem::{align_of, size_of};
        assert_eq!(size_of::<GameGeometry>(), 4 * 4 + 4);
        assert_eq!(size_of::<SystemTiming>(), 8 + 8);
        assert_eq!(align_of::<SystemTiming>(), align_of::<f64>());
        assert_eq!(
            size_of::<SystemAvInfo>(),
            size_of::<GameGeometry>() + size_of::<SystemTiming>() + 4 // trailing pad
        );
    }
}
