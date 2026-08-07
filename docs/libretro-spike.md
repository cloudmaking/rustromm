# libretro spike — measured, not assumed

Before designing embedded emulation, one question had to be answered with a running
program: **can a stock libretro core be loaded and stepped from Rust, and what does it
actually cost?** Everything else in the plan rests on the answer, so it was tested first
and cheaply.

~180 lines of hand-written FFI over `libloading`, no libretro crate. Cores taken
unmodified from `buildbot.libretro.com/nightly/linux/x86_64/latest/`. ROMs taken from a
real 5,736-game RomM library, not test fixtures.

Run on the development machine: Linux Mint 22.1, Intel i5-8265U (4C/8T, 2018 laptop part).
Deliberately modest hardware — if it is comfortable here it is comfortable anywhere.

## Result: 6 of 7 cores ran on the first attempt

| Core | System | Frame | Pixel format | fps | Audio Hz | ms/frame | Headroom |
|---|---|---|---|---|---|---|---|
| Gambatte 0.5.0 | Game Boy | 160×144 | RGB565 | 59.7275 | 32768 | 0.186 | **90×** |
| ProSystem 1.3e | Atari 7800 | 320×223 | RGB565 | 60.0000 | 31440 | 0.626 | 27× |
| Genesis Plus GX 1.7.4 | Mega Drive | 256×224 | RGB565 | 59.9227 | 44100 | 0.716 | 23× |
| FCEUmm | NES | 256×240 | XRGB8888 | 60.0998 | 48000 | 0.754 | 22× |
| mGBA 0.11.0 | GB/GBA | 160×144 | RGB565 | 59.7275 | 131072 | 0.776 | 22× |
| Snes9x 1.63 | SNES | 256×224 | RGB565 | 60.0988 | 32040 | 1.131 | 15× |
| Stella 8.0_pre | Atari 2600 | 160×228 | XRGB8888 | 60.0000 | 31440 | 2.277 | 7× |
| Handy 0.97 | Atari Lynx | — | — | — | — | — | **SIGSEGV** |

Verified beyond "it returned true": every passing core delivered frames whose pixels were
counted and found non-black, so a core that loaded but rendered nothing could not pass.

## What this changes

### 1. No libretro crate is needed

The frontend surface is about 30 symbols. Hand-written FFI worked on the first attempt
against seven independently-built cores and removes any dependency on a third-party
binding crate — several of which are core-side rather than frontend-side, and most of
which are unmaintained. For a one-person project, an unmaintained dependency in the
load-bearing layer is a worse risk than 180 lines of FFI.

### 2. Run-ahead is affordable, which is the whole point of the project

RustRomM exists because gamepad input in RomM's browser player is bad. Matching RetroArch
would be par; beating it needs **run-ahead**, which hides one or more frames of inherent
input latency by re-simulating. It costs a full extra emulated frame per displayed frame.

At 15–90× realtime headroom, that is comfortably affordable on every core measured except
Stella. Even Snes9x — the most demanding of the popular set at 1.13 ms against a 16.6 ms
budget — leaves room for several run-ahead frames.

This is the strongest argument for embedding: it makes achievable something the launcher
model could never offer.

### 3. `pitch` really is padded, and the geometry really does lie

Two traps, both confirmed rather than merely warned about:

**Pitch is bytes-per-row and is not width × bpp.** Gambatte delivers 160 px at 2 bpp — 320
bytes of pixels — in rows of **512 bytes**. Snes9x: 256 px, 512 bytes of pixels, **2048
byte** rows. A copy that assumes packed rows produces a diagonally sheared image.

**`retro_get_system_av_info` geometry is a hint, not the truth.** Stella reports a base
geometry of 320×228 and then delivers 160×228 frames. Sizing anything from `av_info`
rather than from the per-frame `width`/`height` arguments gets Atari 2600 wrong.

### 4. Pixel format is per-core and both formats are in real use

RGB565 in five cores, XRGB8888 in two. Neither is safely assumable; the environment
callback must record whatever the core sets and the conversion must handle both.
0RGB1555 was not observed but remains the format a core gets if it never sets one.

### 5. `need_fullpath` is not hypothetical

Genesis Plus GX sets `need_fullpath = true` — it wants a file path and will not accept
a buffer. Six of seven wanted data in memory. Both paths must exist from day one, which
matters because RomM serves ROMs over HTTP and the natural implementation holds them in
memory.

### 6. A core can take the whole app down — demonstrated, not theorised

Handy **segfaults inside `retro_load_game`** on a missing `lynxboot.img`, rather than
returning `false` as the API allows. Exit code 139, core dumped.

This is the single most consequential finding. A libretro core is arbitrary C++ running
in-process, and the very first seven-core sample contained one that crashes the host on a
foreseeable user condition — no BIOS. In a launcher, a crashing emulator is a separate
process and the library survives. Embedded, it is our crash, our bug report, and
potentially the user's unsaved progress.

Consequences for the design:

- **BIOS presence must be checked before `retro_load_game`, not after.** By the time the
  core tells us, the process is gone.
- **SRAM must be flushed on a timer**, not only on clean exit, because a clean exit is not
  guaranteed.
- Out-of-process core hosting deserves a real evaluation rather than being dismissed as
  overengineering.

### 7. Save state sizes span three orders of magnitude

1.2 KB (Stella) to 1,036 KB (Genesis Plus GX), with Snes9x at 823 KB. Any design that
syncs states to RomM has to treat them as substantial uploads, not metadata. SRAM is
separate and much smaller — Genesis Plus GX reported 64 KB, and cartridges without battery
backup correctly report 0.

## Distribution: five of six targets have cores, one has none

Probed `buildbot.libretro.com` directly.

| Target | Path | Cores |
|---|---|---|
| Linux x86_64 | `nightly/linux/x86_64/latest/` | ✅ `.so.zip` |
| Linux arm64 | `nightly/linux/aarch64/latest/` | ✅ `.so.zip` |
| macOS x86_64 | `nightly/apple/osx/x86_64/latest/` | ✅ `.dylib.zip` |
| macOS arm64 | `nightly/apple/osx/arm64/latest/` | ✅ `.dylib.zip` |
| Windows x86_64 | `nightly/windows/x86_64/latest/` | ✅ `.dll.zip` |
| **Windows arm64** | — | ❌ **none exist** |

`nightly/windows/` contains only `x86/` and `x86_64/`. There is no arm64 Windows core
build on the buildbot at all.

An arm64 Windows build of RustRomM cannot load an x86_64 DLL, so that target cannot use
downloaded cores. The workable answer is to ship the **x86_64 Windows binary** to Windows
on ARM and let the OS's built-in x86_64 emulation run the whole app — the measured
headroom is large enough to absorb it. Building cores for win-arm64 in CI is the
alternative and is much more work.

This needs to be stated plainly in the README rather than discovered by a user.

## Reproducing

```sh
curl -O https://buildbot.libretro.com/nightly/linux/x86_64/latest/gambatte_libretro.so.zip
unzip gambatte_libretro.so.zip
cargo run --release -- gambatte_libretro.so "some-game.gb"
```

The spike source is not part of the shipped crate; it exists to be re-run when a claim
here is doubted. Every number above came from it, on one machine, in one sitting — they
are indicative, not a benchmark suite.

---

## Update: the two unknowns are settled, by CI rather than by argument

The research phase named these the risks that would sink the rewrite. Both were
answerable with one CI run each, so they were answered before anything was built
on top of them.

### Windows on ARM can load an x64 core under emulation ✅

The buildbot has no arm64 Windows cores, and a native ARM64 process cannot
`LoadLibrary` an x64 DLL — so the plan was to ship the x86_64 build to Windows on
ARM and let Prism emulate the whole app. Nobody had verified that an *emulated*
x64 process could then load an x64 core.

It can. The `windows-arm-under-emulation` CI job builds for
`x86_64-pc-windows-msvc` on a `windows-11-arm` runner — exactly what a
Windows-on-ARM user downloads — and the full core suite passes. That target has a
path, and it is the same binary everyone else on Windows gets.

### macOS loads an ad-hoc-signed third-party core ✅

The buildbot's arm64 dylibs are ad-hoc signed with no CMS blob and are not
notarized; the x86_64 ones carry no signature at all. Library validation could
have refused them outright.

It does not. `Test (macOS arm64)` downloads and `dlopen`s a buildbot core and
passes.

**One caveat worth keeping honest:** this proves it for an *unsigned* binary,
which is what RustRomM ships today. Library validation is imposed by the Hardened
Runtime, so if the app is ever signed and notarized it will need the
`com.apple.security.cs.disable-library-validation` entitlement, or this stops
working. Anyone setting up signing should read this paragraph first.

### And one that changed the design

Handing a core the log interface is not optional, and doing it in pure Rust does
not work. `c_variadic` is still unstable on Rust 1.97, so a non-variadic
stand-in receives the format string with no arguments substituted. Measured
against Gambatte, that loses **100%** of the content — every message it emits is
`log_cb(level, "[Gambatte] %s\n", text)`, so the string we would capture is
literally `[Gambatte] %s`. Cores announce missing BIOS files the same way.

The fix is nineteen lines of C doing the `vsnprintf`, in
`src/libretro/shim/log_shim.c`. Before and after, same core, same ROM:

```
  [core INFO] [Gambatte] %s   (arguments not expanded)     <- pure Rust
  [core INFO] [Gambatte] MBC1 ROM loaded.                  <- via the shim
  [core INFO] [Gambatte] Got internal game name: TETRIS2.
```

It compiles on all six targets in CI.

---

## Update: the reported save size lies, twice

Found while wiring up battery saves, and worth its own section because getting it
wrong silently loses people's games.

`retro_get_memory_size(RETRO_MEMORY_SAVE_RAM)` is not stable after
`retro_load_game`. Genesis Plus GX, loading Landstalker:

| When | Reported SRAM |
|---|---|
| Immediately after `retro_load_game` | **65536** bytes |
| After 1 frame | **0** |
| After 60 frames | **8240** |
| After 600 frames | 8240 |

Only the last is true. The obvious implementation — read the size right after
load, restore the save into it — fails in two ways at once:

- Restoring an 8240-byte save against the provisional 65536 fails the length
  check, so the player's save silently does not load.
- Writing at that moment puts bytes into a buffer the core is about to
  reallocate.

And the transient **0** is worse than either, because it looks exactly like "this
cartridge has no battery". A frontend that concluded that would refuse to save
the game at all, forever, and the player would have no idea why.

RustRomM therefore touches SRAM only once the reported size has been identical on
two consecutive checks, 60 frames apart — `saves::SETTLE_FRAMES`. Restore happens
then, not at load. Nothing is written before it.

Two further games confirm the pattern is per-cartridge rather than per-core: 3
Ninjas Kick Back and Phantasy Star II both settle at 0, which is correct — those
cartridges have no battery. So the transient 65536 is not a constant to subtract,
it is genuinely meaningless.
