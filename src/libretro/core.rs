//! Loading a libretro core and driving it.
//!
//! # Why this is full of global state
//!
//! libretro's callbacks carry no user-data pointer. `retro_set_video_refresh`
//! takes a bare `extern "C" fn`, so there is nowhere to thread a handle to
//! "which core instance is calling". Every frontend in existence therefore
//! keeps the receiving state in globals, and can only host one core at a time
//! per process. That is a property of the C API, not a shortcut taken here —
//! `INSTANCE_HELD` makes the limitation explicit and enforced rather than
//! latent.
//!
//! # Safety posture
//!
//! A core is arbitrary C++ compiled by someone else and loaded into our
//! address space. It can and does crash us: the Handy core segfaults inside
//! `retro_load_game` when its BIOS is missing rather than returning `false`
//! (`docs/libretro-spike.md`). Nothing in Rust can catch that. The mitigations
//! are therefore preventative — check before calling, and never let unsaved
//! progress live only in the core's memory.

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, anyhow, bail};
use libloading::{Library, Symbol};

use super::sys;
use super::video::{Frame, PixelFormat, convert};

/// Controller ports we accept input for. libretro allows more; two covers
/// every system in the supported set and keeps the input path a fixed array.
pub const MAX_PORTS: usize = 2;

// ─── Process-global receiving state ──────────────────────────────────────────

struct Shared {
    /// Most recent converted frame. `None` until the first non-duplicate frame.
    frame: Mutex<Option<Frame>>,
    /// Interleaved stereo i16, drained by the audio thread.
    audio: Mutex<Vec<i16>>,
    /// One bitmask per port, indexed by `RETRO_DEVICE_ID_JOYPAD_*`. Atomic so
    /// the input callback — invoked dozens of times per frame — never blocks.
    buttons: [AtomicU16; MAX_PORTS],
    pixel_format: AtomicU32,
    frames_seen: AtomicUsize,
    /// Set when the core asks the frontend to shut down.
    wants_shutdown: AtomicBool,
    /// Directories handed to the core. Leaked as C strings on first request
    /// because the core keeps the pointer indefinitely.
    system_dir: Mutex<Option<&'static CStr>>,
    save_dir: Mutex<Option<&'static CStr>>,
    /// Everything the core told us about itself, kept for the Logs tab. The
    /// interesting failures happen on someone else's machine, and "it didn't
    /// work" is unanswerable without this.
    diagnostics: Mutex<Diagnostics>,
}

/// What the core said, and what we refused to let it do.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    /// Messages the core emitted through the log interface, newest last.
    /// Bounded: a chatty core at 60 fps would otherwise flood the app's shared
    /// ring buffer within seconds and push out everything else.
    pub core_log: Vec<String>,
    /// Environment commands we declined, by number, in order and deduplicated.
    ///
    /// Worth its own field because it is often the whole explanation. A refused
    /// 14 (`SET_HW_RENDER`) means the core wanted a GL context and gave up —
    /// which presents to the user as a black screen and to us, without this, as
    /// nothing at all.
    pub refused_commands: Vec<u32>,
    /// Raw value from `SET_SERIALIZATION_QUIRKS`, or `None` if never set.
    pub serialization_quirks: Option<u64>,
    /// Pixel format the core actually chose, once it has.
    pub pixel_format: Option<&'static str>,
}

/// Enough to diagnose a session, small enough to paste into a message.
const MAX_CORE_LOG: usize = 300;

impl Shared {
    fn new() -> Self {
        Self {
            frame: Mutex::new(None),
            audio: Mutex::new(Vec::with_capacity(8192)),
            buttons: [const { AtomicU16::new(0) }; MAX_PORTS],
            pixel_format: AtomicU32::new(sys::RETRO_PIXEL_FORMAT_0RGB1555),
            frames_seen: AtomicUsize::new(0),
            wants_shutdown: AtomicBool::new(false),
            system_dir: Mutex::new(None),
            save_dir: Mutex::new(None),
            diagnostics: Mutex::new(Diagnostics::default()),
        }
    }

    fn reset(&self) {
        *lock(&self.frame) = None;
        lock(&self.audio).clear();
        for b in &self.buttons {
            b.store(0, Ordering::Relaxed);
        }
        // Back to the format a core gets if it never sets one, so a stale
        // choice from a previous core cannot be inherited.
        self.pixel_format
            .store(sys::RETRO_PIXEL_FORMAT_0RGB1555, Ordering::SeqCst);
        self.frames_seen.store(0, Ordering::SeqCst);
        self.wants_shutdown.store(false, Ordering::SeqCst);
        *lock(&self.diagnostics) = Diagnostics::default();
    }
}

/// A poisoned lock only means some other thread panicked while holding it. The
/// data behind it is a frame buffer or an audio queue — losing a frame is never
/// worth turning into a second panic.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(Shared::new)
}

/// Enforces the one-core-per-process rule the C API imposes.
static INSTANCE_HELD: AtomicBool = AtomicBool::new(false);

// ─── Callbacks ───────────────────────────────────────────────────────────────

/// Leak a path as a C string the core can hold forever.
///
/// Deliberate: cores stash the pointer from `GET_SYSTEM_DIRECTORY` and read it
/// long afterwards. Freeing it would be a use-after-free in someone else's C++.
/// One small leak per core load is the correct trade.
fn leak_cstr(path: &Path) -> Option<&'static CStr> {
    let s = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    Some(Box::leak(s.into_boxed_c_str()))
}

unsafe extern "C" {
    /// The C shim that does the `vsnprintf` stable Rust cannot.
    ///
    /// This is the pointer handed to cores. It formats the message and calls
    /// `rustromm_core_log_line` below with the finished string. See
    /// `src/libretro/shim/log_shim.c` for why the C is unavoidable.
    fn rustromm_log_shim(level: c_uint, fmt: *const c_char, ...);
}

/// Called by the shim with an already-formatted message.
///
/// `#[unsafe(no_mangle)]` because C resolves it by name at link time.
#[unsafe(no_mangle)]
extern "C" fn rustromm_core_log_line(level: c_uint, text: *const c_char) {
    if text.is_null() {
        return;
    }
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    record_core_log(level, text.trim_end_matches(['\n', '\r']));
}

fn record_core_log(level: c_uint, text: &str) {
    let tag = match level {
        sys::RETRO_LOG_ERROR => "ERR ",
        sys::RETRO_LOG_WARN => "WARN",
        sys::RETRO_LOG_DEBUG => "DBG ",
        _ => "INFO",
    };
    let line = format!("[core {tag}] {text}");

    let s = shared();
    let mut d = lock(&s.diagnostics);
    if d.core_log.len() >= MAX_CORE_LOG {
        d.core_log.remove(0);
    }
    d.core_log.push(line.clone());
    drop(d);

    // Errors and warnings also go to the app-wide log, because those are what
    // explain a failed load — genesis_plus_gx announces a missing Sega CD BIOS
    // only through this channel.
    match level {
        sys::RETRO_LOG_ERROR => crate::logging::error(line),
        sys::RETRO_LOG_WARN => crate::logging::warn(line),
        _ => {}
    }
}

fn note_refusal(cmd: c_uint) {
    let s = shared();
    let mut d = lock(&s.diagnostics);
    if !d.refused_commands.contains(&cmd) {
        d.refused_commands.push(cmd);
    }
}

unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    let s = shared();
    match sys::env_command(cmd) {
        // Deliberately refused, and the single most important refusal in the
        // project. Granting a hardware render context would pull in FBO
        // handoff, get_proc_address and OpenGL on a platform where Apple has
        // deprecated it. Saying no makes "software-rendered cores only" an
        // invariant rather than an aspiration, and a core that needs GL fails
        // here — visibly, in the log — instead of rendering nothing.
        sys::RETRO_ENVIRONMENT_SET_HW_RENDER | sys::RETRO_ENVIRONMENT_GET_PREFERRED_HW_RENDER => {
            note_refusal(sys::env_command(cmd));
            false
        }
        // Cores announce missing BIOS files and unsupported mappers here and
        // nowhere else. Without it a failed load is silent.
        sys::RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {
            if data.is_null() {
                return false;
            }
            unsafe {
                (*(data as *mut sys::LogCallback)).log = Some(rustromm_log_shim);
            }
            true
        }
        // Short player-facing notices. Recorded rather than displayed for now;
        // the point is that they stop vanishing.
        sys::RETRO_ENVIRONMENT_SET_MESSAGE | sys::RETRO_ENVIRONMENT_SET_MESSAGE_EXT => {
            if data.is_null() {
                return false;
            }
            // `msg` is the first field of both retro_message and
            // retro_message_ext, so reading it is layout-safe for either.
            let msg = unsafe { *(data as *const *const c_char) };
            if let Some(text) = cstr_to_string(msg) {
                let line = format!("[core MSG ] {text}");
                let mut d = lock(&s.diagnostics);
                if d.core_log.len() >= MAX_CORE_LOG {
                    d.core_log.remove(0);
                }
                d.core_log.push(line);
            }
            true
        }
        // The core is telling us which of our save-state assumptions are wrong.
        // Record it, then report support for none of the special cases by
        // writing back zero — an honest answer, and the one that stops us
        // uploading a state that could never be restored.
        sys::RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS => {
            if data.is_null() {
                return false;
            }
            let requested = unsafe { *(data as *const u64) };
            lock(&s.diagnostics).serialization_quirks = Some(requested);
            unsafe { *(data as *mut u64) = 0 };
            true
        }
        // Answering true means the core may pass a null frame to mean "same as
        // last time". Cores rely on this for skipped frames; saying false makes
        // them redundantly re-render.
        sys::RETRO_ENVIRONMENT_GET_CAN_DUPE => {
            if data.is_null() {
                return false;
            }
            unsafe { *(data as *mut bool) = true };
            true
        }
        sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
            if data.is_null() {
                return false;
            }
            let raw = unsafe { *(data as *const c_uint) };
            match PixelFormat::from_raw(raw) {
                Some(f) => {
                    s.pixel_format.store(raw, Ordering::SeqCst);
                    lock(&s.diagnostics).pixel_format = Some(f.name());
                    true
                }
                // Refusing an unknown format is correct: the core will fall
                // back to one we do understand rather than send us pixels we
                // would misinterpret.
                None => false,
            }
        }
        sys::RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY => {
            if data.is_null() {
                return false;
            }
            match *lock(&s.system_dir) {
                Some(dir) => {
                    unsafe { *(data as *mut *const c_char) = dir.as_ptr() };
                    true
                }
                None => false,
            }
        }
        sys::RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
            if data.is_null() {
                return false;
            }
            match *lock(&s.save_dir) {
                Some(dir) => {
                    unsafe { *(data as *mut *const c_char) = dir.as_ptr() };
                    true
                }
                None => false,
            }
        }
        // No core-option overrides yet, so report none set and no updates
        // pending. Answering GET_VARIABLE with false makes the core use its own
        // defaults, which is what we want until options are exposed in the UI.
        sys::RETRO_ENVIRONMENT_SET_VARIABLES => true,
        sys::RETRO_ENVIRONMENT_GET_VARIABLE => false,
        sys::RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
            if !data.is_null() {
                unsafe { *(data as *mut bool) = false };
            }
            true
        }
        sys::RETRO_ENVIRONMENT_SHUTDOWN => {
            s.wants_shutdown.store(true, Ordering::SeqCst);
            true
        }
        // Everything else is declined. The spike left thirteen commands
        // unanswered and all six working cores booted regardless — but which
        // ones were refused is recorded, because that list is frequently the
        // entire explanation for a core misbehaving.
        other => {
            note_refusal(other);
            false
        }
    }
}

unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    let s = shared();
    s.frames_seen.fetch_add(1, Ordering::Relaxed);

    // Null means "repeat the previous frame" — legal only because we answered
    // GET_CAN_DUPE with true. Keeping the existing frame is the whole point.
    if data.is_null() {
        return;
    }
    let format = PixelFormat::from_raw(s.pixel_format.load(Ordering::SeqCst)).unwrap_or_default();
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || pitch == 0 {
        return;
    }
    let len = pitch * (h - 1) + w * format.bytes_per_pixel();
    let src = unsafe { std::slice::from_raw_parts(data as *const u8, len) };
    // Size from the callback arguments, never from av_info: Stella advertises
    // 320x228 and delivers 160x228.
    if let Some(frame) = convert(src, w, h, pitch, format) {
        *lock(&s.frame) = Some(frame);
    }
}

unsafe extern "C" fn audio_sample(left: i16, right: i16) {
    let mut buf = lock(&shared().audio);
    buf.push(left);
    buf.push(right);
}

unsafe extern "C" fn audio_sample_batch(data: *const i16, frames: usize) -> usize {
    if data.is_null() || frames == 0 {
        return frames;
    }
    // `frames` counts stereo pairs, so the buffer is twice as long.
    let samples = unsafe { std::slice::from_raw_parts(data, frames * 2) };
    lock(&shared().audio).extend_from_slice(samples);
    frames
}

unsafe extern "C" fn input_poll() {
    // Input is pushed in by the frontend before `run()`, so there is nothing to
    // sample here. The core still calls it, and it must exist.
}

unsafe extern "C" fn input_state(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    if device != sys::RETRO_DEVICE_JOYPAD {
        return 0;
    }
    let (port, id) = (port as usize, id as usize);
    if port >= MAX_PORTS || id >= sys::JOYPAD_BUTTON_COUNT {
        return 0;
    }
    let mask = shared().buttons[port].load(Ordering::Relaxed);
    ((mask >> id) & 1) as i16
}

// ─── Resolved entry points ───────────────────────────────────────────────────

/// Raw function pointers pulled out of the library.
///
/// Stored as plain `fn` rather than `Symbol<'_>` so `Core` is not tangled in
/// self-referential lifetimes. Sound because `Core` owns the `Library` and
/// drops it last.
struct Api {
    init: unsafe extern "C" fn(),
    deinit: unsafe extern "C" fn(),
    get_system_info: unsafe extern "C" fn(*mut sys::SystemInfo),
    get_system_av_info: unsafe extern "C" fn(*mut sys::SystemAvInfo),
    load_game: unsafe extern "C" fn(*const sys::GameInfo) -> bool,
    unload_game: unsafe extern "C" fn(),
    run: unsafe extern "C" fn(),
    reset: unsafe extern "C" fn(),
    serialize_size: unsafe extern "C" fn() -> usize,
    serialize: unsafe extern "C" fn(*mut c_void, usize) -> bool,
    unserialize: unsafe extern "C" fn(*const c_void, usize) -> bool,
    get_memory_data: unsafe extern "C" fn(c_uint) -> *mut c_void,
    get_memory_size: unsafe extern "C" fn(c_uint) -> usize,
    set_controller_port_device: unsafe extern "C" fn(c_uint, c_uint),
}

/// What a core says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreInfo {
    pub name: String,
    pub version: String,
    /// Lower-cased, dot-free, as libretro reports them: `["gb", "gbc", "dmg"]`.
    pub extensions: Vec<String>,
    /// When true the core insists on a path and ignores an in-memory buffer.
    /// Genesis Plus GX does; most cores do not.
    pub need_fullpath: bool,
}

impl CoreInfo {
    pub fn handles_extension(&self, ext: &str) -> bool {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        self.extensions.contains(&ext)
    }
}

/// Timing and geometry, read once after the game loads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvInfo {
    pub fps: f64,
    pub sample_rate: f64,
    pub aspect_ratio: f32,
    pub max_width: u32,
    pub max_height: u32,
}

// ─── The core ────────────────────────────────────────────────────────────────

pub struct Core {
    api: Api,
    info: CoreInfo,
    game_loaded: bool,
    /// Held for `need_fullpath == false` cores: several read from the buffer
    /// during `run`, not only during `load_game`, so it must outlive the call.
    _rom: Option<Vec<u8>>,
    /// Dropped last. Unloading the library while its code is still referenced
    /// would be a use-after-free.
    _lib: Library,
}

macro_rules! resolve {
    ($lib:expr, $name:literal, $ty:ty) => {{
        let sym: Symbol<$ty> = unsafe { $lib.get(concat!($name, "\0").as_bytes()) }
            .with_context(|| format!("core is missing the {} entry point", $name))?;
        unsafe { *sym.into_raw() }
    }};
}

impl Core {
    /// Load a core from a shared library.
    ///
    /// Fails rather than blocking if another core is already loaded — the C API
    /// permits only one per process, and silently corrupting the first one's
    /// callbacks would be far worse than an error.
    pub fn load(path: &Path, system_dir: &Path, save_dir: &Path) -> Result<Self> {
        if INSTANCE_HELD.swap(true, Ordering::SeqCst) {
            bail!("a core is already loaded; libretro allows only one per process");
        }
        // From here on every early return must release the flag.
        match Self::load_inner(path, system_dir, save_dir) {
            Ok(core) => Ok(core),
            Err(e) => {
                INSTANCE_HELD.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    fn load_inner(path: &Path, system_dir: &Path, save_dir: &Path) -> Result<Self> {
        let lib = unsafe { Library::new(path) }
            .with_context(|| format!("could not load core {}", path.display()))?;

        let api_version = resolve!(lib, "retro_api_version", unsafe extern "C" fn() -> c_uint);
        let version = unsafe { api_version() };
        if version != sys::RETRO_API_VERSION {
            bail!(
                "core speaks libretro API {version}, this build speaks {}",
                sys::RETRO_API_VERSION
            );
        }

        let set_environment = resolve!(
            lib,
            "retro_set_environment",
            unsafe extern "C" fn(sys::EnvironmentFn)
        );
        let set_video = resolve!(
            lib,
            "retro_set_video_refresh",
            unsafe extern "C" fn(sys::VideoRefreshFn)
        );
        let set_audio = resolve!(
            lib,
            "retro_set_audio_sample",
            unsafe extern "C" fn(sys::AudioSampleFn)
        );
        let set_audio_batch = resolve!(
            lib,
            "retro_set_audio_sample_batch",
            unsafe extern "C" fn(sys::AudioSampleBatchFn)
        );
        let set_input_poll = resolve!(
            lib,
            "retro_set_input_poll",
            unsafe extern "C" fn(sys::InputPollFn)
        );
        let set_input_state = resolve!(
            lib,
            "retro_set_input_state",
            unsafe extern "C" fn(sys::InputStateFn)
        );

        let api = Api {
            init: resolve!(lib, "retro_init", unsafe extern "C" fn()),
            deinit: resolve!(lib, "retro_deinit", unsafe extern "C" fn()),
            get_system_info: resolve!(
                lib,
                "retro_get_system_info",
                unsafe extern "C" fn(*mut sys::SystemInfo)
            ),
            get_system_av_info: resolve!(
                lib,
                "retro_get_system_av_info",
                unsafe extern "C" fn(*mut sys::SystemAvInfo)
            ),
            load_game: resolve!(
                lib,
                "retro_load_game",
                unsafe extern "C" fn(*const sys::GameInfo) -> bool
            ),
            unload_game: resolve!(lib, "retro_unload_game", unsafe extern "C" fn()),
            run: resolve!(lib, "retro_run", unsafe extern "C" fn()),
            reset: resolve!(lib, "retro_reset", unsafe extern "C" fn()),
            serialize_size: resolve!(lib, "retro_serialize_size", unsafe extern "C" fn() -> usize),
            serialize: resolve!(
                lib,
                "retro_serialize",
                unsafe extern "C" fn(*mut c_void, usize) -> bool
            ),
            unserialize: resolve!(
                lib,
                "retro_unserialize",
                unsafe extern "C" fn(*const c_void, usize) -> bool
            ),
            get_memory_data: resolve!(
                lib,
                "retro_get_memory_data",
                unsafe extern "C" fn(c_uint) -> *mut c_void
            ),
            get_memory_size: resolve!(
                lib,
                "retro_get_memory_size",
                unsafe extern "C" fn(c_uint) -> usize
            ),
            set_controller_port_device: resolve!(
                lib,
                "retro_set_controller_port_device",
                unsafe extern "C" fn(c_uint, c_uint)
            ),
        };

        let s = shared();
        s.reset();
        std::fs::create_dir_all(system_dir).ok();
        std::fs::create_dir_all(save_dir).ok();
        *lock(&s.system_dir) = leak_cstr(system_dir);
        *lock(&s.save_dir) = leak_cstr(save_dir);

        // Order is load-bearing. `set_environment` must precede `retro_init`,
        // because cores issue environment calls from inside init — and some
        // issue them from inside `get_system_info`, before init at all.
        unsafe {
            set_environment(environment);
            set_video(video_refresh);
            set_audio(audio_sample);
            set_audio_batch(audio_sample_batch);
            set_input_poll(input_poll);
            set_input_state(input_state);
        }

        let mut raw = sys::SystemInfo::default();
        unsafe { (api.get_system_info)(&mut raw) };
        let info = CoreInfo {
            name: cstr_to_string(raw.library_name).unwrap_or_else(|| "unknown".into()),
            version: cstr_to_string(raw.library_version).unwrap_or_else(|| "unknown".into()),
            extensions: cstr_to_string(raw.valid_extensions)
                .unwrap_or_default()
                .split('|')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_ascii_lowercase())
                .collect(),
            need_fullpath: raw.need_fullpath,
        };

        unsafe { (api.init)() };

        Ok(Self {
            api,
            info,
            game_loaded: false,
            _rom: None,
            _lib: lib,
        })
    }

    pub fn info(&self) -> &CoreInfo {
        &self.info
    }

    /// Load a ROM.
    ///
    /// `rom` is the file contents and `path` where they live on disk. Both are
    /// required because the core decides which it wants via `need_fullpath`,
    /// and a core that wants a path will silently ignore the buffer.
    ///
    /// **This call can crash the process.** Handy segfaults here when its BIOS
    /// is absent instead of returning `false`. Callers must check prerequisites
    /// beforehand; there is no recovering afterwards.
    pub fn load_game(&mut self, path: &Path, rom: Vec<u8>) -> Result<AvInfo> {
        if self.game_loaded {
            bail!("a game is already loaded in this core");
        }
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| anyhow!("ROM path contains a null byte"))?;

        let game = sys::GameInfo {
            path: c_path.as_ptr(),
            data: if self.info.need_fullpath {
                std::ptr::null()
            } else {
                rom.as_ptr() as *const c_void
            },
            size: if self.info.need_fullpath {
                0
            } else {
                rom.len()
            },
            meta: std::ptr::null(),
        };

        let ok = unsafe { (self.api.load_game)(&game) };
        if !ok {
            bail!(
                "{} refused to load {}",
                self.info.name,
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
        self.game_loaded = true;
        // Cores that read from the buffer during `run` need it kept alive;
        // those using a path do not, but holding it costs nothing next to a
        // dangling pointer.
        self._rom = if self.info.need_fullpath {
            None
        } else {
            Some(rom)
        };

        let mut av = sys::SystemAvInfo::default();
        unsafe { (self.api.get_system_av_info)(&mut av) };

        // Default both ports to a standard pad. Without this some cores
        // present no controller at all.
        for port in 0..MAX_PORTS {
            unsafe {
                (self.api.set_controller_port_device)(port as c_uint, sys::RETRO_DEVICE_JOYPAD)
            };
        }

        Ok(AvInfo {
            // A core reporting nonsense timing would make the frame pacer
            // divide by zero, so fall back to something sane.
            fps: if av.timing.fps > 1.0 {
                av.timing.fps
            } else {
                60.0
            },
            sample_rate: if av.timing.sample_rate > 1.0 {
                av.timing.sample_rate
            } else {
                48000.0
            },
            aspect_ratio: av.geometry.aspect_ratio,
            max_width: av.geometry.max_width,
            max_height: av.geometry.max_height,
        })
    }

    /// Advance one frame. Cheap: 0.19–2.3 ms across the measured core set.
    pub fn run(&mut self) {
        unsafe { (self.api.run)() };
    }

    pub fn reset(&mut self) {
        if self.game_loaded {
            unsafe { (self.api.reset)() };
        }
    }

    /// Set the whole button state for a port at once.
    ///
    /// A bitmask rather than per-button calls so a frame's input lands
    /// atomically — a partially-applied mask would let the core see a
    /// combination the player never pressed.
    pub fn set_buttons(&self, port: usize, mask: u16) {
        if port < MAX_PORTS {
            shared().buttons[port].store(mask, Ordering::Relaxed);
        }
    }

    /// The latest frame, if the core has produced one.
    pub fn take_frame(&self) -> Option<Frame> {
        lock(&shared().frame).clone()
    }

    pub fn frames_seen(&self) -> usize {
        shared().frames_seen.load(Ordering::Relaxed)
    }

    /// Drain queued audio as interleaved stereo i16.
    pub fn drain_audio(&self) -> Vec<i16> {
        std::mem::take(&mut *lock(&shared().audio))
    }

    pub fn wants_shutdown(&self) -> bool {
        shared().wants_shutdown.load(Ordering::SeqCst)
    }

    /// What the core reported about itself, for the Logs tab.
    pub fn diagnostics(&self) -> Diagnostics {
        lock(&shared().diagnostics).clone()
    }

    /// Whether a save state from this core may be written to disk or uploaded.
    ///
    /// Three quirks make that unsafe, and a frontend that ignores them promises
    /// the user a saved game it can never give back:
    ///
    /// - `SINGLE_SESSION` — the state is only valid in the run that made it.
    /// - `ENDIAN_DEPENDENT` / `PLATFORM_DEPENDENT` — a state written on x86_64
    ///   macOS will not load on arm64 Linux. RomM is shared between machines of
    ///   different architectures by design, so syncing these is data loss with
    ///   extra steps.
    ///
    /// Returns the reason it is unsafe, or `None` when persisting is fine.
    pub fn state_persistence_blocker(&self) -> Option<&'static str> {
        let quirks = lock(&shared().diagnostics).serialization_quirks?;
        if quirks & sys::RETRO_SERIALIZATION_QUIRK_SINGLE_SESSION != 0 {
            return Some("this core's save states only work until you close the game");
        }
        if quirks
            & (sys::RETRO_SERIALIZATION_QUIRK_ENDIAN_DEPENDENT
                | sys::RETRO_SERIALIZATION_QUIRK_PLATFORM_DEPENDENT)
            != 0
        {
            return Some(
                "this core's save states are tied to this computer's processor, so they \
                 cannot be synced to other devices",
            );
        }
        None
    }

    /// Battery-backed cartridge save, if this game has one.
    ///
    /// Returns `None` for cartridges without one — the spike saw 0 bytes for
    /// Tetris 2 (no battery) and 64 KB for a Mega Drive title.
    pub fn save_ram(&self) -> Option<Vec<u8>> {
        let size = unsafe { (self.api.get_memory_size)(sys::RETRO_MEMORY_SAVE_RAM) };
        if size == 0 {
            return None;
        }
        let ptr = unsafe { (self.api.get_memory_data)(sys::RETRO_MEMORY_SAVE_RAM) };
        if ptr.is_null() {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }.to_vec())
    }

    /// Restore a previously saved SRAM image.
    ///
    /// Refuses on a size mismatch rather than writing what fits. A short write
    /// would leave half the old save in place and corrupt it in a way the user
    /// only discovers much later.
    pub fn restore_save_ram(&mut self, data: &[u8]) -> Result<()> {
        let size = unsafe { (self.api.get_memory_size)(sys::RETRO_MEMORY_SAVE_RAM) };
        if size == 0 {
            bail!("this game has no battery save");
        }
        if data.len() != size {
            bail!(
                "save is {} bytes but this game expects {size} — refusing to write a partial save",
                data.len()
            );
        }
        let ptr = unsafe { (self.api.get_memory_data)(sys::RETRO_MEMORY_SAVE_RAM) };
        if ptr.is_null() {
            bail!("core exposes no save memory");
        }
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, size) };
        Ok(())
    }

    /// Capture a save state. Sizes range from ~1 KB to ~1 MB by system.
    pub fn save_state(&mut self) -> Result<Vec<u8>> {
        let size = unsafe { (self.api.serialize_size)() };
        if size == 0 {
            // A zero here does not always mean "unsupported". Cores that set
            // MUST_INITIALIZE report zero until some frames have run, and a
            // frontend that treats the first zero as final disables save states
            // permanently for those cores.
            let must_init = lock(&shared().diagnostics)
                .serialization_quirks
                .is_some_and(|q| q & sys::RETRO_SERIALIZATION_QUIRK_MUST_INITIALIZE != 0);
            if must_init {
                bail!(
                    "{} cannot save a state yet — start the game first",
                    self.info.name
                );
            }
            bail!("{} does not support save states", self.info.name);
        }
        let mut buf = vec![0u8; size];
        let ok = unsafe { (self.api.serialize)(buf.as_mut_ptr() as *mut c_void, size) };
        if !ok {
            bail!("{} failed to write a save state", self.info.name);
        }
        Ok(buf)
    }

    /// Restore a save state.
    ///
    /// State layout is specific to a core *build*. A state written by a
    /// different version may be rejected — or, worse, accepted and produce a
    /// corrupted game. Callers must record which core wrote a state and refuse
    /// mismatches themselves; this cannot tell.
    pub fn load_state(&mut self, data: &[u8]) -> Result<()> {
        let expected = unsafe { (self.api.serialize_size)() };
        if expected == 0 {
            bail!("{} does not support save states", self.info.name);
        }
        if data.len() != expected {
            bail!(
                "save state is {} bytes but this core expects {expected} — it was probably \
                 written by a different version of {}",
                data.len(),
                self.info.name
            );
        }
        let ok = unsafe { (self.api.unserialize)(data.as_ptr() as *const c_void, data.len()) };
        if !ok {
            bail!("{} rejected the save state", self.info.name);
        }
        Ok(())
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        // Teardown order is fixed by the API: unload the game, then deinit,
        // then let the library go. Doing it out of order crashes some cores.
        unsafe {
            if self.game_loaded {
                (self.api.unload_game)();
            }
            (self.api.deinit)();
        }
        shared().reset();
        INSTANCE_HELD.store(false, Ordering::SeqCst);
    }
}

fn cstr_to_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Where a core's shared library lives on disk for the running platform.
pub fn core_file_name(core: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{core}_libretro.dll")
    } else if cfg!(target_os = "macos") {
        format!("{core}_libretro.dylib")
    } else {
        format!("{core}_libretro.so")
    }
}

/// Path within the cores directory for a named core.
pub fn core_path(cores_dir: &Path, core: &str) -> PathBuf {
    cores_dir.join(core_file_name(core))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shared()` and `INSTANCE_HELD` are process-global — they have to be,
    /// because libretro's callbacks carry no user-data pointer. Cargo runs
    /// tests in parallel threads of one process, so any test touching that
    /// state races every other one: a `reset()` lands in the middle of a
    /// neighbour's assertions and the failure looks like a logic bug rather
    /// than a scheduling one.
    ///
    /// Serialise them. This is the third time this bug class has appeared in
    /// this project (after the config env var and the log ring buffer), and
    /// each time it presented as an intermittent failure in unrelated code.
    static GLOBAL_STATE: Mutex<()> = Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        // Poisoning only means an earlier test panicked; the globals are still
        // ours to reset.
        GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn info(exts: &[&str], need_fullpath: bool) -> CoreInfo {
        CoreInfo {
            name: "Test".into(),
            version: "1.0".into(),
            extensions: exts.iter().map(|e| e.to_string()).collect(),
            need_fullpath,
        }
    }

    #[test]
    fn extension_matching_ignores_case_and_a_leading_dot() {
        let i = info(&["gb", "gbc", "dmg"], false);
        assert!(i.handles_extension("gb"));
        assert!(i.handles_extension(".gb"));
        assert!(i.handles_extension("GBC"));
        assert!(i.handles_extension(".DMG"));
        assert!(!i.handles_extension("nes"));
        // A real library has files like "Game (U) [!].GG" — the case in the
        // filename is not the case in the core's list.
        assert!(info(&["gg"], false).handles_extension("GG"));
    }

    #[test]
    fn core_file_names_use_the_platform_extension() {
        let n = core_file_name("gambatte");
        assert!(n.starts_with("gambatte_libretro."));
        let expected = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };
        assert!(n.ends_with(expected), "{n} should end with {expected}");
    }

    #[test]
    fn core_paths_land_inside_the_cores_directory() {
        let p = core_path(Path::new("/tmp/cores"), "snes9x");
        assert!(p.starts_with("/tmp/cores"));
        assert_eq!(p.file_name().unwrap(), core_file_name("snes9x").as_str());
    }

    #[test]
    fn loading_a_file_that_is_not_a_core_fails_without_holding_the_instance_lock() {
        let _guard = exclusive();
        // The failure path must release the single-instance flag, or one bad
        // load would make every later load fail with a misleading message.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("not_a_core.so");
        std::fs::write(&fake, b"this is not an ELF shared object").unwrap();

        assert!(Core::load(&fake, dir.path(), dir.path()).is_err());
        assert!(
            !INSTANCE_HELD.load(Ordering::SeqCst),
            "a failed load left the instance lock held"
        );
        // Proven by a second attempt reaching the same error rather than
        // "already loaded".
        let err = match Core::load(&fake, dir.path(), dir.path()) {
            Ok(_) => panic!("a text file loaded as a core"),
            Err(e) => e.to_string(),
        };
        assert!(!err.contains("already loaded"), "got: {err}");
    }

    #[test]
    fn button_masks_are_read_back_per_port_and_bit() {
        let _guard = exclusive();
        let s = shared();
        for b in &s.buttons {
            b.store(0, Ordering::Relaxed);
        }
        s.buttons[0].store(1 << sys::RETRO_DEVICE_ID_JOYPAD_A, Ordering::Relaxed);
        s.buttons[1].store(1 << sys::RETRO_DEVICE_ID_JOYPAD_START, Ordering::Relaxed);

        unsafe {
            // Port 0 has A pressed and nothing else.
            assert_eq!(
                input_state(
                    0,
                    sys::RETRO_DEVICE_JOYPAD,
                    0,
                    sys::RETRO_DEVICE_ID_JOYPAD_A
                ),
                1
            );
            assert_eq!(
                input_state(
                    0,
                    sys::RETRO_DEVICE_JOYPAD,
                    0,
                    sys::RETRO_DEVICE_ID_JOYPAD_B
                ),
                0
            );
            // Ports are independent.
            assert_eq!(
                input_state(
                    1,
                    sys::RETRO_DEVICE_JOYPAD,
                    0,
                    sys::RETRO_DEVICE_ID_JOYPAD_START
                ),
                1
            );
            assert_eq!(
                input_state(
                    1,
                    sys::RETRO_DEVICE_JOYPAD,
                    0,
                    sys::RETRO_DEVICE_ID_JOYPAD_A
                ),
                0
            );
            // Out-of-range port, id and non-joypad device must not read out of
            // bounds — they are reachable from C and cannot be trusted.
            assert_eq!(
                input_state(
                    99,
                    sys::RETRO_DEVICE_JOYPAD,
                    0,
                    sys::RETRO_DEVICE_ID_JOYPAD_A
                ),
                0
            );
            assert_eq!(input_state(0, sys::RETRO_DEVICE_JOYPAD, 0, 999), 0);
            assert_eq!(input_state(0, sys::RETRO_DEVICE_MOUSE, 0, 0), 0);
        }
        for b in &s.buttons {
            b.store(0, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_environment_callback_answers_the_commands_cores_depend_on() {
        let _guard = exclusive();
        unsafe {
            // CAN_DUPE must be true or cores redundantly re-render every frame.
            let mut dupe = false;
            assert!(environment(
                sys::RETRO_ENVIRONMENT_GET_CAN_DUPE,
                &mut dupe as *mut bool as *mut c_void
            ));
            assert!(dupe);

            // A known pixel format is accepted and recorded.
            let mut fmt = sys::RETRO_PIXEL_FORMAT_RGB565;
            assert!(environment(
                sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                &mut fmt as *mut c_uint as *mut c_void
            ));
            assert_eq!(
                shared().pixel_format.load(Ordering::SeqCst),
                sys::RETRO_PIXEL_FORMAT_RGB565
            );

            // An unknown one is refused, so the core picks again rather than
            // sending pixels we would misread.
            let mut bad: c_uint = 999;
            assert!(!environment(
                sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                &mut bad as *mut c_uint as *mut c_void
            ));
            assert_eq!(
                shared().pixel_format.load(Ordering::SeqCst),
                sys::RETRO_PIXEL_FORMAT_RGB565,
                "a refused format must not overwrite the accepted one"
            );

            // Unknown commands decline rather than crash.
            assert!(!environment(31337, std::ptr::null_mut()));
        }
        shared().reset();
    }

    #[test]
    fn a_hardware_render_request_is_refused_and_recorded() {
        let _guard = exclusive();
        shared().reset();
        unsafe {
            // The invariant the whole project rests on: no core ever gets a GL
            // context, so no core can drag OpenGL back in.
            let mut junk = [0u8; 64];
            assert!(!environment(
                sys::RETRO_ENVIRONMENT_SET_HW_RENDER,
                junk.as_mut_ptr() as *mut c_void
            ));
            assert!(!environment(
                sys::RETRO_ENVIRONMENT_GET_PREFERRED_HW_RENDER,
                junk.as_mut_ptr() as *mut c_void
            ));
        }
        // Recorded, because a black screen with a refused 14 in the log is a
        // diagnosis, and a black screen without one is a mystery.
        let refused = shared()
            .diagnostics
            .lock()
            .unwrap()
            .refused_commands
            .clone();
        assert!(refused.contains(&sys::RETRO_ENVIRONMENT_SET_HW_RENDER));
        assert!(refused.contains(&sys::RETRO_ENVIRONMENT_GET_PREFERRED_HW_RENDER));
        shared().reset();
    }

    #[test]
    fn refused_commands_are_recorded_once_each() {
        let _guard = exclusive();
        shared().reset();
        unsafe {
            for _ in 0..5 {
                let _ = environment(31337, std::ptr::null_mut());
                let _ = environment(31338, std::ptr::null_mut());
            }
        }
        let refused = shared()
            .diagnostics
            .lock()
            .unwrap()
            .refused_commands
            .clone();
        // Deduplicated: a command refused every frame must not fill the report.
        assert_eq!(refused, vec![31337, 31338]);
        shared().reset();
    }

    #[test]
    fn core_log_messages_are_formatted_through_the_c_shim() {
        let _guard = exclusive();
        shared().reset();

        // Gambatte logs everything as ("[Gambatte] %s\n", text), so a
        // non-variadic callback would capture the format string and lose the
        // entire message. This goes through the real shim.
        let fmt = CString::new("[Test] %s loaded, %d banks").unwrap();
        let arg = CString::new("MBC1 ROM").unwrap();
        unsafe {
            rustromm_log_shim(sys::RETRO_LOG_INFO, fmt.as_ptr(), arg.as_ptr(), 8);
            rustromm_log_shim(
                sys::RETRO_LOG_ERROR,
                CString::new("missing BIOS: %s").unwrap().as_ptr(),
                CString::new("lynxboot.img").unwrap().as_ptr(),
            );
            // Cores do pass null; must not crash inside our own callback.
            rustromm_core_log_line(sys::RETRO_LOG_INFO, std::ptr::null());
        }

        let log = shared().diagnostics.lock().unwrap().core_log.clone();
        assert_eq!(log.len(), 2, "a null message should be dropped, not logged");
        assert_eq!(log[0], "[core INFO] [Test] MBC1 ROM loaded, 8 banks");
        // The message that actually matters: without the shim this would read
        // "missing BIOS: %s" and name no file at all.
        assert_eq!(log[1], "[core ERR ] missing BIOS: lynxboot.img");

        shared().reset();
    }

    #[test]
    fn the_core_log_is_bounded() {
        let _guard = exclusive();
        shared().reset();
        let msg = CString::new("chatter").unwrap();
        unsafe {
            for _ in 0..(MAX_CORE_LOG + 100) {
                rustromm_log_shim(sys::RETRO_LOG_INFO, msg.as_ptr());
            }
        }
        // A core logging every frame would otherwise grow without limit for as
        // long as someone plays.
        assert_eq!(
            shared().diagnostics.lock().unwrap().core_log.len(),
            MAX_CORE_LOG
        );
        shared().reset();
    }

    #[test]
    fn serialization_quirks_decide_whether_a_state_may_be_persisted() {
        let _guard = exclusive();

        let set = |quirks: u64| {
            shared().reset();
            let mut q = quirks;
            unsafe {
                assert!(environment(
                    sys::RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS,
                    &mut q as *mut u64 as *mut c_void
                ));
            }
            // We must report supporting none of them, or a core will assume
            // capabilities we do not have.
            assert_eq!(q, 0, "the frontend must write back the quirks it accepts");
            shared().diagnostics.lock().unwrap().serialization_quirks
        };

        assert_eq!(set(0), Some(0));
        assert_eq!(
            set(sys::RETRO_SERIALIZATION_QUIRK_SINGLE_SESSION),
            Some(sys::RETRO_SERIALIZATION_QUIRK_SINGLE_SESSION)
        );
        shared().reset();
    }

    #[test]
    fn the_environment_callback_survives_null_data() {
        let _guard = exclusive();
        // Cores do pass null. Dereferencing it would be a segfault inside our
        // own callback, which is the failure mode hardest to diagnose remotely.
        unsafe {
            for cmd in [
                sys::RETRO_ENVIRONMENT_GET_CAN_DUPE,
                sys::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                sys::RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY,
                sys::RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY,
                sys::RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE,
                sys::RETRO_ENVIRONMENT_SHUTDOWN,
            ] {
                let _ = environment(cmd, std::ptr::null_mut());
            }
        }
        shared().reset();
    }

    #[test]
    fn a_null_video_frame_keeps_the_previous_one() {
        let _guard = exclusive();
        // GET_CAN_DUPE is answered true, so a null frame means "same again".
        // Clearing the frame instead would make the picture flicker.
        let s = shared();
        s.reset();
        s.pixel_format
            .store(sys::RETRO_PIXEL_FORMAT_RGB565, Ordering::SeqCst);

        let src = vec![0xFFu8; 512 * 144];
        unsafe { video_refresh(src.as_ptr() as *const c_void, 160, 144, 512) };
        assert!(lock(&s.frame).is_some());
        let before = lock(&s.frame).clone();

        unsafe { video_refresh(std::ptr::null(), 160, 144, 512) };
        assert_eq!(
            *lock(&s.frame),
            before,
            "a duplicate frame wiped the picture"
        );
        // The duplicate still counts as a delivered frame for pacing.
        assert_eq!(s.frames_seen.load(Ordering::Relaxed), 2);
        s.reset();
    }

    #[test]
    fn audio_batches_are_appended_as_interleaved_stereo() {
        let _guard = exclusive();
        let s = shared();
        s.reset();
        let samples: Vec<i16> = (0..8).collect();
        // `frames` counts stereo pairs, so four frames is eight samples.
        let taken = unsafe { audio_sample_batch(samples.as_ptr(), 4) };
        assert_eq!(taken, 4);
        assert_eq!(*lock(&s.audio), samples);

        unsafe { audio_sample(100, -100) };
        assert_eq!(lock(&s.audio).len(), 10);
        assert_eq!(&lock(&s.audio)[8..], &[100, -100]);

        // A null buffer must be tolerated and still reported as consumed.
        assert_eq!(unsafe { audio_sample_batch(std::ptr::null(), 4) }, 4);
        s.reset();
    }
}
