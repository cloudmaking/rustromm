//! Navigation input, from the keyboard or a game controller.
//!
//! Both sources are funnelled into one [`NavAction`] enum so the UI has a
//! single code path. That also makes the interesting part testable: the
//! keyboard layer is exercised through `egui_kittest`, and the controller
//! mapping is a pure function with unit tests. Nothing here needs real
//! hardware to verify.

/// A navigation intent, independent of what produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    Up,
    Down,
    /// Confirm — download the highlighted game, or play it if already saved.
    Confirm,
    /// Back out: clear the search, or drop the highlight.
    Back,
    PagePrev,
    PageNext,
    /// Move to the previous/next platform in the sidebar.
    PlatformPrev,
    PlatformNext,
}

/// Keyboard mapping.
///
/// Arrow keys and Enter are the obvious ones. Page navigation is on the
/// left/right arrows because that is what the D-pad maps to, and the two
/// should agree.
pub fn key_to_action(key: egui::Key) -> Option<NavAction> {
    use egui::Key;
    Some(match key {
        Key::ArrowUp | Key::K => NavAction::Up,
        Key::ArrowDown | Key::J => NavAction::Down,
        Key::ArrowLeft => NavAction::PagePrev,
        Key::ArrowRight => NavAction::PageNext,
        Key::Enter | Key::Space => NavAction::Confirm,
        Key::Escape => NavAction::Back,
        Key::PageUp => NavAction::PlatformPrev,
        Key::PageDown => NavAction::PlatformNext,
        _ => return None,
    })
}

/// Controller mapping, following the convention every console uses: the
/// bottom face button confirms, the right face button goes back.
///
/// `gilrs` normalises across pads, so `South` is A on Xbox, Cross on
/// PlayStation and B on a Nintendo layout — all of them the bottom button.
#[cfg(feature = "gamepad")]
pub fn button_to_action(button: gilrs::Button) -> Option<NavAction> {
    use gilrs::Button;
    Some(match button {
        Button::DPadUp => NavAction::Up,
        Button::DPadDown => NavAction::Down,
        Button::DPadLeft => NavAction::PagePrev,
        Button::DPadRight => NavAction::PageNext,
        Button::South => NavAction::Confirm,
        Button::East => NavAction::Back,
        // Shoulders jump between consoles, mirroring tab-switching on a pad.
        Button::LeftTrigger => NavAction::PlatformPrev,
        Button::RightTrigger => NavAction::PlatformNext,
        _ => return None,
    })
}

/// Below this, a stick is considered centred. Analog sticks rest slightly
/// off-zero, so a threshold is required or the list scrolls on its own.
#[cfg(feature = "gamepad")]
const STICK_THRESHOLD: f32 = 0.6;

/// Translates a left-stick Y position into an action, but only when it
/// *crosses* the threshold — otherwise holding the stick would emit an action
/// every frame. Returns the new latched state alongside any action.
#[cfg(feature = "gamepad")]
pub fn stick_y_to_action(value: f32, was_latched: bool) -> (Option<NavAction>, bool) {
    if value.abs() < STICK_THRESHOLD {
        return (None, false);
    }
    if was_latched {
        return (None, true);
    }
    // gilrs reports up as positive.
    let action = if value > 0.0 {
        NavAction::Up
    } else {
        NavAction::Down
    };
    (Some(action), true)
}

/// Polls connected controllers.
///
/// Holds no state beyond gilrs itself plus the stick latch. When the
/// `gamepad` feature is off this is a stub that reports nothing connected,
/// so the rest of the app compiles unchanged.
#[cfg(feature = "gamepad")]
pub struct Gamepads {
    gilrs: Option<gilrs::Gilrs>,
    stick_latched: bool,
}

#[cfg(feature = "gamepad")]
impl Gamepads {
    pub fn new() -> Self {
        // A missing input subsystem shouldn't stop the app launching —
        // headless CI and locked-down systems both hit this.
        let gilrs = gilrs::Gilrs::new().ok();
        Self {
            gilrs,
            stick_latched: false,
        }
    }

    /// True when at least one pad is connected. The UI uses this to decide
    /// whether to keep repainting and whether to show the button hints.
    pub fn any_connected(&self) -> bool {
        self.gilrs
            .as_ref()
            .is_some_and(|g| g.gamepads().any(|(_, pad)| pad.is_connected()))
    }

    /// The pressed RetroPad buttons and left-stick position, right now.
    ///
    /// Menu navigation is event-driven — one press, one move — but a game needs
    /// *held* state every frame: walking left means LEFT is down for a hundred
    /// frames, which produces exactly one gilrs event. So this polls current
    /// state rather than draining the queue.
    ///
    /// `gilrs` still needs its event queue pumped for the state to update, and
    /// `poll()` does that. Both are called each frame.
    pub fn retropad_state(&self) -> (u16, f32, f32) {
        let Some(gilrs) = self.gilrs.as_ref() else {
            return (0, 0.0, 0.0);
        };
        let mut mask = 0u16;
        let (mut x, mut y) = (0.0f32, 0.0f32);
        for (_, pad) in gilrs.gamepads() {
            if !pad.is_connected() {
                continue;
            }
            for b in [
                gilrs::Button::DPadUp,
                gilrs::Button::DPadDown,
                gilrs::Button::DPadLeft,
                gilrs::Button::DPadRight,
                gilrs::Button::South,
                gilrs::Button::East,
                gilrs::Button::West,
                gilrs::Button::North,
                gilrs::Button::LeftTrigger,
                gilrs::Button::RightTrigger,
                gilrs::Button::LeftTrigger2,
                gilrs::Button::RightTrigger2,
                gilrs::Button::LeftThumb,
                gilrs::Button::RightThumb,
                gilrs::Button::Start,
                gilrs::Button::Select,
            ] {
                if pad.is_pressed(b) {
                    if let Some(id) = crate::play::retropad_id_for_button(b) {
                        mask |= 1 << id;
                    }
                }
            }
            // Several pads connected means whichever moved last wins, which is
            // right for one player on a machine with a pad and a wheel plugged in.
            let ax = pad.value(gilrs::Axis::LeftStickX);
            let ay = pad.value(gilrs::Axis::LeftStickY);
            if ax.abs() > x.abs() {
                x = ax;
            }
            if ay.abs() > y.abs() {
                y = ay;
            }
        }
        (mask, x, y)
    }

    /// Drain pending controller events into navigation actions.
    pub fn poll(&mut self) -> Vec<NavAction> {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        while let Some(event) = gilrs.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(button, _) => {
                    if let Some(a) = button_to_action(button) {
                        actions.push(a);
                    }
                }
                gilrs::EventType::AxisChanged(gilrs::Axis::LeftStickY, value, _) => {
                    let (action, latched) = stick_y_to_action(value, self.stick_latched);
                    self.stick_latched = latched;
                    if let Some(a) = action {
                        actions.push(a);
                    }
                }
                _ => {}
            }
        }
        actions
    }
}

#[cfg(feature = "gamepad")]
impl Default for Gamepads {
    fn default() -> Self {
        Self::new()
    }
}

/// Stub used when the `gamepad` feature is disabled.
#[cfg(not(feature = "gamepad"))]
#[derive(Default)]
pub struct Gamepads;

#[cfg(not(feature = "gamepad"))]
impl Gamepads {
    pub fn new() -> Self {
        Self
    }
    pub fn any_connected(&self) -> bool {
        false
    }
    pub fn poll(&mut self) -> Vec<NavAction> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_and_vim_keys_both_move() {
        assert_eq!(key_to_action(egui::Key::ArrowDown), Some(NavAction::Down));
        assert_eq!(key_to_action(egui::Key::J), Some(NavAction::Down));
        assert_eq!(key_to_action(egui::Key::ArrowUp), Some(NavAction::Up));
        assert_eq!(key_to_action(egui::Key::K), Some(NavAction::Up));
    }

    #[test]
    fn enter_and_space_confirm() {
        assert_eq!(key_to_action(egui::Key::Enter), Some(NavAction::Confirm));
        assert_eq!(key_to_action(egui::Key::Space), Some(NavAction::Confirm));
    }

    #[test]
    fn unmapped_keys_do_nothing() {
        assert_eq!(key_to_action(egui::Key::F1), None);
        assert_eq!(key_to_action(egui::Key::Q), None);
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn face_buttons_follow_console_convention() {
        // South is the bottom button on every layout gilrs normalises.
        assert_eq!(
            button_to_action(gilrs::Button::South),
            Some(NavAction::Confirm)
        );
        assert_eq!(button_to_action(gilrs::Button::East), Some(NavAction::Back));
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn dpad_maps_to_the_same_actions_as_the_arrow_keys() {
        for (button, key) in [
            (gilrs::Button::DPadUp, egui::Key::ArrowUp),
            (gilrs::Button::DPadDown, egui::Key::ArrowDown),
            (gilrs::Button::DPadLeft, egui::Key::ArrowLeft),
            (gilrs::Button::DPadRight, egui::Key::ArrowRight),
        ] {
            assert_eq!(
                button_to_action(button),
                key_to_action(key),
                "{button:?} and {key:?} should agree"
            );
        }
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn unmapped_buttons_do_nothing() {
        assert_eq!(button_to_action(gilrs::Button::Mode), None);
        assert_eq!(button_to_action(gilrs::Button::North), None);
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn a_resting_stick_produces_nothing() {
        // Sticks rarely sit at exactly zero; small drift must be ignored.
        assert_eq!(stick_y_to_action(0.0, false), (None, false));
        assert_eq!(stick_y_to_action(0.3, false), (None, false));
        assert_eq!(stick_y_to_action(-0.59, false), (None, false));
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn a_held_stick_fires_once_not_every_frame() {
        let (action, latched) = stick_y_to_action(-0.9, false);
        assert_eq!(action, Some(NavAction::Down));
        assert!(latched);

        // Still held: no repeat.
        let (action, latched) = stick_y_to_action(-0.95, latched);
        assert_eq!(action, None);
        assert!(latched);

        // Released, then pushed again: fires once more.
        let (_, latched) = stick_y_to_action(0.0, latched);
        assert!(!latched);
        let (action, _) = stick_y_to_action(-0.9, latched);
        assert_eq!(action, Some(NavAction::Down));
    }

    #[cfg(feature = "gamepad")]
    #[test]
    fn pushing_the_stick_up_moves_up() {
        assert_eq!(stick_y_to_action(0.9, false).0, Some(NavAction::Up));
    }
}
