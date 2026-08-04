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
/// Used for "Show in folder" and as the fallback when no emulator is set.
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

    cmd.spawn()
        .with_context(|| format!("could not open {}", path.display()))?;
    Ok(())
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
