//! End-to-end tests against a real libretro core.
//!
//! Everything else in the suite runs without a core. These do not — they exist
//! to catch the failures that only appear when actual compiled C++ is on the
//! other side of the FFI: a struct laid out wrongly, a callback registered in
//! the wrong order, a pointer that was supposed to stay alive.
//!
//! **No ROM is needed and none is shipped.** Gambatte accepts a zeroed 32 KB
//! buffer as a cartridge and renders a blank Game Boy screen from it, which is
//! enough to exercise load, run, video, save states and teardown. That keeps
//! the suite free of copyrighted content and makes it runnable in CI on every
//! platform.
//!
//! Skipped unless a core is provided, so `cargo test` stays offline:
//!
//! ```sh
//! RUSTROMM_TEST_CORE=/path/to/gambatte_libretro.so cargo test --test core_tests
//! ```
//!
//! CI fetches that core from the libretro buildbot per target. Note that
//! Windows arm64 is not covered: the buildbot publishes no arm64 Windows cores
//! at all. See `docs/libretro-spike.md`.

use std::path::PathBuf;
use std::sync::Mutex;

use rustromm::libretro::core::Core;
use rustromm::libretro::sys;

/// Only one core may be loaded per process — libretro's callbacks have nowhere
/// to carry an instance handle — so these tests cannot run in parallel.
static SERIAL: Mutex<()> = Mutex::new(());

fn guard() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// The core under test, or `None` to skip.
fn core_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("RUSTROMM_TEST_CORE").ok()?);
    p.exists().then_some(p)
}

/// A 32 KB cartridge of zeroes.
///
/// Not a real game: no Nintendo logo, no code, no header checksum. Gambatte
/// loads it anyway and shows the blank white LCD a Game Boy displays with
/// nothing to run, which is exactly the observable behaviour these tests need.
fn blank_cartridge() -> Vec<u8> {
    vec![0u8; 32 * 1024]
}

struct Loaded {
    core: Core,
    _dir: tempfile::TempDir,
    rom_path: PathBuf,
}

fn load() -> Option<Loaded> {
    let path = core_path()?;
    let dir = tempfile::tempdir().unwrap();
    let rom_path = dir.path().join("blank.gb");
    std::fs::write(&rom_path, blank_cartridge()).unwrap();
    let core = Core::load(&path, dir.path(), dir.path()).expect("core failed to load");
    Some(Loaded {
        core,
        _dir: dir,
        rom_path,
    })
}

macro_rules! skip_without_core {
    () => {
        match load() {
            Some(l) => l,
            None => {
                eprintln!("skipping: set RUSTROMM_TEST_CORE to a libretro core to run this");
                return;
            }
        }
    };
}

#[test]
fn a_core_reports_who_it_is() {
    let _g = guard();
    let l = skip_without_core!();
    let info = l.core.info();

    // A garbled name is the first sign SystemInfo is laid out wrongly — the
    // failure mode that otherwise shows up much later as nonsense geometry.
    assert!(!info.name.is_empty());
    assert!(
        info.name.is_ascii(),
        "library_name came back as mojibake: {:?}",
        info.name
    );
    assert!(
        !info.extensions.is_empty(),
        "core claims to handle no file types"
    );
    assert!(
        info.extensions
            .iter()
            .all(|e| !e.starts_with('.') && e == &e.to_lowercase()),
        "extensions should be bare and lower-case: {:?}",
        info.extensions
    );
}

#[test]
fn loading_a_game_yields_plausible_timing_and_geometry() {
    let _g = guard();
    let mut l = skip_without_core!();
    let av = l
        .core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");

    // Real cores report awkward numbers — 59.7275 fps, 32768 Hz — so assert on
    // plausible ranges rather than round values.
    assert!(
        (20.0..=130.0).contains(&av.fps),
        "implausible fps: {}",
        av.fps
    );
    assert!(
        (8000.0..=200_000.0).contains(&av.sample_rate),
        "implausible sample rate: {}",
        av.sample_rate
    );
    assert!(av.max_width > 0 && av.max_height > 0);
}

#[test]
fn running_produces_frames_with_real_pixels() {
    let _g = guard();
    let mut l = skip_without_core!();
    l.core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");

    for _ in 0..60 {
        l.core.run();
    }

    assert!(
        l.core.frames_seen() >= 60,
        "got {} frames from 60 runs",
        l.core.frames_seen()
    );
    let frame = l.core.take_frame().expect("no frame was ever delivered");
    assert!(frame.width > 0 && frame.height > 0);
    assert_eq!(frame.rgba.len(), frame.width * frame.height * 4);

    // A core that loads but renders nothing returns all-zero pixels, which
    // would otherwise pass every structural check above. A blank Game Boy LCD
    // is white, not black.
    assert!(
        !frame.is_blank(),
        "every pixel was black — the core loaded but produced no picture, or the \
         pixel format was misread"
    );
    assert!(
        frame.rgba.chunks_exact(4).all(|p| p[3] == 255),
        "alpha must be opaque or the frame renders invisible"
    );
}

#[test]
fn audio_is_produced_at_roughly_the_advertised_rate() {
    let _g = guard();
    let mut l = skip_without_core!();
    let av = l
        .core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");

    let frames = 60;
    for _ in 0..frames {
        l.core.run();
    }
    let audio = l.core.drain_audio();
    assert!(!audio.is_empty(), "core produced no audio at all");
    assert_eq!(audio.len() % 2, 0, "audio must be interleaved stereo pairs");

    // Sample count should track sample_rate / fps per frame. Wide tolerance:
    // cores buffer unevenly and the first frames after load are atypical.
    let expected = (av.sample_rate / av.fps * frames as f64) as usize;
    let got = audio.len() / 2;
    assert!(
        got > expected / 4 && got < expected * 4,
        "got {got} audio frames, expected around {expected} — the batch callback \
         may be misreading `frames` as samples rather than stereo pairs"
    );

    // Draining must actually empty the queue.
    assert!(l.core.drain_audio().is_empty());
}

#[test]
fn a_save_state_restores_the_exact_same_picture() {
    let _g = guard();
    let mut l = skip_without_core!();
    l.core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");

    for _ in 0..120 {
        l.core.run();
    }
    let state = l.core.save_state().expect("save_state failed");
    assert!(!state.is_empty());

    let at_save = l.core.take_frame().expect("no frame at save time");

    // Diverge, then rewind.
    for _ in 0..120 {
        l.core.run();
    }
    l.core.load_state(&state).expect("load_state failed");
    l.core.run();
    let after_restore = l.core.take_frame().expect("no frame after restore");

    assert_eq!(
        after_restore.width, at_save.width,
        "geometry changed across a state restore"
    );
    // Emulation is deterministic, so one frame on from a restored state must
    // match one frame on from where the state was taken.
    l.core.load_state(&state).expect("second load_state failed");
    l.core.run();
    let again = l.core.take_frame().unwrap();
    assert_eq!(
        after_restore, again,
        "restoring the same state twice gave different results — the state is \
         incomplete or something outside it is carrying over"
    );
}

#[test]
fn a_save_state_from_the_wrong_build_is_refused_rather_than_applied() {
    let _g = guard();
    let mut l = skip_without_core!();
    l.core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");
    l.core.run();

    let mut state = l.core.save_state().expect("save_state failed");
    state.truncate(state.len() / 2);

    // Silently accepting a short state corrupts the game in a way the player
    // discovers much later. Refusing is the only safe answer.
    let err = l.core.load_state(&state).unwrap_err().to_string();
    assert!(
        err.contains("bytes") || err.contains("version"),
        "unhelpful error for a mismatched state: {err}"
    );
}

#[test]
fn buttons_reach_the_core_without_panicking() {
    let _g = guard();
    let mut l = skip_without_core!();
    l.core
        .load_game(&l.rom_path.clone(), blank_cartridge())
        .expect("load_game failed");

    // Every button, on every port, then released. The core polls input from
    // inside `run`, so this exercises the callback under real conditions —
    // including out-of-range values it must survive.
    for id in 0..sys::JOYPAD_BUTTON_COUNT {
        l.core.set_buttons(0, 1 << id);
        l.core.set_buttons(1, 1 << id);
        l.core.run();
    }
    l.core.set_buttons(0, u16::MAX);
    l.core.run();
    l.core.set_buttons(0, 0);
    l.core.set_buttons(99, 1); // ignored, must not panic
    l.core.run();

    assert!(l.core.frames_seen() > 0);
}

#[test]
fn only_one_core_can_be_loaded_at_a_time() {
    let _g = guard();
    let l = skip_without_core!();
    let path = core_path().unwrap();
    let dir = tempfile::tempdir().unwrap();

    // The C API has no way to distinguish instances, so a second load must
    // fail loudly rather than silently redirect the first core's callbacks.
    let second = Core::load(&path, dir.path(), dir.path());
    assert!(
        second.is_err(),
        "a second core loaded while the first was live"
    );

    drop(l);
    // ...and the slot must be released, or one game per launch.
    let third = Core::load(&path, dir.path(), dir.path());
    assert!(third.is_ok(), "the instance slot was not released on drop");
}

#[test]
fn a_core_can_be_loaded_and_dropped_repeatedly() {
    let _g = guard();
    if core_path().is_none() {
        eprintln!("skipping: set RUSTROMM_TEST_CORE to a libretro core to run this");
        return;
    }
    // Playing several games in one session means load/deinit/unload happening
    // over and over. Getting the teardown order wrong crashes some cores, and
    // it would show up here rather than in the user's living room.
    for _ in 0..3 {
        let mut l = load().unwrap();
        l.core
            .load_game(&l.rom_path.clone(), blank_cartridge())
            .expect("load_game failed");
        for _ in 0..10 {
            l.core.run();
        }
        assert!(l.core.take_frame().is_some());
    }
}
