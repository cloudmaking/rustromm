//! Sound.
//!
//! Cores emit signed 16-bit stereo at a rate of their own choosing, and those
//! rates are strange: 32040 Hz on Snes9x, 32768 on Gambatte, 44100 on Genesis
//! Plus GX, **131072** on mGBA. The output device runs at 48000 and will not
//! negotiate. So every sample is resampled, and the ratio is not fixed — it is
//! nudged continuously, because the emulation clock and the sound card clock
//! drift apart no matter how carefully the first is set.
//!
//! # Why the ratio has to move
//!
//! A core producing exactly 48000 samples per second into a device consuming
//! exactly 48000 would still drift: the two clocks are different crystals. Left
//! alone, the buffer slowly empties (clicks) or fills (growing latency, then
//! clicks). Every mature frontend solves this the same way — measure buffer
//! fullness and trim the resampling ratio by a fraction of a percent to hold it
//! near half full. That is the `PI`-ish controller in `Resampler::adjust`.
//!
//! # The downsampling trap
//!
//! mGBA at 131072 Hz to 48000 is a **2.7× decimation**. Linear interpolation
//! alone aliases audibly when downsampling — content above the output Nyquist
//! folds back as tones that were never in the signal. So a low-pass runs ahead
//! of the interpolator whenever the input rate is higher than the output.
//! Upsampling (32040 → 48000) does not need one and does not get one.
//!
//! The first attempt used a box average, on the grounds that it was cheap and
//! roughly right. It is not: a two-tap box at 131072 Hz puts its first null at
//! 65 kHz, so a 40 kHz tone came through at 0.28 amplitude and would have been
//! plainly audible as a sound the game never made. The test caught it. What is
//! here now is a windowed-sinc FIR, which is thirty lines and actually works.

use std::collections::VecDeque;

/// Device rate. Every desktop audio stack resamples to its own clock anyway,
/// and 48 kHz is what they all use natively.
pub const OUTPUT_RATE: u32 = 48_000;

/// Target buffer occupancy, in seconds of audio.
///
/// The trade is latency against dropouts. 80 ms is enough to ride out a
/// scheduler hiccup on a loaded laptop, and is below the ~100 ms where audio
/// lag starts being noticeable against the picture.
pub const TARGET_LATENCY_S: f32 = 0.080;

/// How hard the controller pulls the ratio back. 0.5% maximum correction —
/// enough to beat any real clock drift, small enough to be inaudible.
const MAX_RATIO_TRIM: f64 = 0.005;

/// Windowed-sinc low-pass, used only when decimating.
///
/// Designed against the *input* rate with the cutoff placed just under the
/// output Nyquist, so everything that would fold back is gone before the
/// interpolator ever sees it.
struct LowPass {
    taps: Vec<f32>,
    /// Previous input, so filtering is continuous across chunk boundaries.
    history: VecDeque<(f32, f32)>,
}

impl LowPass {
    /// `cutoff_hz` and `rate_hz` are both in Hz; `len` is forced odd for a
    /// symmetric, linear-phase response.
    fn new(cutoff_hz: f64, rate_hz: f64, len: usize) -> Self {
        let len = len | 1;
        let fc = (cutoff_hz / rate_hz).clamp(0.0001, 0.499);
        let mid = (len / 2) as f64;
        let mut taps: Vec<f32> = (0..len)
            .map(|i| {
                let x = i as f64 - mid;
                let sinc = if x.abs() < 1e-9 {
                    2.0 * fc
                } else {
                    (std::f64::consts::TAU * fc * x).sin() / (std::f64::consts::PI * x)
                };
                // Blackman window: much deeper stopband than Hamming, which
                // matters here because the alias we are killing is loud.
                let n = i as f64 / (len - 1) as f64;
                let w = 0.42 - 0.5 * (std::f64::consts::TAU * n).cos()
                    + 0.08 * (2.0 * std::f64::consts::TAU * n).cos();
                (sinc * w) as f32
            })
            .collect();
        // Normalise to unity gain at DC, or the whole signal changes volume.
        let sum: f32 = taps.iter().sum();
        if sum.abs() > 1e-9 {
            for t in &mut taps {
                *t /= sum;
            }
        }
        Self {
            taps,
            history: VecDeque::from(vec![(0.0, 0.0); len]),
        }
    }

    fn process(&mut self, frames: &[(f32, f32)], out: &mut Vec<(f32, f32)>) {
        for &f in frames {
            self.history.push_back(f);
            self.history.pop_front();
            let mut l = 0.0;
            let mut r = 0.0;
            for (tap, sample) in self.taps.iter().zip(self.history.iter()) {
                l += tap * sample.0;
                r += tap * sample.1;
            }
            out.push((l, r));
        }
    }
}

/// Converts a core's output rate to the device rate, with drift correction.
pub struct Resampler {
    /// Nominal input rate as the core reported it.
    source_rate: f64,
    /// Fractional read position between input samples.
    position: f64,
    /// Effective step per output sample. `source/output`, trimmed for drift.
    step: f64,
    /// Last stereo pair, kept so interpolation works across chunk boundaries.
    last: (f32, f32),
    /// Anti-aliasing filter, present only when the core outputs faster than the
    /// device. Upsampling cannot alias, so it costs nothing there.
    low_pass: Option<LowPass>,
    /// Scratch buffers, reused so a 60 Hz audio callback does not allocate.
    frames: Vec<(f32, f32)>,
    filtered: Vec<(f32, f32)>,
}

impl Resampler {
    pub fn new(source_rate: f64) -> Self {
        let ratio = source_rate / OUTPUT_RATE as f64;
        Self {
            source_rate,
            position: 0.0,
            step: ratio,
            last: (0.0, 0.0),
            // 45% of the output rate leaves a little transition room below the
            // 24 kHz output Nyquist without eating anything audible. 63 taps is
            // enough for a steep enough roll-off at these ratios and costs
            // microseconds per frame.
            low_pass: (ratio > 1.05)
                .then(|| LowPass::new(OUTPUT_RATE as f64 * 0.45, source_rate, 63)),
            frames: Vec::new(),
            filtered: Vec::new(),
        }
    }

    pub fn source_rate(&self) -> f64 {
        self.source_rate
    }

    /// Nudge the ratio to hold the output buffer near `TARGET_LATENCY_S`.
    ///
    /// `fullness` is the buffer occupancy as a fraction of the target: 1.0 is
    /// exactly on target, 0 is empty, 2 is twice as full as wanted.
    ///
    /// Too full means we are producing faster than the device consumes, so read
    /// the input *faster* to catch up — hence the ratio rising with fullness.
    pub fn adjust(&mut self, fullness: f32) {
        let error = (fullness as f64 - 1.0).clamp(-1.0, 1.0);
        let trim = 1.0 + error * MAX_RATIO_TRIM;
        self.step = (self.source_rate / OUTPUT_RATE as f64) * trim;
    }

    /// Current ratio, for diagnostics.
    pub fn ratio(&self) -> f64 {
        self.step
    }

    /// Resample interleaved stereo i16 into interleaved stereo f32.
    pub fn process(&mut self, input: &[i16], out: &mut Vec<f32>) {
        if input.len() < 2 {
            return;
        }
        // Convert once, then low-pass if we are decimating, so the
        // interpolator below never sees content above its Nyquist limit.
        self.frames.clear();
        self.frames.extend(
            input
                .chunks_exact(2)
                .map(|p| (p[0] as f32 / 32768.0, p[1] as f32 / 32768.0)),
        );
        if self.frames.is_empty() {
            return;
        }
        let frames: &[(f32, f32)] = match self.low_pass.as_mut() {
            Some(lp) => {
                self.filtered.clear();
                lp.process(&self.frames, &mut self.filtered);
                &self.filtered
            }
            None => &self.frames,
        };
        let step = self.step;

        let mut pos = self.position;
        while pos < frames.len() as f64 {
            let i = pos.floor() as usize;
            let frac = (pos - i as f64) as f32;
            // Sample -1 is the last frame of the previous chunk; without it,
            // every chunk boundary is a discontinuity, which is an audible click
            // at whatever rate chunks arrive.
            let a = if i == 0 { self.last } else { frames[i - 1] };
            let b = frames[i];
            out.push(a.0 + (b.0 - a.0) * frac);
            out.push(a.1 + (b.1 - a.1) * frac);
            pos += step;
        }
        self.last = *frames.last().unwrap();
        // Carry the fractional remainder into the next chunk so the phase is
        // continuous. Dropping it would add a fraction of a sample of jitter to
        // every chunk.
        self.position = pos - frames.len() as f64;
    }
}

/// Samples waiting for the sound card, and the numbers needed to explain a
/// crackle after the fact.
pub struct AudioBuffer {
    samples: VecDeque<f32>,
    capacity: usize,
    /// Times the device asked for audio and there was not enough. Each one is
    /// an audible click, and the count is the single most useful audio
    /// diagnostic there is.
    pub underruns: u64,
    /// Samples thrown away because the buffer was full. Means the core is
    /// outrunning the device.
    pub overruns: u64,
}

impl AudioBuffer {
    pub fn new() -> Self {
        // Four times the target, so a burst has somewhere to go without the
        // latency controller having to react instantly.
        let capacity = (OUTPUT_RATE as f32 * TARGET_LATENCY_S * 4.0) as usize * 2;
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
            underruns: 0,
            overruns: 0,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            if self.samples.len() >= self.capacity {
                // Drop the oldest rather than the newest: falling behind should
                // cost latency, not continuity.
                self.samples.pop_front();
                self.overruns += 1;
            }
            self.samples.push_back(s);
        }
    }

    /// Fill an output buffer, padding with silence if short.
    pub fn fill(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            match self.samples.pop_front() {
                Some(s) => *slot = s,
                None => {
                    *slot = 0.0;
                    self.underruns += 1;
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Occupancy relative to the target, for the drift controller.
    pub fn fullness(&self) -> f32 {
        let target = OUTPUT_RATE as f32 * TARGET_LATENCY_S * 2.0;
        self.samples.len() as f32 / target
    }
}

impl Default for AudioBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Device output ───────────────────────────────────────────────────────────

/// A live audio device fed by the emulation thread.
///
/// Deliberately not constructed on the emulation thread: `cpal::Stream` is
/// `!Send` on some platforms, and on macOS the device must be opened from the
/// thread that owns the run loop. The UI thread creates it and hands the shared
/// buffer to the emulator.
pub struct AudioOutput {
    _stream: cpal::Stream,
    pub buffer: std::sync::Arc<std::sync::Mutex<AudioBuffer>>,
    pub device_name: String,
    pub sample_rate: u32,
}

impl AudioOutput {
    /// Open the default output device.
    ///
    /// Returns an error rather than panicking when there is no sound card —
    /// CI runners have none, and a silent game is far better than a game that
    /// refuses to start.
    pub fn open() -> anyhow::Result<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no audio output device"))?;
        // cpal 0.18 dropped `name()`; Device implements Display instead.
        let device_name = device.to_string();
        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;

        let buffer = std::sync::Arc::new(std::sync::Mutex::new(AudioBuffer::new()));
        let cb_buffer = std::sync::Arc::clone(&buffer);

        // Only f32 is handled. Every desktop platform offers it, and supporting
        // the integer formats as well would mean three copies of the callback
        // for a case that does not arise in practice.
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config.into(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut buf = cb_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if channels == 2 {
                        buf.fill(out);
                    } else {
                        // Mono or surround: fill from the stereo stream and
                        // duplicate, rather than producing silence.
                        let mut stereo = vec![0.0f32; (out.len() / channels) * 2];
                        buf.fill(&mut stereo);
                        for (frame, pair) in out.chunks_mut(channels).zip(stereo.chunks(2)) {
                            let mono = (pair[0] + pair[1]) * 0.5;
                            for s in frame.iter_mut() {
                                *s = mono;
                            }
                        }
                    }
                },
                move |err| crate::logging::error(format!("audio stream error: {err}")),
                None,
            )?,
            other => anyhow::bail!("audio device wants {other:?} samples, which is not supported"),
        };
        stream.play()?;
        crate::logging::info(format!(
            "audio: {device_name} at {sample_rate} Hz, {channels} ch"
        ));

        Ok(Self {
            _stream: stream,
            buffer,
            device_name,
            sample_rate,
        })
    }

    pub fn stats(&self) -> (u64, u64, usize) {
        let b = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
        (b.underruns, b.overruns, b.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Interleaved stereo sine, both channels identical.
    fn sine(freq: f32, rate: f32, seconds: f32) -> Vec<i16> {
        let n = (rate * seconds) as usize;
        (0..n)
            .flat_map(|i| {
                let v = ((i as f32 / rate) * freq * TAU).sin() * 0.5;
                let s = (v * 32767.0) as i16;
                [s, s]
            })
            .collect()
    }

    /// Frequency estimate from zero crossings — enough to tell 1 kHz from an
    /// alias, without pulling in an FFT.
    fn estimate_freq(samples: &[f32], rate: f32) -> f32 {
        let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
        let crossings = left
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        crossings as f32 * rate / (2.0 * left.len() as f32)
    }

    #[test]
    fn upsampling_preserves_the_tone() {
        // Snes9x's real rate. A 1 kHz tone must still be 1 kHz at 48 kHz out.
        let mut r = Resampler::new(32040.0);
        let mut out = Vec::new();
        r.process(&sine(1000.0, 32040.0, 0.5), &mut out);

        let got = estimate_freq(&out, OUTPUT_RATE as f32);
        assert!(
            (got - 1000.0).abs() < 25.0,
            "1 kHz resampled 32040 -> 48000 came out at {got} Hz"
        );
        // Roughly 48000/32040 as many frames out as in.
        let expected = (0.5 * OUTPUT_RATE as f32) as usize * 2;
        let ratio = out.len() as f32 / expected as f32;
        assert!(
            (0.97..=1.03).contains(&ratio),
            "produced {} samples, expected ~{expected}",
            out.len()
        );
    }

    #[test]
    fn downsampling_preserves_the_tone_too() {
        // mGBA's real rate: 131072 Hz, a 2.7x decimation.
        let mut r = Resampler::new(131072.0);
        let mut out = Vec::new();
        r.process(&sine(1000.0, 131072.0, 0.5), &mut out);

        let got = estimate_freq(&out, OUTPUT_RATE as f32);
        assert!(
            (got - 1000.0).abs() < 30.0,
            "1 kHz resampled 131072 -> 48000 came out at {got} Hz"
        );
    }

    #[test]
    fn decimation_attenuates_content_above_the_output_nyquist() {
        // THE test this module exists for. A 40 kHz tone at 131072 Hz is above
        // 48 kHz output Nyquist (24 kHz). Linear interpolation alone folds it
        // back as a loud, entirely fictional low tone. The box filter must
        // squash it instead.
        let mut r = Resampler::new(131072.0);
        let mut out = Vec::new();
        r.process(&sine(40_000.0, 131072.0, 0.3), &mut out);

        // Skip the filter's start-up transient: its history begins at zero, so
        // the first tap-length of output is a ramp rather than its steady-state
        // response. Measuring that would understate the filter, not flatter it.
        let steady = &out[out.len() / 4..];
        let peak = steady.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak < 0.005,
            "a 40 kHz tone survived decimation at amplitude {peak} — it will be \
             audible as an alias that was never in the game's audio"
        );

        // And the audible band must still pass, or the filter is just deafness.
        let mut r = Resampler::new(131072.0);
        let mut out = Vec::new();
        r.process(&sine(1000.0, 131072.0, 0.3), &mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak > 0.3,
            "1 kHz was attenuated to {peak} — the filter is too aggressive"
        );
    }

    #[test]
    fn chunk_boundaries_do_not_click() {
        // Audio arrives one frame at a time. If each chunk restarts from zero
        // instead of carrying the previous sample, every boundary is a step
        // discontinuity — a click at the frame rate, which is 60 Hz of buzz.
        let mut r = Resampler::new(32040.0);
        let whole = sine(200.0, 32040.0, 0.2);
        let mut chunked = Vec::new();
        for chunk in whole.chunks(534 * 2) {
            r.process(chunk, &mut chunked);
        }

        // No sample-to-sample jump larger than a smooth 200 Hz wave could make.
        let max_jump = chunked
            .iter()
            .step_by(2)
            .zip(chunked.iter().step_by(2).skip(1))
            .fold(0.0f32, |m, (a, b)| m.max((b - a).abs()));
        assert!(
            max_jump < 0.05,
            "a jump of {max_jump} between consecutive samples — chunk boundaries are clicking"
        );
    }

    #[test]
    fn the_drift_controller_pushes_the_right_way() {
        let mut r = Resampler::new(48_000.0);
        let nominal = r.ratio();

        // Buffer too full: the core is ahead, so consume its samples faster.
        r.adjust(2.0);
        assert!(
            r.ratio() > nominal,
            "an over-full buffer must speed the read up"
        );
        // Buffer too empty: slow down and let it refill.
        r.adjust(0.0);
        assert!(
            r.ratio() < nominal,
            "an empty buffer must slow the read down"
        );
        // On target: back to nominal.
        r.adjust(1.0);
        assert!((r.ratio() - nominal).abs() < 1e-9);
    }

    #[test]
    fn the_drift_correction_stays_inaudible() {
        // A correction big enough to hear is worse than the drift it fixes:
        // pitch wobble is far more noticeable than a click every few minutes.
        let mut r = Resampler::new(48_000.0);
        let nominal = r.ratio();
        for extreme in [0.0, 100.0, -50.0] {
            r.adjust(extreme);
            let change = (r.ratio() / nominal - 1.0).abs();
            assert!(
                change <= MAX_RATIO_TRIM + 1e-9,
                "fullness {extreme} moved the ratio by {change}, which is audible"
            );
        }
    }

    #[test]
    fn an_empty_buffer_yields_silence_and_counts_it() {
        // Silence is the right failure. Repeating stale samples would buzz, and
        // the count is what makes a crackle report actionable.
        let mut b = AudioBuffer::new();
        let mut out = vec![9.0f32; 64];
        b.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
        assert_eq!(b.underruns, 64);
    }

    #[test]
    fn samples_come_out_in_the_order_they_went_in() {
        let mut b = AudioBuffer::new();
        b.push(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = vec![0.0; 4];
        b.fill(&mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(b.underruns, 0);
        assert!(b.is_empty());
    }

    #[test]
    fn overflow_drops_the_oldest_audio_not_the_newest() {
        // Falling behind should cost latency, not continuity — keeping stale
        // audio and discarding fresh would make the game sound like it is
        // lagging further behind the longer it runs.
        let mut b = AudioBuffer::new();
        let cap = b.capacity;
        let flood: Vec<f32> = (0..cap + 1000).map(|i| i as f32).collect();
        b.push(&flood);
        assert_eq!(b.len(), cap);
        assert_eq!(b.overruns, 1000);
        let mut out = vec![0.0; 1];
        b.fill(&mut out);
        assert_eq!(
            out[0], 1000.0,
            "the oldest samples should have been dropped"
        );
    }

    #[test]
    fn fullness_reads_one_at_the_target_latency() {
        let mut b = AudioBuffer::new();
        assert_eq!(b.fullness(), 0.0);
        let target = (OUTPUT_RATE as f32 * TARGET_LATENCY_S * 2.0) as usize;
        b.push(&vec![0.0; target]);
        assert!((b.fullness() - 1.0).abs() < 0.01);
    }

    #[test]
    fn a_short_or_empty_chunk_is_ignored_rather_than_panicking() {
        let mut r = Resampler::new(44_100.0);
        let mut out = Vec::new();
        r.process(&[], &mut out);
        r.process(&[42], &mut out); // half a stereo pair
        assert!(out.is_empty());
    }
}
