//! The one list of named keys, and the two mappings derived from it.
//!
//! The crate names a key by where it sits on the keyboard. Two foreign enums
//! name the same keys: winit's, which a window event carries, and
//! `keyboard-types`', which a document reads. Both are total over the keys this
//! crate names, and both agree with [`Key`](crate::input::Key) variant for variant, so the list is
//! written once here and each direction is generated from it. Listing them
//! separately would let a key this crate names reach one side and not the
//! other.
//!
//! The mappings are internal: [`Key`](crate::input::Key) is the crate's own name for a key, and
//! these two only translate at the boundaries where a foreign enum is all the
//! caller has.

macro_rules! keymap {
    ($($key:ident => $winit:ident, $code:ident,)*) => {
        /// The crate's name for one of winit's key codes.
        ///
        /// `None` is a code this crate does not name — a numpad key, a media
        /// key, a key only some layouts have — which the caller keeps as
        /// [`Key::Other`](crate::input::Key::Other). Numpad digits and Enter are
        /// unnamed on purpose: the crate's [`Key::Num0`](crate::input::Key::Num0)
        /// and [`Key::Enter`](crate::input::Key::Enter) mean the top row and the
        /// main Enter, and collapsing the two would make a keypad control
        /// impossible.
        #[cfg(feature = "winit")]
        pub(crate) fn from_winit(code: ::winit::keyboard::KeyCode) -> Option<crate::input::Key> {
            Some(match code {
                $(::winit::keyboard::KeyCode::$winit => crate::input::Key::$key,)*
                _ => return None,
            })
        }

        /// The code a document reports for one of the crate's keys.
        ///
        /// `None` is [`Key::Other`](crate::input::Key::Other), which carries only a
        /// platform code and so has no code of its own.
        #[cfg(feature = "blitz")]
        pub(crate) fn to_code(key: crate::input::Key) -> Option<::keyboard_types::Code> {
            Some(match key {
                $(crate::input::Key::$key => ::keyboard_types::Code::$code,)*
                crate::input::Key::Other(_) => return None,
            })
        }
    };
}

keymap! {
    Escape => Escape, Escape,
    Space => Space, Space,
    Enter => Enter, Enter,
    Tab => Tab, Tab,
    Backspace => Backspace, Backspace,
    Delete => Delete, Delete,
    ArrowUp => ArrowUp, ArrowUp,
    ArrowDown => ArrowDown, ArrowDown,
    ArrowLeft => ArrowLeft, ArrowLeft,
    ArrowRight => ArrowRight, ArrowRight,
    Home => Home, Home,
    End => End, End,
    PageUp => PageUp, PageUp,
    PageDown => PageDown, PageDown,
    A => KeyA, KeyA,
    B => KeyB, KeyB,
    C => KeyC, KeyC,
    D => KeyD, KeyD,
    E => KeyE, KeyE,
    F => KeyF, KeyF,
    G => KeyG, KeyG,
    H => KeyH, KeyH,
    I => KeyI, KeyI,
    J => KeyJ, KeyJ,
    K => KeyK, KeyK,
    L => KeyL, KeyL,
    M => KeyM, KeyM,
    N => KeyN, KeyN,
    O => KeyO, KeyO,
    P => KeyP, KeyP,
    Q => KeyQ, KeyQ,
    R => KeyR, KeyR,
    S => KeyS, KeyS,
    T => KeyT, KeyT,
    U => KeyU, KeyU,
    V => KeyV, KeyV,
    W => KeyW, KeyW,
    X => KeyX, KeyX,
    Y => KeyY, KeyY,
    Z => KeyZ, KeyZ,
    Num0 => Digit0, Digit0,
    Num1 => Digit1, Digit1,
    Num2 => Digit2, Digit2,
    Num3 => Digit3, Digit3,
    Num4 => Digit4, Digit4,
    Num5 => Digit5, Digit5,
    Num6 => Digit6, Digit6,
    Num7 => Digit7, Digit7,
    Num8 => Digit8, Digit8,
    Num9 => Digit9, Digit9,
    F1 => F1, F1,
    F2 => F2, F2,
    F3 => F3, F3,
    F4 => F4, F4,
    F5 => F5, F5,
    F6 => F6, F6,
    F7 => F7, F7,
    F8 => F8, F8,
    F9 => F9, F9,
    F10 => F10, F10,
    F11 => F11, F11,
    F12 => F12, F12,
    F13 => F13, F13,
    F14 => F14, F14,
    F15 => F15, F15,
    F16 => F16, F16,
    F17 => F17, F17,
    F18 => F18, F18,
    F19 => F19, F19,
    F20 => F20, F20,
    F21 => F21, F21,
    F22 => F22, F22,
    F23 => F23, F23,
    F24 => F24, F24,
    F25 => F25, F25,
    F26 => F26, F26,
    F27 => F27, F27,
    F28 => F28, F28,
    F29 => F29, F29,
    F30 => F30, F30,
    F31 => F31, F31,
    F32 => F32, F32,
    F33 => F33, F33,
    F34 => F34, F34,
    F35 => F35, F35,
}
