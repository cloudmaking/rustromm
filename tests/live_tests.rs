//! Optional tests against a real RomM server.
//!
//! Skipped unless all three environment variables are set, so CI and a plain
//! `cargo test` stay green without a server:
//!
//! ```sh
//! RUSTROMM_LIVE_URL=http://192.168.1.10:8087 \
//! RUSTROMM_LIVE_USER=me \
//! RUSTROMM_LIVE_PASS=secret \
//! cargo test --test live_tests -- --nocapture
//! ```
//!
//! These are the tests that catch a RomM release changing its API shape —
//! something the mock-server tests cannot see by construction.

use std::sync::atomic::AtomicBool;

use rustromm::api::Api;

/// Returns `None` (and prints why) when the live server details are absent.
fn live_api() -> Option<Api> {
    let url = std::env::var("RUSTROMM_LIVE_URL").ok()?;
    let user = std::env::var("RUSTROMM_LIVE_USER").ok()?;
    let pass = std::env::var("RUSTROMM_LIVE_PASS").ok()?;
    Some(Api::new(&url, &user, &pass).expect("valid server details"))
}

macro_rules! live {
    ($api:ident) => {
        let Some($api) = live_api() else {
            eprintln!("skipping: set RUSTROMM_LIVE_URL / _USER / _PASS to run live tests");
            return;
        };
    };
}

#[test]
fn connects_and_reports_a_version() {
    live!(api);
    let version = api.check_connection().expect("connects to the live server");
    println!("connected to RomM {version}");
    assert!(!version.is_empty());
}

#[test]
fn lists_platforms_with_games() {
    live!(api);
    let platforms = api.platforms().expect("lists platforms");
    println!("{} platforms", platforms.len());
    assert!(
        !platforms.is_empty(),
        "a real library should have platforms"
    );
    for p in platforms.iter().take(5) {
        println!("  {} ({} games)", p.display_name(), p.rom_count);
    }
}

#[test]
fn first_page_of_the_library_deserialises() {
    live!(api);
    let page = api.roms(None, "", 0).expect("fetches the first page");
    println!(
        "{} games total, {} on this page",
        page.total,
        page.items.len()
    );
    assert!(page.total > 0);
    assert!(!page.items.is_empty());

    // Every row must produce a usable title and download name — this is what
    // would break if RomM renamed or nulled a field we depend on.
    for rom in &page.items {
        assert!(
            !rom.title().is_empty(),
            "rom {} has no usable title",
            rom.id
        );
        assert!(!rom.download_file_name().is_empty());
    }
}

#[test]
fn pagination_returns_different_games() {
    live!(api);
    let first = api.roms(None, "", 0).expect("page 1");
    if first.total <= 100 {
        eprintln!("skipping: library too small to paginate");
        return;
    }
    let second = api.roms(None, "", 100).expect("page 2");
    assert_ne!(
        first.items.first().map(|r| r.id),
        second.items.first().map(|r| r.id),
        "offset should move the window"
    );
}

#[test]
fn searching_narrows_the_results() {
    live!(api);
    let all = api.roms(None, "", 0).expect("unfiltered");
    let hits = api.roms(None, "mario", 0).expect("search");
    println!("search 'mario': {} of {}", hits.total, all.total);
    assert!(hits.total <= all.total, "a search cannot widen the library");
}

#[test]
fn downloads_a_real_rom_and_the_size_matches() {
    live!(api);
    let page = api.roms(None, "", 0).expect("page");
    // Pick the smallest present file so the test stays quick.
    let Some(rom) = page
        .items
        .iter()
        .filter(|r| !r.missing_from_fs && !r.has_multiple_files && r.fs_size_bytes > 0)
        .min_by_key(|r| r.fs_size_bytes)
    else {
        eprintln!("skipping: no single-file ROM available to download");
        return;
    };

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(rom.download_file_name());
    let cancel = AtomicBool::new(false);

    api.download_rom(rom, &dest, &cancel, |_, _| {})
        .expect("downloads");

    let written = std::fs::metadata(&dest).expect("file exists").len();
    println!("downloaded {} ({written} bytes)", rom.title());
    assert_eq!(
        written, rom.fs_size_bytes as u64,
        "downloaded size should match the size RomM reported"
    );
}

#[test]
fn cover_art_downloads_when_present() {
    live!(api);
    let page = api.roms(None, "", 0).expect("page");
    let Some(rom) = page.items.iter().find(|r| r.cover_path().is_some()) else {
        eprintln!("skipping: no cover art in the first page");
        return;
    };
    let bytes = api
        .cover(rom.cover_path().unwrap())
        .expect("cover downloads");
    assert!(!bytes.is_empty());
    println!("cover for {} is {} bytes", rom.title(), bytes.len());
}

#[test]
fn a_wrong_password_is_rejected() {
    let Ok(url) = std::env::var("RUSTROMM_LIVE_URL") else {
        eprintln!("skipping: RUSTROMM_LIVE_URL not set");
        return;
    };
    let api = Api::new(&url, "definitely-not-a-user", "definitely-not-a-password").unwrap();
    let err = api.check_connection().unwrap_err().to_string();
    assert!(
        err.contains("username or password"),
        "expected rejection, got: {err}"
    );
}
