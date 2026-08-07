//! The emulation thread.
//!
//! # Why a thread at all
//!
//! egui repaints on its own schedule — on window events, on `request_repaint`,
//! and not at all when the app is idle. A core must advance at its own rate,
//! which is 59.7275 Hz on a Game Boy and 60.0988 Hz on a SNES, matching neither
//! the display nor egui's whims. Driving `retro_run` from inside the UI frame
//! ties emulation speed to repaint scheduling: the game slows down when the
//! window is unfocused, stutters when the user drags it, and runs at the
//! monitor's rate rather than the console's.
//!
//! So the core gets its own thread with its own clock, and the UI reads
//! whatever the latest frame happens to be. A dropped frame is then a visual
//! hiccup rather than a change in game speed.
//!
//! # What this thread must never do
//!
//! Block. It owns the only handle to a core that may be mid-frame, and a core
//! that stops being called stops producing audio, which is audible immediately.
//! Commands are therefore taken without blocking and the frame budget is
//! respected even when the UI is not asking for anything.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::core::{AvInfo, Core, CoreInfo, Diagnostics};
use super::video::Frame;
use crate::logging;

/// Sent from the UI thread to the emulation thread.
enum Command {
    SetPaused(bool),
    Reset,
    SaveState(Sender<Result<Vec<u8>>>),
    LoadState(Vec<u8>, Sender<Result<()>>),
    SaveRam(Sender<Option<Vec<u8>>>),
    Stop,
}

/// Shared with the UI thread. Everything here is read every repaint, so it is
/// either atomic or behind a lock held for as short a time as possible.
struct Shared {
    frame: Mutex<Option<Frame>>,
    /// Frames the core has produced. Compared against repaints to spot the
    /// emulation thread having died or stalled.
    frames: AtomicU64,
    /// Actual emulation rate in millihertz, so the UI can show that the game is
    /// running slow rather than leaving the player to guess.
    rate_mhz: AtomicU32,
    paused: AtomicBool,
    running: AtomicBool,
    /// Set when the core asked to shut down, or the thread stopped by itself.
    finished: AtomicBool,
}

/// A running game. Dropping this stops the emulation thread and unloads the
/// core.
pub struct Emulator {
    shared: Arc<Shared>,
    commands: Sender<Command>,
    handle: Option<JoinHandle<()>>,
    pub info: CoreInfo,
    pub av: AvInfo,
}

/// How far behind schedule the loop tolerates before giving up on catching up.
///
/// Without this, a laptop that sleeps mid-game wakes to a deadline hours in the
/// past and fast-forwards through the whole interval as quickly as it can. The
/// cap turns that into a dropped second rather than a burst of unplayable
/// nonsense — and, more importantly, a burst of unplayable audio.
const MAX_CATCHUP: Duration = Duration::from_millis(250);

/// Pure scheduling decision, split out so it can be tested without a core.
///
/// Returns how long to sleep before the next `retro_run`, and the deadline to
/// use for the frame after that.
fn pace(now: Instant, deadline: Instant, period: Duration) -> (Duration, Instant) {
    if now >= deadline {
        // Behind schedule. Run immediately, but do not try to reclaim more than
        // MAX_CATCHUP — see above.
        let behind = now.duration_since(deadline);
        let next = if behind > MAX_CATCHUP {
            now + period
        } else {
            deadline + period
        };
        (Duration::ZERO, next)
    } else {
        (deadline.duration_since(now), deadline + period)
    }
}

impl Emulator {
    /// Load a core and a ROM, and start running.
    ///
    /// The core is loaded on the emulation thread rather than here, because
    /// `retro_init` and `retro_load_game` must happen on the thread that will
    /// call `retro_run` — some cores keep thread-local state.
    pub fn start(
        core_path: PathBuf,
        rom_path: PathBuf,
        rom: Vec<u8>,
        system_dir: PathBuf,
        save_dir: PathBuf,
        initial_sram: Option<Vec<u8>>,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            frame: Mutex::new(None),
            frames: AtomicU64::new(0),
            rate_mhz: AtomicU32::new(0),
            paused: AtomicBool::new(false),
            running: AtomicBool::new(true),
            finished: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        // The thread reports back what the core turned out to be, so `start`
        // can fail with a real error rather than returning a handle to a
        // session that never began.
        let (ready_tx, ready_rx) = channel();

        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("rustromm-emu".into())
            .spawn(move || {
                run_thread(
                    thread_shared,
                    rx,
                    ready_tx,
                    core_path,
                    rom_path,
                    rom,
                    system_dir,
                    save_dir,
                    initial_sram,
                )
            })?;

        match ready_rx.recv() {
            Ok(Ok((info, av))) => Ok(Self {
                shared,
                commands: tx,
                handle: Some(handle),
                info,
                av,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            // The channel closing without a message means the thread died
            // before it could report — in practice, a core that panicked.
            Err(_) => {
                let _ = handle.join();
                anyhow::bail!("the emulation thread stopped before the game loaded")
            }
        }
    }

    /// The most recent frame, or `None` if the core has not produced one yet.
    pub fn frame(&self) -> Option<Frame> {
        self.shared
            .frame
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn frames_rendered(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    /// Measured emulation rate in frames per second.
    pub fn measured_fps(&self) -> f32 {
        self.shared.rate_mhz.load(Ordering::Relaxed) as f32 / 1000.0
    }

    /// True once the core has asked to quit or the thread has stopped.
    pub fn finished(&self) -> bool {
        self.shared.finished.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
        let _ = self.commands.send(Command::SetPaused(paused));
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Set the pressed buttons for a port.
    ///
    /// Goes straight to the core's atomics rather than through the command
    /// channel: a queued input would arrive a frame or more late, and input
    /// latency is the entire reason this project exists.
    pub fn set_buttons(&self, port: usize, mask: u16) {
        super::core::set_buttons_global(port, mask);
    }

    pub fn reset(&self) {
        let _ = self.commands.send(Command::Reset);
    }

    /// Round-trips to the emulation thread, because serializing while the core
    /// is mid-frame would capture an inconsistent state.
    pub fn save_state(&self) -> Result<Vec<u8>> {
        let (tx, rx) = channel();
        self.commands.send(Command::SaveState(tx))?;
        rx.recv()?
    }

    pub fn load_state(&self, data: Vec<u8>) -> Result<()> {
        let (tx, rx) = channel();
        self.commands.send(Command::LoadState(data, tx))?;
        rx.recv()?
    }

    /// The battery save, for persisting. `None` when the cartridge has none.
    pub fn save_ram(&self) -> Option<Vec<u8>> {
        let (tx, rx) = channel();
        self.commands.send(Command::SaveRam(tx)).ok()?;
        rx.recv().ok().flatten()
    }

    pub fn diagnostics(&self) -> Diagnostics {
        super::core::diagnostics_global()
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        let _ = self.commands.send(Command::Stop);
        if let Some(h) = self.handle.take() {
            // Joining matters: the core must finish unloading before another
            // can be loaded, and the single-instance flag is released in
            // Core::drop on the emulation thread.
            let _ = h.join();
        }
    }
}

type Ready = Result<(CoreInfo, AvInfo)>;

#[allow(clippy::too_many_arguments)]
fn run_thread(
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    ready: Sender<Ready>,
    core_path: PathBuf,
    rom_path: PathBuf,
    rom: Vec<u8>,
    system_dir: PathBuf,
    save_dir: PathBuf,
    initial_sram: Option<Vec<u8>>,
) {
    let mut core = match Core::load(&core_path, &system_dir, &save_dir) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e));
            shared.finished.store(true, Ordering::SeqCst);
            return;
        }
    };
    let info = core.info().clone();
    logging::info(format!("loaded core {} {}", info.name, info.version));

    let av = match core.load_game(&rom_path, rom) {
        Ok(av) => av,
        Err(e) => {
            let _ = ready.send(Err(e));
            shared.finished.store(true, Ordering::SeqCst);
            return;
        }
    };
    logging::info(format!(
        "{} running at {:.4} fps, {:.0} Hz audio",
        info.name, av.fps, av.sample_rate
    ));

    // Restore the battery save before the first frame, or the game boots,
    // sees no save, and may overwrite it.
    if let Some(sram) = initial_sram {
        match core.restore_save_ram(&sram) {
            Ok(()) => logging::info(format!("restored {} bytes of battery save", sram.len())),
            Err(e) => logging::warn(format!("could not restore battery save: {e}")),
        }
    }

    if ready.send(Ok((info, av))).is_err() {
        return; // The caller gave up; nothing to run for.
    }

    let period = Duration::from_secs_f64(1.0 / av.fps);
    let mut deadline = Instant::now() + period;
    let mut rate_window = Instant::now();
    let mut rate_frames = 0u32;

    while shared.running.load(Ordering::Relaxed) {
        // Drain commands without blocking. Blocking here would stop the core
        // mid-game whenever the UI thread went quiet.
        loop {
            match commands.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => {
                    shared.running.store(false, Ordering::SeqCst);
                    break;
                }
                Ok(Command::SetPaused(p)) => shared.paused.store(p, Ordering::Relaxed),
                Ok(Command::Reset) => {
                    core.reset();
                    logging::info("game reset");
                }
                Ok(Command::SaveState(reply)) => {
                    let _ = reply.send(core.save_state());
                }
                Ok(Command::LoadState(data, reply)) => {
                    let _ = reply.send(core.load_state(&data));
                }
                Ok(Command::SaveRam(reply)) => {
                    let _ = reply.send(core.save_ram());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if !shared.running.load(Ordering::Relaxed) {
            break;
        }

        if shared.paused.load(Ordering::Relaxed) {
            // Do not advance, but keep the deadline moving so unpausing does
            // not trigger a catch-up burst.
            std::thread::sleep(Duration::from_millis(8));
            deadline = Instant::now() + period;
            continue;
        }

        core.run();

        // Audio is drained and dropped until the audio thread lands. Draining
        // is not optional: the callback appends to a Vec on every frame, so
        // ignoring it grows the heap without limit for as long as the game runs.
        let _ = core.drain_audio();

        if let Some(f) = core.take_frame() {
            *shared.frame.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
            shared.frames.fetch_add(1, Ordering::Relaxed);
        }

        if core.wants_shutdown() {
            logging::info("core requested shutdown");
            break;
        }

        rate_frames += 1;
        if rate_window.elapsed() >= Duration::from_secs(1) {
            let fps = rate_frames as f64 / rate_window.elapsed().as_secs_f64();
            shared
                .rate_mhz
                .store((fps * 1000.0) as u32, Ordering::Relaxed);
            rate_frames = 0;
            rate_window = Instant::now();
        }

        let (sleep_for, next) = pace(Instant::now(), deadline, period);
        deadline = next;
        if !sleep_for.is_zero() {
            std::thread::sleep(sleep_for);
        }
    }

    shared.finished.store(true, Ordering::SeqCst);
    logging::info("emulation stopped");
    // `core` drops here, on the thread that loaded it, releasing the
    // single-instance flag so another game can start.
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_micros(16_744); // 59.7275 fps

    #[test]
    fn pacing_sleeps_until_the_deadline_when_ahead() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(10);
        let (sleep, next) = pace(now, deadline, PERIOD);
        assert!(sleep >= Duration::from_millis(9) && sleep <= Duration::from_millis(10));
        assert_eq!(next, deadline + PERIOD);
    }

    #[test]
    fn pacing_runs_immediately_when_slightly_behind() {
        let now = Instant::now();
        let deadline = now - Duration::from_millis(5);
        let (sleep, next) = pace(now, deadline, PERIOD);
        assert!(sleep.is_zero());
        // Deadline advances from the ORIGINAL deadline, so a frame that ran a
        // little late is absorbed rather than permanently shifting the clock.
        assert_eq!(next, deadline + PERIOD);
    }

    #[test]
    fn pacing_gives_up_catching_up_after_a_long_stall() {
        // The laptop-slept-for-an-hour case. Without the cap, the loop would
        // try to emulate an hour of gameplay as fast as it could.
        let now = Instant::now();
        let deadline = now - Duration::from_secs(3600);
        let (sleep, next) = pace(now, deadline, PERIOD);
        assert!(sleep.is_zero());
        assert!(
            next > now && next <= now + PERIOD,
            "after a long stall the schedule must restart from now, not from an hour ago"
        );
    }

    #[test]
    fn pacing_absorbs_a_run_of_slightly_late_frames_without_drifting() {
        // Emulation must stay at the core's rate on average. Repeatedly
        // resetting the deadline to `now` would make every game run slow by
        // however long a frame takes.
        let start = Instant::now();
        let mut deadline = start + PERIOD;
        let late = Duration::from_micros(500);
        for i in 1..=100u32 {
            let now = start + PERIOD * i + late;
            let (_, next) = pace(now, deadline, PERIOD);
            deadline = next;
        }
        let expected = start + PERIOD * 101;
        let drift = if deadline > expected {
            deadline - expected
        } else {
            expected - deadline
        };
        assert!(
            drift < Duration::from_micros(100),
            "schedule drifted by {drift:?} over 100 frames"
        );
    }
}
