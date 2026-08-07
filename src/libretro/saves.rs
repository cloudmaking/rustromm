//! Persisting battery saves.
//!
//! Cores never write SRAM out themselves. The frontend reads it out of the
//! core's memory and puts it on disk, and if it does not, the player loses
//! everything the moment the app closes.
//!
//! # The only rule that really matters
//!
//! **Never destroy a save that was good.** A forty-hour file is unrecoverable
//! and irreplaceable, and every failure mode here is worse than simply not
//! having the feature. That principle drives three decisions:
//!
//! - Writes are atomic. Write a temporary file, fsync, rename. A crash or a
//!   power cut mid-write then leaves either the old save or the new one, never
//!   half of each. `Core::drop` cannot be relied on either — a core segfault
//!   does not unwind, so no destructor runs.
//! - Saves are flushed on a timer, not only at exit, for the same reason.
//! - A uniform buffer is never allowed to overwrite a non-uniform one. A core
//!   that failed to initialise its memory presents all-`0x00` or all-`0xFF`
//!   SRAM, and writing that over a real save is how you silently delete
//!   somebody's game.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Frames to run before the core's reported save size can be believed.
///
/// Measured, not guessed. Genesis Plus GX loading Landstalker reports 65536
/// bytes immediately after `retro_load_game`, then **0** after a single frame,
/// then settles at **8240** by frame 60. The first two answers are both wrong.
///
/// Restoring a save against the provisional size fails the length check and the
/// player's save silently does not load; worse, writing at that moment would put
/// bytes into a buffer the core is about to reallocate. So nothing touches SRAM
/// until the size has been the same on two consecutive checks.
pub const SETTLE_FRAMES: u32 = 120;

/// Frames between the two checks that must agree before the size is trusted.
pub const SETTLE_RECHECK_FRAMES: u32 = 60;

/// How often the emulation thread writes SRAM out while a game runs.
///
/// Short enough that a crash costs seconds of progress, long enough that a
/// 64 KB write every few seconds is nothing. Games write SRAM rarely, so most
/// of these find nothing changed and do no IO at all.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(10);

/// Where a ROM's battery save lives.
///
/// Named after the ROM file, with `.srm` — the convention every emulator uses,
/// so a save can be moved to or from RetroArch by hand.
pub fn sram_path(save_dir: &Path, rom_path: &Path) -> PathBuf {
    let stem = rom_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "game".to_string());
    save_dir.join(format!("{stem}.srm"))
}

/// Is every byte the same?
///
/// All-zeroes and all-`0xFF` are what uninitialised or failed-to-load cartridge
/// memory looks like. Real saves are never uniform.
pub fn is_uniform(data: &[u8]) -> bool {
    match data.first() {
        None => true,
        Some(first) => data.iter().all(|b| b == first),
    }
}

/// What to do with SRAM the core just handed us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Nothing changed since the last write.
    Unchanged,
    /// Write it.
    Write,
    /// Refuse: this would replace a real save with uninitialised memory.
    RefuseUniform,
    /// The core reported no save memory at all.
    NoSaveMemory,
}

/// Decide whether to persist, given what the core has now and what is on disk.
///
/// Pure, so the rule that protects people's saves is testable without a core,
/// a disk, or a game.
pub fn decide(current: Option<&[u8]>, previous: Option<&[u8]>, on_disk: Option<&[u8]>) -> Decision {
    let Some(current) = current else {
        return Decision::NoSaveMemory;
    };
    if current.is_empty() {
        return Decision::NoSaveMemory;
    }
    if previous == Some(current) {
        return Decision::Unchanged;
    }
    // The guard. A core that failed to initialise presents uniform memory; if
    // there is a real save on disk, writing over it destroys the game.
    if is_uniform(current) {
        if let Some(disk) = on_disk {
            if !is_uniform(disk) {
                return Decision::RefuseUniform;
            }
        }
    }
    Decision::Write
}

/// Write bytes so that an interrupted write cannot corrupt the destination.
///
/// Temp file, flush, fsync, rename. Rename is atomic within a filesystem on
/// every platform RustRomM targets, so a reader sees either the whole old file
/// or the whole new one.
pub fn write_atomically(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let tmp = path.with_extension("srm.tmp");
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("could not create {}", tmp.display()))?;
        f.write_all(data)?;
        f.flush()?;
        // Without this the rename can land before the data does, and a power
        // cut leaves a correctly-named empty file — worse than no file at all,
        // because it looks like a save.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("could not move the save into place at {}", path.display()))?;
    Ok(())
}

/// Read an existing save, or `None` if there is not one.
pub fn read(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_are_named_after_the_rom() {
        let p = sram_path(Path::new("/saves"), Path::new("/roms/Tetris (U) [!].gb"));
        assert_eq!(p, Path::new("/saves/Tetris (U) [!].srm"));
        // The .srm convention means a save can be carried to or from RetroArch.
        assert_eq!(p.extension().unwrap(), "srm");
    }

    #[test]
    fn a_rom_path_with_no_stem_still_produces_a_path() {
        let p = sram_path(Path::new("/saves"), Path::new("/"));
        assert!(p.starts_with("/saves"));
    }

    #[test]
    fn uniform_memory_is_recognised() {
        assert!(is_uniform(&[0u8; 64]));
        assert!(is_uniform(&[0xFFu8; 64]));
        assert!(is_uniform(&[]));
        assert!(!is_uniform(&[0, 0, 0, 1]));
    }

    #[test]
    fn unchanged_memory_is_not_rewritten() {
        // Games touch SRAM rarely. Rewriting 64 KB every ten seconds for hours
        // is pointless IO and pointless flash wear.
        let data = vec![1, 2, 3, 4];
        assert_eq!(decide(Some(&data), Some(&data), None), Decision::Unchanged);
    }

    #[test]
    fn a_core_with_no_save_memory_writes_nothing() {
        // Cartridges without a battery report zero bytes. Creating an empty
        // .srm for them would be noise at best and confusing at worst.
        assert_eq!(decide(None, None, None), Decision::NoSaveMemory);
        assert_eq!(decide(Some(&[]), None, None), Decision::NoSaveMemory);
    }

    #[test]
    fn a_real_save_is_written() {
        let data = vec![0, 1, 2, 3];
        assert_eq!(decide(Some(&data), None, None), Decision::Write);
        assert_eq!(decide(Some(&data), Some(&[9, 9]), None), Decision::Write);
    }

    #[test]
    fn uninitialised_memory_never_overwrites_a_real_save() {
        // THE test this module exists for. A core that failed to load presents
        // all-zero or all-0xFF SRAM. Writing that over a forty-hour save file
        // destroys it, and the player finds out much later.
        let blank = vec![0u8; 8192];
        let ones = vec![0xFFu8; 8192];
        let real = {
            let mut v = vec![0u8; 8192];
            v[100] = 42;
            v
        };
        assert_eq!(
            decide(Some(&blank), None, Some(&real)),
            Decision::RefuseUniform
        );
        assert_eq!(
            decide(Some(&ones), None, Some(&real)),
            Decision::RefuseUniform
        );
    }

    #[test]
    fn uniform_memory_may_replace_uniform_memory() {
        // A fresh cartridge legitimately starts blank, and a game that has just
        // erased its save is entitled to write that. The guard only protects
        // real data.
        let blank = vec![0u8; 64];
        let ones = vec![0xFFu8; 64];
        assert_eq!(decide(Some(&blank), None, Some(&ones)), Decision::Write);
        assert_eq!(decide(Some(&blank), None, None), Decision::Write);
    }

    #[test]
    fn a_real_save_may_replace_anything() {
        // The guard must never block genuine progress.
        let real = vec![1u8, 2, 3, 4];
        let blank = vec![0u8; 4];
        let other = vec![9u8, 9, 9, 9];
        assert_eq!(decide(Some(&real), None, Some(&blank)), Decision::Write);
        assert_eq!(decide(Some(&real), None, Some(&other)), Decision::Write);
    }

    #[test]
    fn writing_is_atomic_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.srm");
        write_atomically(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        write_atomically(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");

        // A leftover .tmp would eventually be mistaken for a save, or just
        // accumulate one per game forever.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left a temporary file behind");
    }

    #[test]
    fn writing_creates_the_save_directory() {
        // First run has no save directory at all; failing there would mean
        // nobody's first game ever saves.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/game.srm");
        write_atomically(&path, b"data").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"data");
    }

    #[test]
    fn a_missing_save_reads_as_none_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("nothing.srm")).is_none());
    }

    #[test]
    fn a_save_survives_a_round_trip_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.srm");
        // 64 KB, the size Genesis Plus GX reported in the spike.
        let data: Vec<u8> = (0..65536).map(|i| (i % 251) as u8).collect();
        write_atomically(&path, &data).unwrap();
        assert_eq!(read(&path).unwrap(), data);
    }
}
