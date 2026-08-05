//! Headless UI tests.
//!
//! `egui_kittest` runs the real widget tree without a window or GPU, so these
//! drive the actual app: typing into fields, clicking buttons, and asserting on
//! what the accessibility tree exposes. Combined with a mock RomM server they
//! cover the full path from keystroke to rendered library.
//!
//! Every test points `RUSTROMM_CONFIG_DIR` at a temp directory so a test run
//! can never read or overwrite a real user's settings.

use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use httpmock::prelude::*;
use rustromm::app::RustRomm;
use rustromm::config::Config;

/// Serialises tests that set the config-dir environment variable.
///
/// Cargo runs tests in parallel threads within one process, and an environment
/// variable is process-global — without this lock, one test's temp directory
/// silently replaces another's and the config assertions become flaky.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Points config at a throwaway directory and holds the lock for the test's
/// lifetime. Must be bound to a named variable, not `_`, or it drops instantly.
struct ConfigGuard {
    dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ConfigGuard {
    fn path(&self) -> &std::path::Path {
        self.dir.path()
    }
}

fn isolated_config() -> ConfigGuard {
    // A poisoned lock just means an earlier test panicked; the environment is
    // still ours to overwrite, so recover rather than cascade the failure.
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("temp dir");
    // SAFETY: the lock above guarantees no other test thread is reading or
    // writing this variable. Rust 2024 requires the unsafe block.
    unsafe { std::env::set_var(Config::CONFIG_DIR_ENV, dir.path()) };
    ConfigGuard { dir, _lock: lock }
}

fn working_server() -> MockServer {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/heartbeat");
        then.status(200)
            .json_body(serde_json::json!({ "SYSTEM": { "VERSION": "5.1.0" } }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/api/platforms");
        then.status(200).json_body(serde_json::json!([
            { "id": 34, "name": "Nintendo Entertainment System", "slug": "nes",
              "fs_slug": "nes", "rom_count": 2, "custom_name": null },
            // rom_count 0 — should be hidden from the sidebar.
            { "id": 99, "name": "Empty Console", "slug": "empty",
              "fs_slug": "empty", "rom_count": 0, "custom_name": null }
        ]));
    });
    server.mock(|when, then| {
        when.method(GET).path("/api/roms");
        then.status(200).json_body(serde_json::json!({
            "total": 2,
            "items": [
                { "id": 1, "name": "Super Mario Bros.", "fs_name": "smb.nes",
                  "fs_extension": "nes", "fs_size_bytes": 40976, "platform_id": 34,
                  "platform_slug": "nes", "platform_display_name": "NES",
                  "path_cover_small": "", "has_multiple_files": false,
                  "files": [], "missing_from_fs": false },
                { "id": 2, "name": "Metroid", "fs_name": "metroid.nes",
                  "fs_extension": "nes", "fs_size_bytes": 131072, "platform_id": 34,
                  "platform_slug": "nes", "platform_display_name": "NES",
                  "path_cover_small": "", "has_multiple_files": false,
                  "files": [], "missing_from_fs": true }
            ]
        }));
    });
    server
}

fn config_for(server: &MockServer) -> Config {
    Config {
        server_url: server.base_url(),
        username: "ali".into(),
        password: "hunter2".into(),
        ..Config::default()
    }
}

/// Build a harness around the app.
///
/// Note that a config carrying a server, username *and* password connects
/// immediately on construction — that is the returning-user path. Pass a blank
/// `Config` to land on the connect screen instead.
fn harness_with(config: Config) -> Harness<'static, RustRomm> {
    Harness::new_ui_state(|ui, app: &mut RustRomm| app.render(ui), {
        let ctx = egui::Context::default();
        RustRomm::with_config(&ctx, config)
    })
}

#[test]
fn connect_screen_shows_the_expected_fields() {
    let _guard = isolated_config();
    let mut h = harness_with(Config::default());
    h.run();

    // Labels a user would look for on first launch.
    assert!(h.query_by_label("Server address").is_some());
    assert!(h.query_by_label("Username").is_some());
    assert!(h.query_by_label("Password").is_some());
    assert!(h.query_by_label("Connect").is_some());
    assert!(h.state().on_connect_screen());
}

#[test]
fn connecting_with_good_credentials_opens_the_library() {
    let _guard = isolated_config();
    let server = working_server();
    let mut h = harness_with(config_for(&server));
    h.run();

    // Saved credentials mean the app connects on construction — this is the
    // returning-user path, so there is no button to press.

    let ctx = h.ctx.clone();
    let connected = h
        .state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.on_library_screen());
    assert!(connected, "should have reached the library screen");

    h.run();
    let state = h.state();
    assert_eq!(state.total_rom_count(), 2);
    assert_eq!(state.visible_rom_count(), 2);
    // The zero-ROM platform is filtered out of the sidebar.
    assert_eq!(state.platform_count(), 1);
    assert!(state.last_error().is_none());
}

#[test]
fn the_library_lists_game_titles() {
    let _guard = isolated_config();
    let server = working_server();
    let mut h = harness_with(config_for(&server));
    h.run();

    let ctx = h.ctx.clone();
    h.state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.on_library_screen());
    h.run();

    assert!(h.query_by_label("Super Mario Bros.").is_some());
    assert!(h.query_by_label("Metroid").is_some());
    assert!(
        h.query_by_label("Nintendo Entertainment System (2)")
            .is_some()
    );
}

#[test]
fn a_game_missing_on_the_server_is_flagged_in_the_list() {
    let _guard = isolated_config();
    let server = working_server();
    let mut h = harness_with(config_for(&server));
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.on_library_screen());
    h.run();

    // Metroid is flagged missing_from_fs. Its row must say so, and its
    // Download button is disabled (the refusal itself is covered by
    // `download_of_a_file_missing_on_the_server_fails_before_any_request`).
    let flagged = h.query_all_by_label_contains("missing on server").count();
    assert_eq!(flagged, 1, "exactly one row should be flagged as missing");

    // The healthy game's row shows its size instead.
    assert!(h.query_by_label_contains("40.0 KB").is_some());
}

#[test]
fn bad_credentials_keep_you_on_the_connect_screen_with_an_error() {
    let _guard = isolated_config();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/heartbeat");
        then.status(200)
            .json_body(serde_json::json!({ "SYSTEM": { "VERSION": "5.1.0" } }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/api/platforms");
        then.status(401);
    });

    let mut h = harness_with(config_for(&server));
    h.run();

    let ctx = h.ctx.clone();
    let got_error = h
        .state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.last_error().is_some());
    assert!(got_error, "an authentication failure should surface");

    h.run();
    assert!(
        h.state().on_connect_screen(),
        "must not proceed to the library"
    );
    assert!(
        h.state()
            .last_error()
            .unwrap()
            .contains("username or password")
    );
}

#[test]
fn an_unreachable_server_reports_an_error_and_does_not_hang() {
    let _guard = isolated_config();
    let config = Config {
        // Nothing listens on port 1.
        server_url: "http://127.0.0.1:1".into(),
        username: "ali".into(),
        password: "hunter2".into(),
        ..Config::default()
    };
    let mut h = harness_with(config);
    h.run();

    let ctx = h.ctx.clone();
    let got_error = h
        .state_mut()
        .pump_until(&ctx, Duration::from_secs(30), |a| a.last_error().is_some());
    assert!(got_error, "connection refusal should surface as an error");
    assert!(h.state().on_connect_screen());
}

#[test]
fn settings_are_written_to_the_overridden_config_dir() {
    let guard = isolated_config();
    let server = working_server();
    let mut h = harness_with(config_for(&server));
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.on_library_screen());

    // A successful connect persists settings — and must do so in the temp dir,
    // never in the real user config location.
    let written = guard.path().join("config.json");
    assert!(written.exists(), "config.json should be written on connect");

    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
    assert_eq!(saved["username"], "ali");
    // remember_password defaults to false, so the password must not be stored.
    assert_eq!(saved["password"], "");
}

// ---------------------------------------------------------------- navigation
//
// Keyboard navigation is the substrate controller support is built on: both
// funnel into the same `NavAction` enum, so exercising the keys here covers the
// selection, paging and confirm logic a pad drives.
//
// What these tests do NOT cover: reading a real controller. That needs physical
// hardware. The gamepad-to-action mapping is a pure function unit-tested in
// `src/input.rs`; the polling layer between it and gilrs is not automatically
// tested at all.

/// Connect and wait for the library, ready to send key presses.
fn library_harness() -> (Harness<'static, RustRomm>, MockServer) {
    let server = working_server();
    let mut h = harness_with(config_for(&server));
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut()
        .pump_until(&ctx, Duration::from_secs(10), |a| a.on_library_screen());
    h.run();
    (h, server)
}

#[test]
fn nothing_is_highlighted_until_you_press_a_key() {
    let _guard = isolated_config();
    let (h, _server) = library_harness();
    assert_eq!(h.state().selected_index(), None);
}

#[test]
fn arrow_down_highlights_the_first_game_then_walks_the_list() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected_index(), Some(0));

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected_index(), Some(1));
}

#[test]
fn the_highlight_stops_at_the_end_rather_than_wrapping() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    // Only two games in the fixture; push well past the end.
    for _ in 0..6 {
        h.key_press(egui::Key::ArrowDown);
        h.run();
    }
    assert_eq!(h.state().selected_index(), Some(1));

    for _ in 0..6 {
        h.key_press(egui::Key::ArrowUp);
        h.run();
    }
    assert_eq!(h.state().selected_index(), Some(0));
}

#[test]
fn arrow_up_from_nothing_selects_the_last_game() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    // Reaching backwards from no selection should land at the bottom, not
    // jump to an arbitrary middle.
    h.key_press(egui::Key::ArrowUp);
    h.run();
    assert_eq!(h.state().selected_index(), Some(1));
}

#[test]
fn vim_keys_move_the_highlight_too() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    h.key_press(egui::Key::J);
    h.run();
    assert_eq!(h.state().selected_index(), Some(0));

    h.key_press(egui::Key::J);
    h.run();
    assert_eq!(h.state().selected_index(), Some(1));

    h.key_press(egui::Key::K);
    h.run();
    assert_eq!(h.state().selected_index(), Some(0));
}

#[test]
fn escape_clears_the_highlight() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert!(h.state().selected_index().is_some());

    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().selected_index(), None);
}

#[test]
fn confirm_on_a_missing_game_explains_itself_instead_of_failing_silently() {
    let _guard = isolated_config();
    let (mut h, _server) = library_harness();

    // Second fixture game has missing_from_fs set.
    h.key_press(egui::Key::ArrowDown);
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected_index(), Some(1));

    h.key_press(egui::Key::Enter);
    h.run();

    assert!(
        h.query_by_label_contains("missing on the server").is_some(),
        "pressing confirm on an unavailable game should say why"
    );
}

#[test]
fn no_controller_is_connected_in_a_headless_test_run() {
    let _guard = isolated_config();
    let (h, _server) = library_harness();
    // Guards the hint text: CI must show the keyboard hints, not pad hints.
    assert!(!h.state().gamepad_connected());
    assert!(h.query_by_label_contains("Enter download/play").is_some());
}
