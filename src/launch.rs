//! Handing a downloaded ROM to an emulator.
//!
//! RustRomM does not emulate anything itself. Bundling libretro cores would be
//! a far larger project and a worse one — people already have RetroArch,
//! PPSSPP, Dolphin and their own preferred settings. We download the file and
//! start their emulator with it.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Run `command` with `rom_path`.
///
/// `command` may include arguments (`retroarch -L /path/to/core.so`). The
/// literal token `{rom}` is replaced with the ROM path; if it isn't present,
/// the path is appended as the final argument, which is what almost every
/// emulator expects.
pub fn launch(command: &str, rom_path: &Path) -> Result<()> {
    let tokens = tokenise(command);
    let Some((program, args)) = tokens.split_first() else {
        bail!("no emulator command configured");
    };

    let rom = rom_path.to_string_lossy().to_string();
    let mut had_placeholder = false;
    let mut final_args: Vec<String> = args
        .iter()
        .map(|a| {
            if a.contains("{rom}") {
                had_placeholder = true;
                a.replace("{rom}", &rom)
            } else {
                a.clone()
            }
        })
        .collect();
    if !had_placeholder {
        final_args.push(rom);
    }

    Command::new(program)
        .args(&final_args)
        .spawn()
        .with_context(|| {
            format!("could not start '{program}' — is it installed and on your PATH?")
        })?;
    Ok(())
}

/// Open a path with whatever the OS considers the default handler.
///
/// Used for "Show in folder". **Not** a good fallback for ROMs: no operating
/// system ships a handler for `.gbc` or `.smc`, so this fails for exactly the
/// case it would be reached in.
///
/// The status is waited on rather than spawned and forgotten. macOS `open`
/// exits non-zero *after* spawning successfully when nothing claims the file
/// — so a fire-and-forget spawn reports success while the launch has actually
/// failed, and the real error only appears on a stderr nobody is reading.
pub fn open_with_os(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        // `start` is a cmd builtin, not an executable, and the empty string is
        // the window title — without it a quoted path is treated as the title.
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };

    // These helpers hand off to another process and exit immediately, so
    // waiting costs milliseconds and buys us a real exit status.
    let status = cmd
        .status()
        .with_context(|| format!("could not open {}", path.display()))?;

    if !status.success() {
        bail!(
            "your system has no application registered for {}",
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string())
        );
    }
    Ok(())
}

/// Emulators we know how to look for, in preference order.
///
/// Typing a full path into a text box is a miserable first-run experience —
/// especially on macOS, where the real binary is buried inside a `.app` bundle
/// and Finder will not show you the path. So we look in the usual places.
#[cfg(target_os = "macos")]
const CANDIDATES: &[(&str, &str)] = &[
    (
        "RetroArch",
        "/Applications/RetroArch.app/Contents/MacOS/RetroArch",
    ),
    (
        "OpenEmu",
        "/Applications/OpenEmu.app/Contents/MacOS/OpenEmu",
    ),
    (
        "PPSSPP",
        "/Applications/PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL",
    ),
    (
        "Dolphin",
        "/Applications/Dolphin.app/Contents/MacOS/Dolphin",
    ),
];

#[cfg(target_os = "linux")]
const CANDIDATES: &[(&str, &str)] = &[
    ("RetroArch", "/usr/bin/retroarch"),
    ("RetroArch (local)", "/usr/local/bin/retroarch"),
    (
        "RetroArch (Flatpak)",
        "/var/lib/flatpak/exports/bin/org.libretro.RetroArch",
    ),
    ("PPSSPP", "/usr/bin/ppsspp"),
    ("Dolphin", "/usr/bin/dolphin-emu"),
];

#[cfg(target_os = "windows")]
const CANDIDATES: &[(&str, &str)] = &[
    ("RetroArch", r"C:\Program Files\RetroArch\retroarch.exe"),
    (
        "RetroArch (x86)",
        r"C:\Program Files (x86)\RetroArch\retroarch.exe",
    ),
    ("PPSSPP", r"C:\Program Files\PPSSPP\PPSSPPWindows64.exe"),
    ("Dolphin", r"C:\Program Files\Dolphin\Dolphin.exe"),
];

/// Emulators actually present on this machine, as (label, command) pairs.
pub fn detect_emulators() -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = CANDIDATES
        .iter()
        .filter(|(_, path)| Path::new(path).exists())
        .map(|(label, path)| ((*label).to_string(), (*path).to_string()))
        .collect();

    // Anything on PATH counts too, and covers package managers that install
    // somewhere we didn't guess.
    for name in ["retroarch", "ppsspp", "dolphin-emu", "mgba-qt", "mednafen"] {
        if found.iter().any(|(_, cmd)| cmd.ends_with(name)) {
            continue;
        }
        if let Some(path) = which_on_path(name) {
            found.push((name.to_string(), path));
        }
    }
    found
}

/// Minimal `which`, to avoid a dependency for one lookup.
fn which_on_path(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|p| p.to_string_lossy().to_string())
}

/// Split a command line on whitespace, honouring single and double quotes so
/// that emulator paths containing spaces survive (`"C:\Program Files\..."`).
fn tokenise(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;

    for ch in input.trim().chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                // An empty quoted string is still a token: `foo "" bar`.
                has_token = true;
            }
            None if ch.is_whitespace() => {
                if has_token || !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            None => current.push(ch),
        }
    }
    if has_token || !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_command() {
        assert_eq!(tokenise("retroarch"), vec!["retroarch"]);
    }

    #[test]
    fn command_with_args() {
        assert_eq!(
            tokenise("retroarch -L /cores/snes9x.so"),
            vec!["retroarch", "-L", "/cores/snes9x.so"]
        );
    }

    #[test]
    fn quoted_path_with_spaces() {
        assert_eq!(
            tokenise(r#""C:\Program Files\RetroArch\retroarch.exe" -f"#),
            vec![r"C:\Program Files\RetroArch\retroarch.exe", "-f"]
        );
    }

    #[test]
    fn single_quotes_too() {
        assert_eq!(
            tokenise("'/Applications/My Emu.app/emu' --run"),
            vec!["/Applications/My Emu.app/emu", "--run"]
        );
    }

    #[test]
    fn collapses_repeated_whitespace() {
        assert_eq!(tokenise("  a   b  "), vec!["a", "b"]);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(tokenise("   ").is_empty());
    }
}
