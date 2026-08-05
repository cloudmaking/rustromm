# RustRomM

A cross-platform desktop client for [RomM](https://github.com/rommapp/romm). Browse your
library, download games, and launch them in the emulator you already use — on Linux, macOS
and Windows.

> **Unofficial.** Not affiliated with or endorsed by the RomM project.

![RustRomM browsing a RomM library](docs/images/rustromm.png)

---

## Why

RomM's built-in browser player (EmulatorJS) works, but gamepad support in the browser is
janky — input lag, dropped buttons, remapping that doesn't stick. The Android client,
[Argosy](https://github.com/rommapp/argosy-launcher), is seamless by comparison because it
plays natively.

RustRomM takes the same approach on the desktop: it is a **launcher, not an emulator**. It
finds your games, downloads them, and hands the file to RetroArch, PPSSPP, Dolphin or
whatever you prefer. Controller support is then whatever your emulator already does — which
is to say, properly.

## What it does

- Connect to any RomM server with your normal username and password
- Browse the whole library or filter by platform, with cover art
- Search across your collection
- Download games, with progress, cancellation, and no half-written files left behind
- Launch straight into your emulator, configurable per platform
- **Drive the whole thing from a controller** — never touch the mouse
- Remembers your server; optionally remembers your password

### What it doesn't do (yet)

- Emulate anything itself — by design, see above
- Save/state sync back to RomM
- Collections, achievements, multiplayer

## Install

Download a build for your platform from
[Releases](https://github.com/cloudmaking/rustromm/releases). Both x86_64 and arm64 are
built for all three operating systems:

| | x86_64 | arm64 |
|---|---|---|
| Linux | `rustromm-linux-x86_64.tar.gz` | `rustromm-linux-arm64.tar.gz` |
| macOS | `rustromm-macos-x86_64.tar.gz` (Intel) | `rustromm-macos-arm64.tar.gz` (M-series) |
| Windows | `rustromm-windows-x86_64.zip` | `rustromm-windows-arm64.zip` |

Every build except Intel macOS is compiled *and* runs the test suite on a runner of its own
architecture — including both arm64 targets. The Intel macOS binary is cross-compiled from
an Apple Silicon runner, since GitHub is retiring its Intel macOS image, so it is the one
build not covered by a test run.

macOS and Windows builds are unsigned, so the OS will warn on first launch — on macOS,
right-click the app and choose Open; on Windows, More info → Run anyway.

### From source

Needs [Rust](https://rustup.rs/) 1.85 or newer.

```sh
git clone https://github.com/cloudmaking/rustromm.git
cd rustromm
cargo run --release
```

On Linux you may also need the usual GUI development libraries:

```sh
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev \
                 libxcursor-dev libxrandr-dev libxi-dev
```

## Using it

1. Launch it, enter your RomM server address (`192.168.1.10:8087` is fine — `http://` is
   assumed if you leave the scheme off), your username and password.
2. Pick a platform on the left, or search.
3. **Download** a game, then **Play**.

### Controller and keyboard

Plug in a pad and browse without reaching for the mouse — the point being that you can sit
on the sofa and never touch a keyboard between picking a game and playing it.

| Controller | Keyboard | |
|---|---|---|
| D-pad ↑↓ / left stick | `↑` `↓`, or `k` `j` | Move the highlight |
| D-pad ← → | `←` `→` | Previous / next page |
| **A** (bottom button) | `Enter` or `Space` | Download the game, or play it if already saved |
| **B** (right button) | `Esc` | Clear the search, then the highlight |
| LB / RB | `PgUp` / `PgDn` | Previous / next console |

Button names follow whatever pad you have: A on Xbox, Cross on PlayStation, B on a Nintendo
layout — it's always the bottom face button that confirms.

The highlight is remembered per console, so going back to the Mega Drive returns you to the
game you were looking at rather than the top of 726.

Controller support is a Cargo feature, on by default. If `gilrs` won't build on your
platform, `cargo build --no-default-features` gives you the app without it — keyboard
navigation still works.

### Pointing it at an emulator

**Settings → Emulators.** Set a default and, optionally, one per platform.

```
retroarch -L /usr/lib/libretro/snes9x_libretro.so
"C:\Program Files\RetroArch\retroarch.exe" -L cores\genesis_plus_gx.dll
/Applications/PPSSPPSDL.app/Contents/MacOS/PPSSPPSDL
```

Use `{rom}` where the file path should go. If you leave it out, the path is appended at the
end — which is what almost every emulator expects, so usually you can just name the program.

With nothing configured, Play hands the file to your operating system's default handler.

### Where things are stored

| | |
|---|---|
| Settings | `~/.config/rustromm/config.json` (Linux), `~/Library/Application Support/uk.cloudmaking.rustromm/` (macOS), `%APPDATA%\cloudmaking\rustromm\` (Windows) |
| Downloads | Your Downloads folder, under `RustRomM/<platform>/` — changeable in Settings |

Set `RUSTROMM_CONFIG_DIR` to override the settings location — handy for a portable install.

**On passwords:** "Remember password" stores it in that config file as plain text, with
owner-only permissions on Linux and macOS. It is not encrypted. Leave the box unticked if
that bothers you and type it each launch.

## Development

```sh
cargo test                        # 57 offline tests; no server or controller needed
cargo test --no-default-features  # same, with the gamepad feature off
cargo clippy --all-targets
cargo fmt
```

The suite has four layers:

| Layer | What it covers |
|---|---|
| Unit tests (22) | URL normalising, emulator command parsing, config fallbacks, size formatting, key and controller button mapping, stick dead-zone and latching |
| API tests (20) | The full HTTP stack against a mock RomM server — auth headers, query strings, streaming downloads, cancellation, error mapping |
| UI tests (15) | The real widget tree, headless, via `egui_kittest` — connect flow, error states, library rendering, and keyboard navigation end to end |
| Live tests (8) | Opt-in, against a real RomM instance — these self-skip, so `cargo test` reports 65 and runs 57 |

A checklist of what automation cannot reach — controller hardware, emulator
launching, how things actually look — is in
[`docs/manual-testing.md`](docs/manual-testing.md).

**One thing is not automatically tested: reading an actual controller.** That needs physical
hardware. The button-to-action mapping is a pure function with unit tests, and the keyboard
path that shares all the same logic is covered end to end — but the layer that talks to
`gilrs` is only verified by hand.

Live tests are skipped unless you point them at a server:

```sh
RUSTROMM_LIVE_URL=http://192.168.1.10:8087 \
RUSTROMM_LIVE_USER=you \
RUSTROMM_LIVE_PASS=secret \
cargo test --test live_tests -- --nocapture
```

These are the ones that catch RomM changing its API shape, which the mocks cannot see by
construction. Verified against **RomM 5.1.0**.

## Credits

- [RomM](https://github.com/rommapp/romm) — the server this talks to (AGPL-3.0)
- [Argosy](https://github.com/rommapp/argosy-launcher) — the Android client whose approach
  and interface this follows (GPL-3.0)
- Built with [egui](https://github.com/emilk/egui)

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

RustRomM follows the design of Argosy, which is GPL-3.0, so it is GPL-3.0 too — anything
derived from this has to stay open as well.

## Support

If this is useful to you: [Buy me a coffee](https://www.buymeacoffee.com/cloudmaking) ·
[PayPal](https://www.paypal.com/donate/?hosted_button_id=66P4DZ3GAYA8N)
