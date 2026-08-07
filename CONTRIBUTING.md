# Contributing

RustRomM works and is genuinely useful, but it is one person's side project and
there is a lot of obvious room to improve it. Contributions are very welcome —
especially from people who own hardware I don't.

## The quickest way to help: tell me what broke

You do not need to write any Rust. Open the app, click **Logs → Copy all**, and
paste that into an issue. The report includes the version, your OS and
architecture, and what the app actually did.

That tab exists because of a real bug: the app reported "Launched Tetris DX"
while macOS was printing an error to a terminal nobody was reading. Silent
failure is the enemy; the log is how we catch it.

The log never contains game data or your password.

## Getting set up

```sh
git clone https://github.com/cloudmaking/rustromm.git
cd rustromm
cargo run
```

Needs Rust 1.85+. On Linux you'll also want:

```sh
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev \
                 libxcursor-dev libxrandr-dev libxi-dev \
                 libudev-dev libgtk-3-dev libasound2-dev
```

Before opening a pull request:

```sh
cargo test                        # 69 tests, no server or controller needed
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

CI runs all of that on Linux, macOS and Windows, on both x86_64 and arm64. It
must be green.

### Testing against a real server

Most tests run against a mock RomM server, so no instance is needed. If you have
one, the live suite catches API changes the mocks can't see:

```sh
RUSTROMM_LIVE_URL=http://192.168.1.10:8087 \
RUSTROMM_LIVE_USER=you \
RUSTROMM_LIVE_PASS=secret \
cargo test --test live_tests -- --nocapture
```

`RUSTROMM_CONFIG_DIR` points settings at a throwaway directory, so testing never
touches your real configuration.

## Good first issues

Roughly in order of how much they'd improve the app:

**Save and state sync.** RomM exposes `/api/saves` and `/api/states`, and we
ignore both. Uploading a save after a session and pulling it before the next one
is the single most requested thing this doesn't do. Note the catch: with an
external emulator we can only find saves if we know where it writes them, which
means per-emulator path configuration.

**Device pairing instead of a stored password.** RomM has a device-code flow
(`/api/auth/device/init`, `/api/auth/device/token`) giving a revocable per-device
token. Right now we store a password in a JSON file. The Android client Argosy
already does this properly — see `docs/argosy-notes.md`.

**Gamepad navigation on the connect and settings screens.** The library screen
is fully controller-driven; the others are not, so first-run setup still needs a
keyboard.

**More emulator detection.** `detect_emulators()` in `src/launch.rs` checks a
hardcoded list of paths. Flatpak, Snap, Homebrew, Scoop and the Microsoft Store
all install elsewhere. Easy, self-contained, and immediately useful.

**Sensible default cores per platform.** Even with RetroArch found, the user must
know to pass `-L /path/to/some_core.so`. Shipping a sane default per platform
would remove most remaining setup.

**A Homebrew tap.** Quarantine is applied by whatever downloads a file — Safari
does, `curl` doesn't. A tap would let people `brew install` with no Gatekeeper
warning at all. Small, and it fixes the worst part of the first-run experience.

**Grid view with cover art.** The list view wastes the artwork we already fetch.

**Translations.** Every string is inline in `src/app.rs`; there is no i18n layer
yet, so this needs someone to add one first.

## The one architectural question

RustRomM launches an external emulator rather than embedding one. Argosy, the
Android client this follows, does the opposite — it embeds LibretroDroid and
plays in-process.

Embedding would make things genuinely seamless: no emulator configuration, and
save sync becomes possible because we'd control the emulator. It is also a large
undertaking, and on desktop RetroArch already exists and is better than anything
we would write.

A middle path looks promising: embed **software-rendered** libretro cores for
the 2D consoles (NES, SNES, Game Boy, GBC, GBA, Mega Drive, Master System),
which hand over a plain framebuffer and need no OpenGL — sidestepping the fact
that Apple has deprecated OpenGL. Keep delegating the heavy platforms (PSP, N64,
GameCube) to standalone emulators, which are better at those anyway.

That would cover the large majority of a typical library with zero setup. If
this interests you, open an issue before writing code — it is a big enough
change to be worth agreeing on first.

## House style

- Comments explain *why*, not *what*. If a line needs explaining, it usually
  needs a comment about the constraint that forced it.
- Prefer honest failure to silent success. See `open_with_os` in
  `src/launch.rs` for the cautionary tale.
- New behaviour comes with a test. Anything that can't be tested — controller
  hardware, emulator launching, how things look — goes in
  `docs/manual-testing.md` instead, so the gap is explicit.

## Licence

GPL-3.0-or-later. By contributing you agree your work ships under it.
