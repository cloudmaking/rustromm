//! API-layer integration tests against a mock RomM server.
//!
//! These run entirely offline — `httpmock` binds a real HTTP server on
//! localhost, so the full reqwest stack (auth headers, query strings, streaming
//! downloads) is exercised without needing a RomM instance. That means CI can
//! verify behaviour on all three platforms.
//!
//! The JSON fixtures mirror the shapes returned by RomM 5.1.0, checked against
//! a live server while writing them.

use std::sync::atomic::AtomicBool;

use httpmock::prelude::*;
use rustromm::api::Api;
use rustromm::models::Rom;

/// base64("ali:hunter2") — the Authorization header value we expect to be sent.
const EXPECTED_AUTH: &str = "Basic YWxpOmh1bnRlcjI=";

fn api_for(server: &MockServer) -> Api {
    Api::new(&server.base_url(), "ali", "hunter2").expect("client builds")
}

fn platforms_json() -> serde_json::Value {
    serde_json::json!([
        { "id": 34, "name": "Nintendo Entertainment System", "slug": "nes",
          "fs_slug": "nes", "rom_count": 1045, "custom_name": null },
        { "id": 52, "name": "Super Nintendo Entertainment System", "slug": "snes",
          "fs_slug": "snes", "rom_count": 845, "custom_name": "Super Nintendo" }
    ])
}

fn roms_page_json() -> serde_json::Value {
    serde_json::json!({
        "total": 5736,
        "items": [
            {
                "id": 3680,
                "name": "2-in-1 Cosmo Cop + Cyber Monster",
                "fs_name": "2-in-1 Cosmo Cop + Cyber Monster (Sachen) [U][!].nes",
                "fs_extension": "nes",
                "fs_size_bytes": 131198,
                "platform_id": 34,
                "platform_slug": "nes",
                "platform_display_name": "Nintendo Entertainment System",
                "summary": null,
                "path_cover_small": "",
                "path_cover_large": null,
                "has_multiple_files": false,
                "files": [],
                "missing_from_fs": false
            },
            {
                "id": 4001,
                "name": null,
                "fs_name": "Some Unidentified Game.smc",
                "fs_extension": "smc",
                "fs_size_bytes": 524288,
                "platform_id": 52,
                "platform_slug": "snes",
                "platform_display_name": "Super Nintendo",
                "path_cover_small": "assets/romm/resources/covers/4001/small.png",
                "has_multiple_files": true,
                "files": [
                    { "id": 1, "file_name": "disc1.bin", "file_size_bytes": 262144 },
                    { "id": 2, "file_name": "disc2.bin", "file_size_bytes": 262144 }
                ],
                "missing_from_fs": false
            }
        ]
    })
}

// ---------------------------------------------------------------- connection

#[test]
fn check_connection_reports_server_version() {
    let server = MockServer::start();
    let hb = server.mock(|when, then| {
        when.method(GET).path("/api/heartbeat");
        then.status(200)
            .json_body(serde_json::json!({ "SYSTEM": { "VERSION": "5.1.0" } }));
    });
    let pf = server.mock(|when, then| {
        when.method(GET)
            .path("/api/platforms")
            .header("authorization", EXPECTED_AUTH);
        then.status(200).json_body(platforms_json());
    });

    let version = api_for(&server).check_connection().expect("connects");

    assert_eq!(version, "5.1.0");
    hb.assert();
    pf.assert();
}

#[test]
fn wrong_password_is_reported_as_a_credential_problem() {
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

    let err = api_for(&server).check_connection().unwrap_err().to_string();

    // The distinction matters: the user needs to know their password is wrong,
    // not that the server is unreachable.
    assert!(
        err.contains("username or password"),
        "expected a credentials message, got: {err}"
    );
}

#[test]
fn a_non_romm_server_is_reported_as_such() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/heartbeat");
        then.status(404);
    });

    let err = api_for(&server).check_connection().unwrap_err().to_string();
    assert!(
        err.contains("RomM server"),
        "expected a 'not a RomM server' hint, got: {err}"
    );
}

#[test]
fn unreachable_server_errors_rather_than_hanging() {
    // Port 1 is reserved and nothing listens there.
    let api = Api::new("http://127.0.0.1:1", "ali", "hunter2").unwrap();
    assert!(api.check_connection().is_err());
}

// ----------------------------------------------------------------- platforms

#[test]
fn platforms_parse_and_expose_custom_names() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/platforms");
        then.status(200).json_body(platforms_json());
    });

    let platforms = api_for(&server).platforms().expect("parses");

    assert_eq!(platforms.len(), 2);
    assert_eq!(platforms[0].display_name(), "Nintendo Entertainment System");
    // custom_name overrides name when present.
    assert_eq!(platforms[1].display_name(), "Super Nintendo");
    assert_eq!(platforms[1].rom_count, 845);
}

// ---------------------------------------------------------------------- roms

#[test]
fn roms_sends_pagination_and_parses_the_page() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/api/roms")
            .query_param("limit", "100")
            .query_param("offset", "200")
            .query_param("order_by", "name")
            .query_param("order_dir", "asc");
        then.status(200).json_body(roms_page_json());
    });

    let page = api_for(&server).roms(None, "", 200).expect("parses");

    m.assert();
    assert_eq!(page.total, 5736);
    assert_eq!(page.items.len(), 2);
}

#[test]
fn platform_filter_and_search_are_passed_through() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/api/roms")
            .query_param("platform_ids", "34")
            .query_param("search_term", "mario");
        then.status(200).json_body(roms_page_json());
    });

    api_for(&server).roms(Some(34), "  mario  ", 0).unwrap();

    // Also proves the search term is trimmed before being sent.
    m.assert();
}

#[test]
fn blank_search_is_omitted_entirely() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/api/roms")
            .is_true(|req| !req.query_params().iter().any(|(k, _)| k == "search_term"));
        then.status(200).json_body(roms_page_json());
    });

    api_for(&server).roms(None, "   ", 0).unwrap();
    m.assert();
}

#[test]
fn unidentified_roms_fall_back_to_the_file_name() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms");
        then.status(200).json_body(roms_page_json());
    });

    let page = api_for(&server).roms(None, "", 0).unwrap();

    assert_eq!(page.items[0].title(), "2-in-1 Cosmo Cop + Cyber Monster");
    // name == null, so the file name is used instead of showing nothing.
    assert_eq!(page.items[1].title(), "Some Unidentified Game.smc");
}

#[test]
fn empty_cover_path_is_treated_as_absent() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms");
        then.status(200).json_body(roms_page_json());
    });

    let page = api_for(&server).roms(None, "", 0).unwrap();

    // RomM sends "" rather than null for missing art; both must mean "no cover".
    assert_eq!(page.items[0].cover_path(), None);
    assert_eq!(
        page.items[1].cover_path(),
        Some("assets/romm/resources/covers/4001/small.png")
    );
}

#[test]
fn server_errors_surface_rather_than_being_swallowed() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms");
        then.status(500);
    });

    assert!(api_for(&server).roms(None, "", 0).is_err());
}

// ------------------------------------------------------------------- covers

#[test]
fn missing_cover_art_is_none_not_an_error() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/assets/nope.png");
        then.status(404);
    });

    // Cover art is cosmetic; a 404 must not become a user-visible failure.
    assert!(api_for(&server).cover("assets/nope.png").is_none());
}

#[test]
fn cover_art_is_fetched_with_auth() {
    let server = MockServer::start();
    let m = server.mock(|when, then| {
        when.method(GET)
            .path("/assets/cover.png")
            .header("authorization", EXPECTED_AUTH);
        then.status(200).body(vec![1u8, 2, 3, 4]);
    });

    let bytes = api_for(&server).cover("/assets/cover.png").expect("some");

    m.assert();
    assert_eq!(bytes, vec![1, 2, 3, 4]);
}

// ---------------------------------------------------------------- downloads

fn rom_fixture(name: &str, multi: bool) -> Rom {
    let json = serde_json::json!({
        "id": 42,
        "name": "Test Game",
        "fs_name": name,
        "fs_extension": "nes",
        "fs_size_bytes": 8,
        "platform_id": 34,
        "platform_slug": "nes",
        "platform_display_name": "NES",
        "has_multiple_files": multi,
        "files": [],
        "missing_from_fs": false
    });
    serde_json::from_value(json).unwrap()
}

#[test]
fn download_writes_the_file_and_reports_progress() {
    let server = MockServer::start();
    let payload = b"ROMBYTES".to_vec();
    server.mock(|when, then| {
        when.method(GET)
            .path("/api/roms/42/content/game.nes")
            .header("authorization", EXPECTED_AUTH);
        then.status(200).body(payload.clone());
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("game.nes");
    let rom = rom_fixture("game.nes", false);
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();

    api_for(&server)
        .download_rom(&rom, &dest, &cancel, |done, total| seen.push((done, total)))
        .expect("downloads");

    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(!seen.is_empty(), "progress callback should fire");
    assert_eq!(seen.last().unwrap().0, payload.len() as u64);
    // The temporary file must not be left behind.
    assert!(!dest.with_extension("part").exists());
}

#[test]
fn download_creates_missing_parent_directories() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms/42/content/game.nes");
        then.status(200).body(b"X");
    });

    let dir = tempfile::tempdir().unwrap();
    // Nested path that does not exist yet — mirrors the per-platform subfolder.
    let dest = dir.path().join("nes").join("game.nes");
    let cancel = AtomicBool::new(false);

    api_for(&server)
        .download_rom(&rom_fixture("game.nes", false), &dest, &cancel, |_, _| {})
        .expect("downloads");

    assert!(dest.exists());
}

#[test]
fn cancelled_download_leaves_no_partial_file() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms/42/content/game.nes");
        then.status(200).body(vec![0u8; 4 * 1024 * 1024]);
    });

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("game.nes");
    // Pre-set: the very first loop iteration aborts.
    let cancel = AtomicBool::new(true);

    let err = api_for(&server)
        .download_rom(&rom_fixture("game.nes", false), &dest, &cancel, |_, _| {})
        .unwrap_err()
        .to_string();

    assert!(err.contains("cancelled"), "got: {err}");
    assert!(!dest.exists(), "no truncated ROM should survive");
    assert!(
        !dest.with_extension("part").exists(),
        "no .part left behind"
    );
}

#[test]
fn download_of_a_file_missing_on_the_server_fails_before_any_request() {
    let server = MockServer::start();
    // Deliberately no mock registered: if a request were made it would 404 and
    // we'd get a different error message than the one asserted below.
    let mut rom = rom_fixture("game.nes", false);
    rom.missing_from_fs = true;

    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let err = api_for(&server)
        .download_rom(&rom, &dir.path().join("game.nes"), &cancel, |_, _| {})
        .unwrap_err()
        .to_string();

    assert!(err.contains("missing from the server"), "got: {err}");
}

#[test]
fn a_404_from_the_server_is_reported() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/api/roms/42/content/game.nes");
        then.status(404);
    });

    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    assert!(
        api_for(&server)
            .download_rom(
                &rom_fixture("game.nes", false),
                &dir.path().join("game.nes"),
                &cancel,
                |_, _| {}
            )
            .is_err()
    );
}

#[test]
fn file_names_with_spaces_and_brackets_are_url_encoded() {
    let server = MockServer::start();
    let name = "Zelda II - The Adventure of Link (USA) [!].nes";
    // Verified against a live RomM 5.1.0 server: the fully percent-encoded form
    // returns 200 with the exact file size, while leaving `()[]!` unencoded
    // produces a URL that is rejected as malformed before it is even sent.
    // Square brackets in particular are not legal in a URL path.
    let encoded = "Zelda%20II%20-%20The%20Adventure%20of%20Link%20%28USA%29%20%5B%21%5D.nes";
    let m = server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/roms/42/content/{encoded}"));
        then.status(200).body(b"OK");
    });

    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    api_for(&server)
        .download_rom(
            &rom_fixture(name, false),
            &dir.path().join("out.nes"),
            &cancel,
            |_, _| {},
        )
        .expect("downloads");

    m.assert();
}

#[test]
fn multi_file_roms_are_saved_as_zip() {
    // The content endpoint returns a zip for multi-file entries regardless of
    // what fs_name says, so the saved name has to change to match.
    let rom = rom_fixture("Final Fantasy VII (Disc 1).nes", true);
    assert_eq!(rom.download_file_name(), "Final Fantasy VII (Disc 1).zip");

    let single = rom_fixture("Sonic.nes", false);
    assert_eq!(single.download_file_name(), "Sonic.nes");
}
