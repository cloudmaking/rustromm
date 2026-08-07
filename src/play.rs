//! Playing a game: input mapping and the screen it renders on.
//!
//! # The button layout question
//!
//! libretro's RetroPad is **positional**, laid out like a SNES pad: `B` is the
//! bottom face button and `A` is the right one. An Xbox pad has `A` at the
//! bottom. Mapping "A to A" by name therefore puts every face button in the
//! wrong place — the single most common mistake in this area.
//!
//! `gilrs` reports buttons by position (`South`, `East`, `West`, `North`), so
//! the correct mapping is positional and needs no transposition: South → `B`,
//! East → `A`. That lands right on every pad regardless of what its buttons are
//! printed with, which is what a player actually expects — the bottom button
//! confirms, on an Xbox pad, a PlayStation pad or a Nintendo one.

use egui::{Key, TextureHandle, TextureOptions};

use crate::libretro::sys;
use crate::libretro::video::{Frame, fit};

/// Keyboard defaults, for the very common case of a laptop with no controller.
///
/// Without these, "press play and the game runs" is false on the most common
/// hardware this app runs on: the game would start and be uncontrollable.
pub fn retropad_from_keys(pressed: impl Fn(Key) -> bool) -> u16 {
    let mut mask = 0u16;
    let mut set = |key: Key, id: u32| {
        if pressed(key) {
            mask |= 1 << id;
        }
    };
    set(Key::ArrowUp, sys::RETRO_DEVICE_ID_JOYPAD_UP);
    set(Key::ArrowDown, sys::RETRO_DEVICE_ID_JOYPAD_DOWN);
    set(Key::ArrowLeft, sys::RETRO_DEVICE_ID_JOYPAD_LEFT);
    set(Key::ArrowRight, sys::RETRO_DEVICE_ID_JOYPAD_RIGHT);
    // Z/X sit under the left hand with the arrows under the right, which is the
    // layout every browser emulator has used for twenty years.
    set(Key::Z, sys::RETRO_DEVICE_ID_JOYPAD_B);
    set(Key::X, sys::RETRO_DEVICE_ID_JOYPAD_A);
    set(Key::A, sys::RETRO_DEVICE_ID_JOYPAD_Y);
    set(Key::S, sys::RETRO_DEVICE_ID_JOYPAD_X);
    set(Key::Enter, sys::RETRO_DEVICE_ID_JOYPAD_START);
    set(Key::Backspace, sys::RETRO_DEVICE_ID_JOYPAD_SELECT);
    set(Key::Q, sys::RETRO_DEVICE_ID_JOYPAD_L);
    set(Key::W, sys::RETRO_DEVICE_ID_JOYPAD_R);
    mask
}

/// Which gilrs button maps to which RetroPad id.
///
/// Positional throughout — see the module note on why mapping by printed label
/// would be wrong.
#[cfg(feature = "gamepad")]
pub fn retropad_id_for_button(button: gilrs::Button) -> Option<u32> {
    use gilrs::Button;
    Some(match button {
        Button::DPadUp => sys::RETRO_DEVICE_ID_JOYPAD_UP,
        Button::DPadDown => sys::RETRO_DEVICE_ID_JOYPAD_DOWN,
        Button::DPadLeft => sys::RETRO_DEVICE_ID_JOYPAD_LEFT,
        Button::DPadRight => sys::RETRO_DEVICE_ID_JOYPAD_RIGHT,
        // Bottom button confirms. On a RetroPad that is B, not A.
        Button::South => sys::RETRO_DEVICE_ID_JOYPAD_B,
        Button::East => sys::RETRO_DEVICE_ID_JOYPAD_A,
        Button::West => sys::RETRO_DEVICE_ID_JOYPAD_Y,
        Button::North => sys::RETRO_DEVICE_ID_JOYPAD_X,
        Button::LeftTrigger => sys::RETRO_DEVICE_ID_JOYPAD_L,
        Button::RightTrigger => sys::RETRO_DEVICE_ID_JOYPAD_R,
        Button::LeftTrigger2 => sys::RETRO_DEVICE_ID_JOYPAD_L2,
        Button::RightTrigger2 => sys::RETRO_DEVICE_ID_JOYPAD_R2,
        Button::LeftThumb => sys::RETRO_DEVICE_ID_JOYPAD_L3,
        Button::RightThumb => sys::RETRO_DEVICE_ID_JOYPAD_R3,
        Button::Start => sys::RETRO_DEVICE_ID_JOYPAD_START,
        Button::Select => sys::RETRO_DEVICE_ID_JOYPAD_SELECT,
        _ => return None,
    })
}

/// Left stick pushed past this counts as a d-pad press.
///
/// Plenty of games only read the d-pad, and a player on a modern pad will use
/// the stick regardless. Lower than the menu threshold because in-game movement
/// should feel responsive rather than deliberate.
pub const STICK_DEADZONE: f32 = 0.5;

/// Fold analog stick position into the d-pad bits of a mask.
pub fn apply_stick(mask: u16, x: f32, y: f32) -> u16 {
    let mut mask = mask;
    if x <= -STICK_DEADZONE {
        mask |= 1 << sys::RETRO_DEVICE_ID_JOYPAD_LEFT;
    }
    if x >= STICK_DEADZONE {
        mask |= 1 << sys::RETRO_DEVICE_ID_JOYPAD_RIGHT;
    }
    // gilrs reports Y positive as up, matching the stick rather than the screen.
    if y >= STICK_DEADZONE {
        mask |= 1 << sys::RETRO_DEVICE_ID_JOYPAD_UP;
    }
    if y <= -STICK_DEADZONE {
        mask |= 1 << sys::RETRO_DEVICE_ID_JOYPAD_DOWN;
    }
    mask
}

/// Upload a frame to a texture, creating it if needed.
///
/// `TextureOptions::NEAREST` is not a preference. Retro games are drawn for
/// square pixels at a fixed grid, and bilinear filtering turns a sharp 160×144
/// image blown up to 1000 px into a blurred mess.
pub fn upload(ctx: &egui::Context, handle: &mut Option<TextureHandle>, frame: &Frame) {
    let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
    match handle {
        // Reusing the handle replaces the texture in place. Allocating a fresh
        // one every frame leaks GPU memory until the driver gives up.
        Some(h) if h.size() == [frame.width, frame.height] => h.set(image, TextureOptions::NEAREST),
        _ => *handle = Some(ctx.load_texture("game", image, TextureOptions::NEAREST)),
    }
}

/// Where to draw the game inside the space available.
///
/// Letterboxed and centred, honouring the core's display aspect — which is
/// frequently not `width / height`, because Mega Drive and SNES pixels are not
/// square.
pub fn placement(frame: &Frame, aspect: f32, available: egui::Vec2) -> egui::Vec2 {
    let (w, h) = fit(
        frame.width,
        frame.height,
        aspect,
        (available.x, available.y),
    );
    egui::vec2(w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn keys(list: &[Key]) -> u16 {
        let set: HashSet<Key> = list.iter().copied().collect();
        retropad_from_keys(|k| set.contains(&k))
    }

    #[test]
    fn nothing_pressed_is_a_clear_mask() {
        assert_eq!(keys(&[]), 0);
    }

    #[test]
    fn arrow_keys_reach_the_dpad() {
        assert_eq!(keys(&[Key::ArrowUp]), 1 << sys::RETRO_DEVICE_ID_JOYPAD_UP);
        assert_eq!(
            keys(&[Key::ArrowLeft, Key::ArrowDown]),
            (1 << sys::RETRO_DEVICE_ID_JOYPAD_LEFT) | (1 << sys::RETRO_DEVICE_ID_JOYPAD_DOWN)
        );
    }

    #[test]
    fn a_keyboard_alone_can_reach_every_button_a_game_needs() {
        // "Press play, game runs" has to be true on a laptop with no
        // controller — the most common hardware this runs on.
        let all = keys(&[
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Z,
            Key::X,
            Key::A,
            Key::S,
            Key::Enter,
            Key::Backspace,
            Key::Q,
            Key::W,
        ]);
        for id in [
            sys::RETRO_DEVICE_ID_JOYPAD_UP,
            sys::RETRO_DEVICE_ID_JOYPAD_DOWN,
            sys::RETRO_DEVICE_ID_JOYPAD_LEFT,
            sys::RETRO_DEVICE_ID_JOYPAD_RIGHT,
            sys::RETRO_DEVICE_ID_JOYPAD_A,
            sys::RETRO_DEVICE_ID_JOYPAD_B,
            sys::RETRO_DEVICE_ID_JOYPAD_X,
            sys::RETRO_DEVICE_ID_JOYPAD_Y,
            sys::RETRO_DEVICE_ID_JOYPAD_START,
            sys::RETRO_DEVICE_ID_JOYPAD_SELECT,
            sys::RETRO_DEVICE_ID_JOYPAD_L,
            sys::RETRO_DEVICE_ID_JOYPAD_R,
        ] {
            assert!(all & (1 << id) != 0, "no key reaches RetroPad id {id}");
        }
    }

    #[test]
    fn a_resting_stick_presses_nothing() {
        // Sticks drift. Without a dead zone the character walks on its own.
        for (x, y) in [(0.0, 0.0), (0.2, -0.1), (-0.35, 0.4), (0.49, 0.49)] {
            assert_eq!(
                apply_stick(0, x, y),
                0,
                "stick at ({x}, {y}) pressed something"
            );
        }
    }

    #[test]
    fn the_stick_is_not_upside_down() {
        // gilrs reports Y positive as up, matching the stick; screen
        // coordinates run the other way. Getting this backwards is invisible in
        // code review and instantly obvious to a player.
        assert_eq!(
            apply_stick(0, 0.0, 1.0),
            1 << sys::RETRO_DEVICE_ID_JOYPAD_UP
        );
        assert_eq!(
            apply_stick(0, 0.0, -1.0),
            1 << sys::RETRO_DEVICE_ID_JOYPAD_DOWN
        );
        assert_eq!(
            apply_stick(0, -1.0, 0.0),
            1 << sys::RETRO_DEVICE_ID_JOYPAD_LEFT
        );
        assert_eq!(
            apply_stick(0, 1.0, 0.0),
            1 << sys::RETRO_DEVICE_ID_JOYPAD_RIGHT
        );
    }

    #[test]
    fn the_stick_adds_to_the_dpad_rather_than_replacing_it() {
        let start = 1 << sys::RETRO_DEVICE_ID_JOYPAD_START;
        let out = apply_stick(start, 1.0, 0.0);
        assert!(
            out & start != 0,
            "applying the stick cleared an unrelated button"
        );
        assert!(out & (1 << sys::RETRO_DEVICE_ID_JOYPAD_RIGHT) != 0);
    }

    #[test]
    fn diagonals_work() {
        let out = apply_stick(0, 0.8, 0.8);
        assert!(out & (1 << sys::RETRO_DEVICE_ID_JOYPAD_RIGHT) != 0);
        assert!(out & (1 << sys::RETRO_DEVICE_ID_JOYPAD_UP) != 0);
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn the_bottom_face_button_is_retropad_b_not_a() {
        use gilrs::Button;
        // THE mapping mistake. RetroPad is positional and SNES-shaped: B is at
        // the bottom. Mapping Xbox A (which is at the bottom) to RetroPad A
        // would rotate every face button.
        assert_eq!(
            retropad_id_for_button(Button::South),
            Some(sys::RETRO_DEVICE_ID_JOYPAD_B)
        );
        assert_eq!(
            retropad_id_for_button(Button::East),
            Some(sys::RETRO_DEVICE_ID_JOYPAD_A)
        );
        assert_eq!(
            retropad_id_for_button(Button::West),
            Some(sys::RETRO_DEVICE_ID_JOYPAD_Y)
        );
        assert_eq!(
            retropad_id_for_button(Button::North),
            Some(sys::RETRO_DEVICE_ID_JOYPAD_X)
        );
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn every_retropad_id_is_reachable_from_a_controller_and_none_is_doubled() {
        use gilrs::Button;
        let buttons = [
            Button::DPadUp,
            Button::DPadDown,
            Button::DPadLeft,
            Button::DPadRight,
            Button::South,
            Button::East,
            Button::West,
            Button::North,
            Button::LeftTrigger,
            Button::RightTrigger,
            Button::LeftTrigger2,
            Button::RightTrigger2,
            Button::LeftThumb,
            Button::RightThumb,
            Button::Start,
            Button::Select,
        ];
        let mut seen = HashSet::new();
        for b in buttons {
            let id = retropad_id_for_button(b).unwrap_or_else(|| panic!("{b:?} maps to nothing"));
            assert!(seen.insert(id), "RetroPad id {id} is mapped twice");
        }
        assert_eq!(seen.len(), sys::JOYPAD_BUTTON_COUNT);
        // Buttons a RetroPad has no equivalent for must be ignored, not
        // silently folded onto something else.
        assert_eq!(retropad_id_for_button(Button::Mode), None);
    }

    #[test]
    fn the_game_is_letterboxed_to_the_cores_aspect_not_the_frames() {
        let frame = Frame {
            width: 256,
            height: 224,
            rgba: vec![0; 256 * 224 * 4],
        };
        // Mega Drive: 256x224 pixels displayed at 1.524, because its pixels are
        // not square. Using 256/224 = 1.143 makes every game too tall.
        let size = placement(&frame, 1.5238, egui::vec2(1000.0, 1000.0));
        assert!((size.x / size.y - 1.5238).abs() < 0.01, "got {size:?}");
        assert!(size.x <= 1000.0 && size.y <= 1000.0);
    }
}
