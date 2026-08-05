# Changelog

## v0.3.0 — 2026-08-05

A **Logs** tab, so problems can be reported without opening a terminal:
Logs → Copy all produces a report headed by version, OS and architecture.

Fixed a launch bug that reported success on failure. Pressing Play with no
emulator configured fell back to the system opener — but no OS has a handler for
`.gbc` or `.smc`, and on macOS `open` fails *after* spawning successfully, so
the app said "Launched Tetris DX" while the real error went to a stderr nobody
was reading. It now waits for the exit status, and with no emulator configured
it says so and jumps to Settings with the right platform highlighted.

First-run setup no longer means typing paths by hand:

- **Browse…** buttons for the download folder and every emulator field, via a
  native file dialog. Typing a path was especially miserable on macOS, where the
  binary hides inside a `.app` bundle.
- **Detect** finds RetroArch, PPSSPP, Dolphin and friends in the usual install
  locations and on `PATH`.

Also: the project is now open for contributions — see `CONTRIBUTING.md`.

Tests: 69, up from 65. The new logging tests initially raced on the shared
buffer and are now serialised.

## v0.2.0 — 2026-08-05

Controller navigation. The project exists because RomM's browser player handles
gamepads badly; launching an external emulator fixed playing, but picking a game
still meant reaching for a mouse. Now it doesn't.

- Browse entirely from a controller: D-pad or left stick moves, **A** downloads
  or plays, **B** backs out, shoulder buttons change console.
- The same actions on the keyboard: arrows or `j`/`k`, `Enter`, `Esc`,
  `PgUp`/`PgDn`.
- The highlight is remembered per console, so returning to a platform puts you
  back on the game you were looking at.
- Analog sticks latch, so holding one moves the highlight once rather than every
  frame, and a resting stick's drift is ignored.
- Controller support is a Cargo feature (`gamepad`), on by default. Build with
  `--no-default-features` if `gilrs` won't compile on your platform.

Fixed:

- The control hints rendered as tofu boxes. egui's bundled font has no arrow
  glyphs, so `↑↓`, `▶` and `‹ ›` all drew as `□`. Found by screenshotting the
  running app — no test could have caught it.
- egui only repaints on window events, and a controller produces none, so the
  app appeared frozen until the mouse moved.
- Typing `j` into the search box scrolled the list instead of writing a letter.

Tests: 57 offline (up from 40), plus 8 opt-in against a real server.

## v0.1.0 — 2026-08-05

First release. Connect to a RomM server, browse the library by platform with
cover art, search, download with progress and cancellation, and launch games in
an emulator you already have.

- Native builds for Linux, macOS and Windows, x86_64 and arm64. Every target
  except Intel macOS is compiled and tested on a runner of its own architecture.
- Downloads stream to a `.part` file and are renamed only on success, so an
  interrupted transfer never leaves a truncated ROM.
- Verified against RomM 5.1.0.
