//! Getting a playable file out of what RomM actually serves.
//!
//! RomM stores and serves plenty of games as `.zip`, and most libretro cores
//! cannot read one — they want the ROM itself. A frontend that hands a core a
//! zip gets a refusal at best, and at worst the core tries to interpret the zip
//! header as a cartridge.
//!
//! A few cores *can*: their `valid_extensions` include `zip`, and those are left
//! alone, because a core that handles its own archives usually does it better
//! (it can pick the right file out of a multi-disc set, for one).
//!
//! Nothing here decides *which* game is in an archive by guessing at names. The
//! rule is: pick the single entry whose extension the core accepts. If there is
//! more than one, say so rather than choosing — picking the wrong disc of a
//! two-disc game and silently starting it is worse than asking.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What to hand the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Use the downloaded file as-is.
    AsDownloaded,
    /// Extract this entry from the archive first.
    Extract(String),
}

/// Is this file an archive we should look inside?
pub fn looks_like_archive(path: &Path) -> bool {
    matches!(
        path.extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref(),
        Some("zip")
    )
}

/// Decide what to do, given the archive's entries and what the core accepts.
///
/// Pure, so the awkward cases are testable without an archive.
pub fn choose<'a>(
    entries: &'a [String],
    core_extensions: &[String],
    core_reads_zip: bool,
) -> Result<Content> {
    if core_reads_zip {
        // The core does archives itself, and does them better.
        return Ok(Content::AsDownloaded);
    }
    let accepts = |name: &str| {
        Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|e| core_extensions.contains(&e))
    };
    // Ignore macOS resource forks and directory entries, which otherwise look
    // like extra candidates and turn a clean single-ROM zip into "ambiguous".
    let candidates: Vec<&'a String> = entries
        .iter()
        .filter(|n| !n.ends_with('/') && !n.starts_with("__MACOSX/"))
        .filter(|n| accepts(n))
        .collect();

    match candidates.len() {
        0 => bail!(
            "the archive contains nothing this core can read (it wants {})",
            core_extensions.join(", ")
        ),
        1 => Ok(Content::Extract(candidates[0].clone())),
        // Deliberately not guessing. A multi-disc game needs disc swapping,
        // which RustRomM does not do yet, and silently launching disc 1 of 3
        // would look like it worked right up until the game asked for disc 2.
        n => bail!(
            "the archive contains {n} games and RustRomM can't yet choose between them: {}",
            candidates
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// List the file names inside a zip.
pub fn list_entries(archive: &Path) -> Result<Vec<String>> {
    let file = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .with_context(|| format!("{} is not a valid zip", archive.display()))?;
    Ok((0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect())
}

/// Extract one entry beside the archive and return its path.
///
/// Cached: an extracted ROM is reused rather than unpacked on every launch,
/// which matters because a Mega Drive ROM is a few megabytes and a PlayStation
/// image is several hundred.
pub fn extract(archive: &Path, entry: &str, dest_dir: &Path) -> Result<PathBuf> {
    // Take only the file name. An archive entry is attacker-controlled text and
    // "../../.ssh/authorized_keys" is a real archive format attack — zip slip.
    let name = Path::new(entry)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("archive entry has no usable file name: {entry}"))?;
    let dest = dest_dir.join(&name);
    if dest.is_file() {
        return Ok(dest);
    }

    std::fs::create_dir_all(dest_dir)?;
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut source = zip
        .by_name(entry)
        .with_context(|| format!("{entry} is not in {}", archive.display()))?;

    // Unpack to a temporary name and rename, so an interrupted extraction never
    // leaves a truncated ROM that would be loaded next time and look corrupt.
    let partial = dest.with_extension("part");
    let mut out = std::fs::File::create(&partial)?;
    std::io::copy(&mut source, &mut out)?;
    out.flush_and_sync()?;
    drop(out);
    std::fs::rename(&partial, &dest)?;
    Ok(dest)
}

/// Small helper so the extract path reads cleanly.
trait FlushAndSync {
    fn flush_and_sync(&mut self) -> std::io::Result<()>;
}

impl FlushAndSync for std::fs::File {
    fn flush_and_sync(&mut self) -> std::io::Result<()> {
        use std::io::Write;
        self.flush()?;
        self.sync_all()
    }
}

/// Read a file, or extract it from an archive first, ready for the core.
pub fn prepare(
    downloaded: &Path,
    core_extensions: &[String],
    cache_dir: &Path,
) -> Result<(PathBuf, Vec<u8>)> {
    let core_reads_zip = core_extensions.iter().any(|e| e == "zip");
    if !looks_like_archive(downloaded) || core_reads_zip {
        let bytes = std::fs::read(downloaded)
            .with_context(|| format!("could not read {}", downloaded.display()))?;
        return Ok((downloaded.to_path_buf(), bytes));
    }
    let entries = list_entries(downloaded)?;
    match choose(&entries, core_extensions, false)? {
        Content::AsDownloaded => {
            let bytes = std::fs::read(downloaded)?;
            Ok((downloaded.to_path_buf(), bytes))
        }
        Content::Extract(entry) => {
            let path = extract(downloaded, &entry, cache_dir)?;
            let bytes = std::fs::read(&path)?;
            crate::logging::info(format!(
                "extracted {entry} from {} ({} bytes)",
                downloaded.file_name().unwrap_or_default().to_string_lossy(),
                bytes.len()
            ));
            Ok((path, bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_plain_rom_is_left_alone() {
        assert!(!looks_like_archive(Path::new("game.gen")));
        assert!(!looks_like_archive(Path::new("game.smc")));
        assert!(looks_like_archive(Path::new("game.zip")));
        assert!(looks_like_archive(Path::new("GAME.ZIP")));
    }

    #[test]
    fn a_core_that_reads_archives_gets_the_archive() {
        // Cores listing `zip` do it better than we would — they can pick the
        // right file out of a multi-disc set.
        let got = choose(&names(&["a.gen", "b.gen"]), &exts(&["gen", "zip"]), true).unwrap();
        assert_eq!(got, Content::AsDownloaded);
    }

    #[test]
    fn the_single_matching_entry_is_chosen() {
        let got = choose(
            &names(&["Sonic.gen", "readme.txt", "cover.png"]),
            &exts(&["gen", "md", "bin"]),
            false,
        )
        .unwrap();
        assert_eq!(got, Content::Extract("Sonic.gen".into()));
    }

    #[test]
    fn resource_forks_and_directories_are_not_candidates() {
        // A zip made on macOS carries __MACOSX entries that mirror every file.
        // Counting them turns a clean single-ROM archive into "ambiguous" and
        // refuses to launch a game that is perfectly fine.
        let got = choose(
            &names(&["Sonic.gen", "__MACOSX/Sonic.gen", "roms/"]),
            &exts(&["gen"]),
            false,
        )
        .unwrap();
        assert_eq!(got, Content::Extract("Sonic.gen".into()));
    }

    #[test]
    fn an_archive_with_nothing_playable_says_what_was_wanted() {
        let err = choose(&names(&["readme.txt"]), &exts(&["gen", "md"]), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("gen"), "unhelpful error: {err}");
    }

    #[test]
    fn a_multi_disc_archive_refuses_rather_than_guessing() {
        // Launching disc 1 of 3 silently looks like it worked, right up until
        // the game asks for disc 2 and there is no way to give it one.
        let err = choose(
            &names(&["Game (Disc 1).bin", "Game (Disc 2).bin"]),
            &exts(&["bin"]),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("2 games"), "got: {err}");
        assert!(
            err.contains("Disc 1"),
            "the message should name them: {err}"
        );
    }

    #[test]
    fn extension_matching_ignores_case() {
        let got = choose(&names(&["GAME.GEN"]), &exts(&["gen"]), false).unwrap();
        assert_eq!(got, Content::Extract("GAME.GEN".into()));
    }

    #[test]
    fn extraction_cannot_write_outside_the_destination() {
        // Zip slip. Archive entry names are attacker-controlled, and
        // "../../../.ssh/authorized_keys" is the classic. Only the file name is
        // ever used.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file::<_, ()>("../../escaped.gen", Default::default())
                .unwrap();
            use std::io::Write;
            z.write_all(b"payload").unwrap();
            z.finish().unwrap();
        }
        let out = dir.path().join("cache");
        let got = extract(&archive, "../../escaped.gen", &out).unwrap();
        assert!(
            got.starts_with(&out),
            "extraction escaped the destination: {}",
            got.display()
        );
        assert_eq!(got.file_name().unwrap(), "escaped.gen");
        assert!(!dir.path().parent().unwrap().join("escaped.gen").exists());
    }

    #[test]
    fn a_zipped_rom_is_extracted_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Sonic.zip");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file::<_, ()>("Sonic.gen", Default::default())
                .unwrap();
            use std::io::Write;
            z.write_all(b"MEGA DRIVE ROM DATA").unwrap();
            z.finish().unwrap();
        }
        let cache = dir.path().join("cache");
        let (path, bytes) = prepare(&archive, &exts(&["gen", "md"]), &cache).unwrap();
        assert_eq!(bytes, b"MEGA DRIVE ROM DATA");
        assert_eq!(path.file_name().unwrap(), "Sonic.gen");

        // Second time must reuse rather than unpack again — a PlayStation image
        // is hundreds of megabytes.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let (again, _) = prepare(&archive, &exts(&["gen"]), &cache).unwrap();
        assert_eq!(again, path);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
    }

    #[test]
    fn an_unzipped_rom_passes_straight_through() {
        let dir = tempfile::tempdir().unwrap();
        let rom = dir.path().join("game.gen");
        std::fs::write(&rom, b"raw").unwrap();
        let (path, bytes) = prepare(&rom, &exts(&["gen"]), &dir.path().join("cache")).unwrap();
        assert_eq!(path, rom);
        assert_eq!(bytes, b"raw");
    }

    #[test]
    fn extraction_leaves_no_partial_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("g.zip");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file::<_, ()>("g.nes", Default::default()).unwrap();
            use std::io::Write;
            z.write_all(b"nes").unwrap();
            z.finish().unwrap();
        }
        let cache = dir.path().join("cache");
        extract(&archive, "g.nes", &cache).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&cache)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
