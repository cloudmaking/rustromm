//! Which core plays what, and how to get it.
//!
//! RustRomM ships **no core binaries**. They are fetched from the libretro
//! buildbot on first use and cached.
//!
//! That is a licence decision as much as a size one. Several of the best cores
//! are not free software — Snes9x is "freeware for personal use only" and
//! Genesis Plus GX carries a "may not be sold, nor used in a commercial
//! product" clause across 129 of its source files, despite a root `LICENSE.txt`
//! that says LGPL and covers only bundled sub-components. Others are GPL-2.0
//! **only**, which RustRomM can never match: `winit` and `glutin` are
//! Apache-2.0-only and unavoidable under `eframe`, so this app cannot be
//! relicensed down to GPLv2.
//!
//! Downloading rather than bundling sidesteps all of it, and it is the
//! established position in this ecosystem: RetroArch is GPL-3.0 and has offered
//! exactly these cores through its Online Updater for over a decade. Argosy,
//! the Android client this project follows, does the same.
//!
//! **Do not bundle a core to save a download.** It would be a licence violation
//! for at least three of the entries below.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// A core, and what it needs to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreSpec {
    /// Buildbot name, without the `_libretro` suffix.
    pub name: &'static str,
    /// Human name for the UI.
    pub display: &'static str,
    /// Files that must exist in the system directory before `retro_load_game`
    /// is called.
    ///
    /// Checked beforehand, not after, because a missing BIOS is not always a
    /// clean refusal — Handy **segfaults** inside `retro_load_game` without
    /// `lynxboot.img`, and by the time the core would have told us, the process
    /// is gone. See `docs/libretro-spike.md`.
    pub required_bios: &'static [&'static str],
    /// Shown wherever the core is named, so a non-free licence is never a
    /// surprise.
    pub licence: &'static str,
}

macro_rules! core {
    ($name:literal, $display:literal, $licence:literal $(, bios: [$($b:literal),*])?) => {
        CoreSpec {
            name: $name,
            display: $display,
            required_bios: &[$($($b),*)?],
            licence: $licence,
        }
    };
}

pub const FCEUMM: CoreSpec = core!("fceumm", "FCEUmm", "GPL-2.0-or-later");
pub const SNES9X: CoreSpec = core!("snes9x", "Snes9x", "non-commercial, personal use only");
pub const MGBA: CoreSpec = core!("mgba", "mGBA", "MPL-2.0");
pub const GENESIS_PLUS_GX: CoreSpec = core!(
    "genesis_plus_gx",
    "Genesis Plus GX",
    "non-commercial (see core source, not its LICENSE.txt)"
);
pub const STELLA: CoreSpec = core!("stella", "Stella", "GPL-2.0-or-later");
pub const PROSYSTEM: CoreSpec = core!("prosystem", "ProSystem", "GPL-2.0-or-later");
pub const MEDNAFEN_PCE: CoreSpec =
    core!("mednafen_pce_fast", "Beetle PCE Fast", "GPL-2.0-or-later");
pub const MEDNAFEN_WSWAN: CoreSpec =
    core!("mednafen_wswan", "Beetle WonderSwan", "GPL-2.0-or-later");
pub const MEDNAFEN_NGP: CoreSpec = core!("mednafen_ngp", "Beetle NeoPop", "GPL-2.0-or-later");
pub const MEDNAFEN_VB: CoreSpec = core!("mednafen_vb", "Beetle VB", "GPL-2.0-or-later");
pub const HANDY: CoreSpec = core!(
    "handy",
    "Handy",
    "Zlib",
    bios: ["lynxboot.img"]
);
pub const ATARI800: CoreSpec = core!("atari800", "Atari800", "GPL-2.0-or-later");
pub const GEARCOLECO: CoreSpec =
    core!("gearcoleco", "Gearcoleco", "GPL-3.0-or-later", bios: ["colecovision.rom"]);
pub const PICODRIVE: CoreSpec = core!("picodrive", "PicoDrive", "non-commercial (MAME-derived)");

/// Why a platform has no embedded core. Shown to the user verbatim, so it must
/// read as an explanation rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCore {
    /// Emulating it needs a hardware render context, which RustRomM
    /// deliberately never provides.
    NeedsGpu(&'static str),
    /// No suitable software core, or not worth the support burden.
    Unsupported(&'static str),
}

impl NoCore {
    pub fn message(&self) -> &'static str {
        match self {
            Self::NeedsGpu(m) | Self::Unsupported(m) => m,
        }
    }
}

/// What plays a RomM platform slug.
///
/// Slugs come from RomM, which uses IGDB's naming, and the same system appears
/// under several of them across server versions. A missing alias means a
/// platform silently has no games playable, which is far worse than an
/// unrecognised one — so aliases are listed generously.
pub fn core_for_platform(slug: &str) -> Result<CoreSpec, NoCore> {
    let s = slug.trim().to_ascii_lowercase();
    match s.as_str() {
        "nes" | "famicom" | "family-computer" | "nintendo-entertainment-system" | "fds" => {
            Ok(FCEUMM)
        }
        "snes"
        | "sfc"
        | "super-nintendo-entertainment-system"
        | "super-famicom"
        | "snes-slash-super-famicom" => Ok(SNES9X),
        "gb" | "game-boy" | "gbc" | "game-boy-color" | "gba" | "game-boy-advance" => Ok(MGBA),
        "genesis"
        | "megadrive"
        | "mega-drive"
        | "genesis-slash-megadrive"
        | "sega-mega-drive-genesis"
        | "sms"
        | "sega-master-system"
        | "sega-master-system-slash-mark-iii"
        | "gamegear"
        | "game-gear"
        | "gg"
        | "sg1000" => Ok(GENESIS_PLUS_GX),
        "sega32" | "sega32x" | "sega-32x" | "32x" => Ok(PICODRIVE),
        "atari2600" | "atari-2600" => Ok(STELLA),
        "atari7800" | "atari-7800" => Ok(PROSYSTEM),
        "atari5200" | "atari-5200" | "atari8bit" | "atari-8-bit" => Ok(ATARI800),
        "colecovision" => Ok(GEARCOLECO),
        "lynx" | "atari-lynx" => Ok(HANDY),
        "turbografx16--1" | "turbografx-16-slash-pc-engine" | "pc-engine" | "pce" | "tg16" => {
            Ok(MEDNAFEN_PCE)
        }
        "wonderswan" | "wonderswan-color" | "ws" => Ok(MEDNAFEN_WSWAN),
        "ngp" | "neo-geo-pocket" | "neo-geo-pocket-color" => Ok(MEDNAFEN_NGP),
        "virtualboy" | "virtual-boy" | "vb" => Ok(MEDNAFEN_VB),

        // Deliberately unsupported, each with a reason a player can act on.
        "psp" | "playstation-portable" => Err(NoCore::NeedsGpu(
            "PSP emulation needs a graphics core RustRomM doesn't embed. Use PPSSPP.",
        )),
        "n64" | "nintendo-64" => Err(NoCore::NeedsGpu(
            "Nintendo 64 emulation needs a graphics core RustRomM doesn't embed.",
        )),
        "ngc" | "gamecube" | "wii" => Err(NoCore::NeedsGpu(
            "GameCube and Wii emulation needs a graphics core RustRomM doesn't embed. Use Dolphin.",
        )),
        "jaguar" | "atari-jaguar" => Err(NoCore::Unsupported(
            "Atari Jaguar emulation is not reliable enough to embed.",
        )),
        "mame" | "arcade" | "fbneo" => Err(NoCore::Unsupported(
            "Arcade games need per-machine ROM sets and BIOS files that RustRomM can't set up for you.",
        )),
        "scummvm" => Err(NoCore::Unsupported(
            "ScummVM games are folders of assets rather than ROMs. Use ScummVM.",
        )),
        "ports" | "nul" | "" => Err(NoCore::Unsupported("This isn't an emulated platform.")),
        _ => Err(NoCore::Unsupported(
            "No embedded core is mapped to this platform yet.",
        )),
    }
}

// ─── Acquisition ─────────────────────────────────────────────────────────────

/// The buildbot directory for the running platform, or `None` where none
/// exists.
///
/// `None` means exactly one thing today: **arm64 Windows**. The buildbot
/// publishes `windows/x86` and `windows/x86_64` and nothing else, so a native
/// ARM64 build has no core it can load. The answer is to ship the x86_64 binary
/// to Windows on ARM and let the OS emulate it — verified working in CI, see
/// `docs/libretro-spike.md`.
pub const fn buildbot_dir() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux/x86_64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("linux/aarch64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("apple/osx/x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("apple/osx/arm64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows/x86_64")
    } else {
        None
    }
}

/// Shared-library extension for the running platform.
pub const fn lib_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// Where a core's zip lives on the buildbot.
pub fn download_url(dir: &str, core: &str, ext: &str) -> String {
    format!("https://buildbot.libretro.com/nightly/{dir}/latest/{core}_libretro.{ext}.zip")
}

/// The file name inside that zip, which is also the cached name on disk.
pub fn library_file_name(core: &str, ext: &str) -> String {
    format!("{core}_libretro.{ext}")
}

/// Is this core already downloaded?
pub fn cached_path(cores_dir: &Path, core: &str) -> Option<PathBuf> {
    let p = cores_dir.join(library_file_name(core, lib_extension()));
    p.is_file().then_some(p)
}

/// Which required BIOS files are missing from the system directory.
///
/// Must be run before `retro_load_game`. A missing BIOS is not reliably a clean
/// refusal — Handy segfaults on one — so this is the only chance to fail
/// safely.
pub fn missing_bios(spec: &CoreSpec, system_dir: &Path) -> Vec<&'static str> {
    spec.required_bios
        .iter()
        .filter(|f| !system_dir.join(f).is_file())
        .copied()
        .collect()
}

/// Fetch a core and unpack it into `cores_dir`, returning the library path.
///
/// Returns the cached copy without touching the network when one exists.
pub fn ensure_core(
    client: &reqwest::blocking::Client,
    cores_dir: &Path,
    core: &str,
) -> Result<PathBuf> {
    if let Some(p) = cached_path(cores_dir, core) {
        return Ok(p);
    }
    let Some(dir) = buildbot_dir() else {
        bail!(
            "no libretro cores are published for {} on {}. On Windows for ARM, \
             download the x86_64 build of RustRomM instead — it runs under \
             emulation and can load cores.",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
    };
    let ext = lib_extension();
    let url = download_url(dir, core, ext);
    let name = library_file_name(core, ext);

    crate::logging::info(format!("downloading core {core} from {url}"));
    let response = client
        .get(&url)
        .send()
        .with_context(|| format!("could not reach the libretro buildbot for {core}"))?;
    if !response.status().is_success() {
        bail!(
            "the libretro buildbot has no {core} for this platform ({})",
            response.status()
        );
    }
    let zipped = response.bytes()?;

    std::fs::create_dir_all(cores_dir)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zipped))
        .with_context(|| format!("{core} did not download as a valid zip"))?;
    let mut entry = archive
        .by_name(&name)
        .with_context(|| format!("the {core} download did not contain {name}"))?;

    // Unpack to a temporary name and rename, so an interrupted download can
    // never leave a truncated library that would be loaded on the next run.
    let final_path = cores_dir.join(&name);
    let partial = cores_dir.join(format!("{name}.part"));
    let mut out = std::fs::File::create(&partial)?;
    std::io::copy(&mut entry, &mut out)?;
    drop(out);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&partial, &final_path)?;
    crate::logging::info(format!("core {core} ready at {}", final_path.display()));
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platforms_in_a_real_library_all_resolve() {
        // Taken from an actual 5,736-game RomM library. A slug that silently
        // fails to map means a whole console has no playable games, which is
        // the worst possible way for this to break.
        let playable = [
            "atari2600",
            "atari5200",
            "atari7800",
            "colecovision",
            "gamegear",
            "gb",
            "gbc",
            "genesis",
            "lynx",
            "nes",
            "sega32",
            "sms",
            "snes",
        ];
        for slug in playable {
            assert!(
                core_for_platform(slug).is_ok(),
                "{slug} has no core and it is in a real library"
            );
        }

        // These are expected to have none, but must explain themselves rather
        // than falling through to a generic error.
        for slug in ["psp", "mame", "scummvm", "jaguar", "ports"] {
            let err = core_for_platform(slug).unwrap_err();
            assert!(
                err.message().len() > 20,
                "{slug} needs a real explanation, got: {}",
                err.message()
            );
        }
    }

    #[test]
    fn slugs_match_regardless_of_case_or_surrounding_space() {
        // RomM's slugs vary across versions and metadata sources.
        assert_eq!(core_for_platform("NES"), Ok(FCEUMM));
        assert_eq!(core_for_platform("  gb  "), Ok(MGBA));
        assert_eq!(core_for_platform("Game-Boy-Color"), Ok(MGBA));
    }

    #[test]
    fn one_core_covers_the_whole_sega_8_and_16_bit_family() {
        // Genesis Plus GX handles all of these, so mapping them anywhere else
        // would mean downloading cores we do not need.
        for slug in ["genesis", "megadrive", "sms", "gamegear", "sg1000"] {
            assert_eq!(core_for_platform(slug), Ok(GENESIS_PLUS_GX), "{slug}");
        }
        // ...and mGBA covers all three Game Boy generations.
        for slug in ["gb", "gbc", "gba"] {
            assert_eq!(core_for_platform(slug), Ok(MGBA), "{slug}");
        }
    }

    #[test]
    fn unknown_platforms_get_an_explanation_rather_than_a_panic() {
        let err = core_for_platform("some-future-console").unwrap_err();
        assert!(matches!(err, NoCore::Unsupported(_)));
        assert!(!err.message().is_empty());
    }

    #[test]
    fn gpu_only_platforms_are_distinguished_from_merely_unsupported_ones() {
        // The distinction is real: "needs a GPU core" is a permanent
        // consequence of refusing SET_HW_RENDER, while "unsupported" might
        // change. The UI should be able to say which.
        assert!(matches!(core_for_platform("psp"), Err(NoCore::NeedsGpu(_))));
        assert!(matches!(core_for_platform("n64"), Err(NoCore::NeedsGpu(_))));
        assert!(matches!(
            core_for_platform("mame"),
            Err(NoCore::Unsupported(_))
        ));
    }

    #[test]
    fn download_urls_match_the_buildbot_layout() {
        // Verified against the live buildbot: these exact paths return 200.
        assert_eq!(
            download_url("linux/x86_64", "gambatte", "so"),
            "https://buildbot.libretro.com/nightly/linux/x86_64/latest/gambatte_libretro.so.zip"
        );
        assert_eq!(
            download_url("apple/osx/arm64", "mgba", "dylib"),
            "https://buildbot.libretro.com/nightly/apple/osx/arm64/latest/mgba_libretro.dylib.zip"
        );
        assert_eq!(
            download_url("windows/x86_64", "snes9x", "dll"),
            "https://buildbot.libretro.com/nightly/windows/x86_64/latest/snes9x_libretro.dll.zip"
        );
    }

    #[test]
    fn this_platform_has_a_buildbot_directory_unless_it_is_windows_arm() {
        // Windows on ARM is the only target with no cores, and it is expected
        // to run the x86_64 build instead.
        let expected_none = cfg!(all(target_os = "windows", target_arch = "aarch64"));
        assert_eq!(buildbot_dir().is_none(), expected_none);
    }

    #[test]
    fn a_missing_bios_is_detected_before_the_core_is_ever_called() {
        // Handy segfaults inside retro_load_game without this file. Detecting
        // it here is the difference between a message and a crash.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(missing_bios(&HANDY, dir.path()), vec!["lynxboot.img"]);

        std::fs::write(dir.path().join("lynxboot.img"), b"x").unwrap();
        assert!(missing_bios(&HANDY, dir.path()).is_empty());

        // Cores that need nothing never report a missing file.
        assert!(missing_bios(&MGBA, dir.path()).is_empty());
        assert!(missing_bios(&STELLA, dir.path()).is_empty());
    }

    #[test]
    fn non_free_cores_say_so() {
        // The licence string is shown wherever a core is named. These two are
        // not free software and a user should never find that out elsewhere.
        assert!(SNES9X.licence.contains("non-commercial"));
        assert!(GENESIS_PLUS_GX.licence.contains("non-commercial"));
        assert!(MGBA.licence.contains("MPL"));
    }

    #[test]
    fn a_cached_core_is_found_and_a_partial_download_is_not() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cached_path(dir.path(), "mgba").is_none());

        // A `.part` file must not count — it is what an interrupted download
        // leaves behind, and loading it would crash.
        std::fs::write(
            dir.path().join(format!(
                "{}.part",
                library_file_name("mgba", lib_extension())
            )),
            b"partial",
        )
        .unwrap();
        assert!(cached_path(dir.path(), "mgba").is_none());

        std::fs::write(
            dir.path().join(library_file_name("mgba", lib_extension())),
            b"x",
        )
        .unwrap();
        assert!(cached_path(dir.path(), "mgba").is_some());
    }
}
