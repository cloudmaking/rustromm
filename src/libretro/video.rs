//! Converting a core's framebuffer into RGBA8 for display.
//!
//! Two traps live here, both confirmed against real cores rather than inferred
//! from the header — see `docs/libretro-spike.md`:
//!
//! 1. **`pitch` is bytes per row and is not `width * bpp`.** Gambatte delivers
//!    160 pixels of 2-byte colour — 320 bytes — in rows of 512. Snes9x uses
//!    2048-byte rows for 512 bytes of pixels. Treating the buffer as packed
//!    produces a diagonally sheared image, which looks like a decoding bug and
//!    is actually an indexing one.
//!
//! 2. **Frame size comes from the video callback, not from `av_info`.** Stella
//!    advertises a 320×228 geometry and then hands over 160×228 frames.
//!
//! Kept as free functions over plain slices so the whole thing is testable
//! without a core, a window, or a GPU.

use super::sys;

/// Colour layout the core chose via `SET_PIXEL_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelFormat {
    /// The format a core gets if it never sets one. Five bits per channel in
    /// the low 15 bits of a 16-bit little-endian word.
    #[default]
    Rgb1555,
    /// 32-bit, blue in the low byte, top byte ignored. Used by FCEUmm and
    /// Stella.
    Xrgb8888,
    /// 16-bit 5-6-5. The common case: Gambatte, mGBA, Snes9x, Genesis Plus GX
    /// and ProSystem all pick it.
    Rgb565,
}

impl PixelFormat {
    pub fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            sys::RETRO_PIXEL_FORMAT_0RGB1555 => Some(Self::Rgb1555),
            sys::RETRO_PIXEL_FORMAT_XRGB8888 => Some(Self::Xrgb8888),
            sys::RETRO_PIXEL_FORMAT_RGB565 => Some(Self::Rgb565),
            _ => None,
        }
    }

    /// Bytes per pixel in the *source* buffer.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb1555 | Self::Rgb565 => 2,
            Self::Xrgb8888 => 4,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Rgb1555 => "0RGB1555",
            Self::Xrgb8888 => "XRGB8888",
            Self::Rgb565 => "RGB565",
        }
    }
}

/// A converted frame, ready to become an egui texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Tightly packed RGBA, four bytes per pixel, `width * height * 4` long.
    pub rgba: Vec<u8>,
}

impl Frame {
    pub fn is_blank(&self) -> bool {
        self.rgba
            .chunks_exact(4)
            .all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
    }
}

/// Expand 5 bits to 8 by replicating the high bits into the low ones.
///
/// The naive `v << 3` never reaches 255, so white comes out as 248 and a
/// whole image is imperceptibly dark. Replication maps 31 to exactly 255.
const fn expand5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

const fn expand6(v: u8) -> u8 {
    (v << 2) | (v >> 4)
}

/// Convert one frame from a core's buffer into packed RGBA.
///
/// `pitch` is bytes per row as libretro reports it. Returns `None` if the
/// source buffer is too small for the stated geometry — a core that lies about
/// its dimensions should produce a missing frame, not an out-of-bounds read.
pub fn convert(
    src: &[u8],
    width: usize,
    height: usize,
    pitch: usize,
    format: PixelFormat,
) -> Option<Frame> {
    if width == 0 || height == 0 {
        return None;
    }
    let bpp = format.bytes_per_pixel();
    if pitch < width * bpp {
        return None;
    }
    // The final row only needs `width * bpp` bytes, not a full pitch — cores
    // are entitled to end the buffer there and several do.
    let required = pitch * (height - 1) + width * bpp;
    if src.len() < required {
        return None;
    }

    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        let row = &src[y * pitch..y * pitch + width * bpp];
        let out = &mut rgba[y * width * 4..(y + 1) * width * 4];
        match format {
            PixelFormat::Rgb565 => {
                for (px, o) in row.chunks_exact(2).zip(out.chunks_exact_mut(4)) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    o[0] = expand5(((v >> 11) & 0x1f) as u8);
                    o[1] = expand6(((v >> 5) & 0x3f) as u8);
                    o[2] = expand5((v & 0x1f) as u8);
                    o[3] = 255;
                }
            }
            PixelFormat::Rgb1555 => {
                for (px, o) in row.chunks_exact(2).zip(out.chunks_exact_mut(4)) {
                    let v = u16::from_le_bytes([px[0], px[1]]);
                    o[0] = expand5(((v >> 10) & 0x1f) as u8);
                    o[1] = expand5(((v >> 5) & 0x1f) as u8);
                    o[2] = expand5((v & 0x1f) as u8);
                    o[3] = 255;
                }
            }
            PixelFormat::Xrgb8888 => {
                // Little-endian XRGB: byte 0 is blue, byte 3 is the ignored X.
                for (px, o) in row.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                    o[0] = px[2];
                    o[1] = px[1];
                    o[2] = px[0];
                    o[3] = 255;
                }
            }
        }
    }
    Some(Frame {
        width,
        height,
        rgba,
    })
}

/// Size a frame to fit `available`, preserving the core's display aspect and
/// never exceeding the space given.
///
/// `aspect_ratio` is the core's *display* aspect, which is often not
/// `width / height`: Genesis Plus GX reports 1.524 for a 256×224 frame because
/// Mega Drive pixels are not square. Ignoring it makes every Mega Drive and
/// SNES game subtly too tall. Zero or a non-finite value means "assume square".
pub fn fit(frame_w: usize, frame_h: usize, aspect_ratio: f32, available: (f32, f32)) -> (f32, f32) {
    if frame_w == 0 || frame_h == 0 {
        return (0.0, 0.0);
    }
    let (aw, ah) = available;
    if aw <= 0.0 || ah <= 0.0 {
        return (0.0, 0.0);
    }
    let target = if aspect_ratio.is_finite() && aspect_ratio > 0.0 {
        aspect_ratio
    } else {
        frame_w as f32 / frame_h as f32
    };

    // Letterbox: fit within both dimensions, never crop.
    let by_width = (aw, aw / target);
    if by_width.1 <= ah {
        by_width
    } else {
        (ah * target, ah)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a source buffer with padded rows, so tests exercise the real
    /// layout rather than a convenient packed one.
    fn padded(width: usize, height: usize, bpp: usize, pitch: usize, fill: &[u8]) -> Vec<u8> {
        let mut buf = vec![0xAA; pitch * height];
        for y in 0..height {
            for x in 0..width {
                let at = y * pitch + x * bpp;
                buf[at..at + bpp].copy_from_slice(fill);
            }
        }
        buf
    }

    #[test]
    fn rgb565_white_is_fully_white() {
        // Shifting instead of replicating bits yields 248, not 255. That is
        // invisible in isolation and wrong across a whole image.
        let src = padded(2, 2, 2, 8, &0xFFFFu16.to_le_bytes());
        let f = convert(&src, 2, 2, 8, PixelFormat::Rgb565).unwrap();
        assert!(f.rgba.chunks_exact(4).all(|p| p == [255, 255, 255, 255]));
    }

    #[test]
    fn rgb565_channels_land_in_the_right_order() {
        // Pure red in 5-6-5 is 0xF800.
        let src = padded(1, 1, 2, 2, &0xF800u16.to_le_bytes());
        let f = convert(&src, 1, 1, 2, PixelFormat::Rgb565).unwrap();
        assert_eq!(&f.rgba, &[255, 0, 0, 255]);

        // Pure green is 0x07E0, pure blue 0x001F.
        let src = padded(1, 1, 2, 2, &0x07E0u16.to_le_bytes());
        assert_eq!(
            convert(&src, 1, 1, 2, PixelFormat::Rgb565).unwrap().rgba,
            [0, 255, 0, 255]
        );
        let src = padded(1, 1, 2, 2, &0x001Fu16.to_le_bytes());
        assert_eq!(
            convert(&src, 1, 1, 2, PixelFormat::Rgb565).unwrap().rgba,
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn rgb1555_ignores_the_top_bit() {
        // 0x7FFF and 0xFFFF differ only in the unused bit and must render the
        // same. A mask error here tints the whole image.
        let a = padded(1, 1, 2, 2, &0x7FFFu16.to_le_bytes());
        let b = padded(1, 1, 2, 2, &0xFFFFu16.to_le_bytes());
        assert_eq!(
            convert(&a, 1, 1, 2, PixelFormat::Rgb1555).unwrap().rgba,
            convert(&b, 1, 1, 2, PixelFormat::Rgb1555).unwrap().rgba
        );
    }

    #[test]
    fn xrgb8888_is_little_endian_bgrx() {
        // Byte order in memory is B, G, R, X — not R, G, B.
        let src = padded(1, 1, 4, 4, &[0x10, 0x20, 0x30, 0xFF]);
        let f = convert(&src, 1, 1, 4, PixelFormat::Xrgb8888).unwrap();
        assert_eq!(&f.rgba, &[0x30, 0x20, 0x10, 255]);
    }

    #[test]
    fn padded_rows_do_not_shear_the_image() {
        // THE bug this module exists to prevent. Gambatte's real geometry:
        // 160 px at 2 bpp is 320 bytes of pixels in a 512-byte row.
        let (w, h, pitch) = (160, 144, 512);
        let mut src = vec![0u8; pitch * h];
        // Mark the first pixel of each row with the row number, and fill the
        // padding with a value that must never appear in the output.
        for y in 0..h {
            src[y * pitch..y * pitch + pitch].fill(0xEE);
            let v = (y as u16) | 0x8000;
            src[y * pitch..y * pitch + 2].copy_from_slice(&v.to_le_bytes());
            for x in 1..w {
                src[y * pitch + x * 2..y * pitch + x * 2 + 2].copy_from_slice(&[0, 0]);
            }
        }
        let f = convert(&src, w, h, pitch, PixelFormat::Rgb565).unwrap();
        assert_eq!(f.rgba.len(), w * h * 4);

        // Every row's second pixel onwards must be black. If the copy assumed
        // packed rows, padding bytes would bleed in and this fails.
        for y in 0..h {
            for x in 1..w {
                let at = (y * w + x) * 4;
                assert_eq!(
                    &f.rgba[at..at + 3],
                    &[0, 0, 0],
                    "row {y} pixel {x} picked up padding — rows are being read at the wrong offset"
                );
            }
        }
    }

    #[test]
    fn a_short_buffer_is_rejected_rather_than_read_past() {
        let src = vec![0u8; 10];
        assert!(convert(&src, 160, 144, 512, PixelFormat::Rgb565).is_none());
    }

    #[test]
    fn the_last_row_need_not_be_padded() {
        // Cores may end the buffer after the final row's pixels. Requiring a
        // full trailing pitch would reject valid frames.
        let (w, h, pitch) = (4, 3, 16);
        let src = vec![0u8; pitch * (h - 1) + w * 2];
        assert!(convert(&src, w, h, pitch, PixelFormat::Rgb565).is_some());
    }

    #[test]
    fn a_pitch_narrower_than_the_row_is_rejected() {
        let src = vec![0u8; 4096];
        assert!(convert(&src, 160, 144, 100, PixelFormat::Rgb565).is_none());
    }

    #[test]
    fn zero_sized_frames_are_rejected() {
        let src = vec![0u8; 4096];
        assert!(convert(&src, 0, 144, 512, PixelFormat::Rgb565).is_none());
        assert!(convert(&src, 160, 0, 512, PixelFormat::Rgb565).is_none());
    }

    #[test]
    fn pixel_formats_round_trip_through_their_raw_values() {
        assert_eq!(PixelFormat::from_raw(0), Some(PixelFormat::Rgb1555));
        assert_eq!(PixelFormat::from_raw(1), Some(PixelFormat::Xrgb8888));
        assert_eq!(PixelFormat::from_raw(2), Some(PixelFormat::Rgb565));
        assert_eq!(PixelFormat::from_raw(3), None);
        // The default matters: it is what a core gets if it never asks.
        assert_eq!(PixelFormat::default(), PixelFormat::Rgb1555);
    }

    #[test]
    fn non_square_pixels_are_respected() {
        // Genesis Plus GX: a 256x224 frame that must be displayed at 1.524,
        // not at 256/224 = 1.143. Getting this wrong makes it too tall.
        let (w, h) = fit(256, 224, 1.5238, (1000.0, 1000.0));
        assert!((w / h - 1.5238).abs() < 0.001, "got {w}x{h}");
        assert!(w <= 1000.0 && h <= 1000.0);
    }

    #[test]
    fn a_missing_aspect_ratio_falls_back_to_square_pixels() {
        for bad in [0.0, f32::NAN, f32::INFINITY, -1.0] {
            let (w, h) = fit(160, 144, bad, (1600.0, 1600.0));
            assert!(
                (w / h - 160.0 / 144.0).abs() < 0.001,
                "aspect {bad} gave {w}x{h}"
            );
        }
    }

    #[test]
    fn fitting_letterboxes_rather_than_cropping() {
        // Wide space, short frame: height is the limit.
        let (w, h) = fit(256, 224, 0.0, (4000.0, 100.0));
        assert!(h <= 100.0 + f32::EPSILON);
        assert!(w <= 4000.0);
        // Tall space: width is the limit.
        let (w, h) = fit(256, 224, 0.0, (100.0, 4000.0));
        assert!(w <= 100.0 + f32::EPSILON);
        assert!(h <= 4000.0);
    }

    #[test]
    fn fitting_degenerate_space_yields_nothing_rather_than_panicking() {
        assert_eq!(fit(160, 144, 1.0, (0.0, 100.0)), (0.0, 0.0));
        assert_eq!(fit(160, 144, 1.0, (100.0, -5.0)), (0.0, 0.0));
        assert_eq!(fit(0, 144, 1.0, (100.0, 100.0)), (0.0, 0.0));
    }

    #[test]
    fn a_black_frame_is_reported_as_blank() {
        // Used to catch the "core loaded but renders nothing" case, which
        // otherwise looks exactly like success.
        let src = vec![0u8; 512 * 144];
        let f = convert(&src, 160, 144, 512, PixelFormat::Rgb565).unwrap();
        assert!(f.is_blank());
    }
}
