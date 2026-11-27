//! The names a document gives to the crate's keys.
//!
//! The crate names a key by the physical position it sits at, where a document
//! names the key and the code separately and also states which side of the
//! keyboard it is on. A key that types nothing under the current layout has a
//! name and no character, which is what a shortcut matches against.

use keyboard_types::{Code, Key as DomKey, Location, NamedKey};

use crate::input::{Key, KeyLocation};

/// The key a document names the crate's key by.
pub(super) fn key_of(key: Key, shift: bool) -> DomKey {
    match character_of(key, shift) {
        Some(character) => DomKey::Character(character.to_owned()),
        None => DomKey::Named(named_key_of(key)),
    }
}

/// The physical key a crate key names, as a document reports it.
///
/// The crate names a key by where it sits on the keyboard and a document
/// names the same key by a code, so the two agree variant for variant. A key
/// the crate only carries the platform code of has no code of its own.
pub(super) fn code_of(key: Key) -> Code {
    crate::input::keymap::to_code(key).unwrap_or(Code::Unidentified)
}

/// The side of the keyboard a key is on, as a document reports it.
///
/// A document tells the left shift from the right one and names the numeric
/// keypad as its own side, which is the same set of places the crate states.
pub(super) fn location_of(location: KeyLocation) -> Location {
    match location {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    }
}
/// The character the crate's key types, if it types one.
///
/// The character is what the key types on a US layout, which is what a document
/// matches a shortcut against; a text field inserts from the text event
/// instead, so the two never disagree about the text a press produced. A layout
/// that types something else for the key still reaches the document through
/// that text: the key's physical position says nothing about what it means on
/// the keyboard in front of the user.
pub(super) fn character_of(key: Key, shift: bool) -> Option<&'static str> {
    Some(match (key, shift) {
        (Key::Space, _) => " ",
        (Key::A, false) => "a",
        (Key::A, true) => "A",
        (Key::B, false) => "b",
        (Key::B, true) => "B",
        (Key::C, false) => "c",
        (Key::C, true) => "C",
        (Key::D, false) => "d",
        (Key::D, true) => "D",
        (Key::E, false) => "e",
        (Key::E, true) => "E",
        (Key::F, false) => "f",
        (Key::F, true) => "F",
        (Key::G, false) => "g",
        (Key::G, true) => "G",
        (Key::H, false) => "h",
        (Key::H, true) => "H",
        (Key::I, false) => "i",
        (Key::I, true) => "I",
        (Key::J, false) => "j",
        (Key::J, true) => "J",
        (Key::K, false) => "k",
        (Key::K, true) => "K",
        (Key::L, false) => "l",
        (Key::L, true) => "L",
        (Key::M, false) => "m",
        (Key::M, true) => "M",
        (Key::N, false) => "n",
        (Key::N, true) => "N",
        (Key::O, false) => "o",
        (Key::O, true) => "O",
        (Key::P, false) => "p",
        (Key::P, true) => "P",
        (Key::Q, false) => "q",
        (Key::Q, true) => "Q",
        (Key::R, false) => "r",
        (Key::R, true) => "R",
        (Key::S, false) => "s",
        (Key::S, true) => "S",
        (Key::T, false) => "t",
        (Key::T, true) => "T",
        (Key::U, false) => "u",
        (Key::U, true) => "U",
        (Key::V, false) => "v",
        (Key::V, true) => "V",
        (Key::W, false) => "w",
        (Key::W, true) => "W",
        (Key::X, false) => "x",
        (Key::X, true) => "X",
        (Key::Y, false) => "y",
        (Key::Y, true) => "Y",
        (Key::Z, false) => "z",
        (Key::Z, true) => "Z",
        (Key::Num0, false) => "0",
        (Key::Num0, true) => ")",
        (Key::Num1, false) => "1",
        (Key::Num1, true) => "!",
        (Key::Num2, false) => "2",
        (Key::Num2, true) => "@",
        (Key::Num3, false) => "3",
        (Key::Num3, true) => "#",
        (Key::Num4, false) => "4",
        (Key::Num4, true) => "$",
        (Key::Num5, false) => "5",
        (Key::Num5, true) => "%",
        (Key::Num6, false) => "6",
        (Key::Num6, true) => "^",
        (Key::Num7, false) => "7",
        (Key::Num7, true) => "&",
        (Key::Num8, false) => "8",
        (Key::Num8, true) => "*",
        (Key::Num9, false) => "9",
        (Key::Num9, true) => "(",
        _ => return None,
    })
}

/// The name a document knows the crate's key by.
///
/// A key the crate has no name of its own for is still dispatched, unnamed,
/// rather than dropped: a page can react to the press even if neither side
/// knows what the key is called.
pub(super) fn named_key_of(key: Key) -> NamedKey {
    match key {
        Key::Enter => NamedKey::Enter,
        Key::Tab => NamedKey::Tab,
        Key::Backspace => NamedKey::Backspace,
        Key::Delete => NamedKey::Delete,
        Key::Escape => NamedKey::Escape,
        Key::ArrowUp => NamedKey::ArrowUp,
        Key::ArrowDown => NamedKey::ArrowDown,
        Key::ArrowLeft => NamedKey::ArrowLeft,
        Key::ArrowRight => NamedKey::ArrowRight,
        Key::Home => NamedKey::Home,
        Key::End => NamedKey::End,
        Key::PageUp => NamedKey::PageUp,
        Key::PageDown => NamedKey::PageDown,
        Key::F1 => NamedKey::F1,
        Key::F2 => NamedKey::F2,
        Key::F3 => NamedKey::F3,
        Key::F4 => NamedKey::F4,
        Key::F5 => NamedKey::F5,
        Key::F6 => NamedKey::F6,
        Key::F7 => NamedKey::F7,
        Key::F8 => NamedKey::F8,
        Key::F9 => NamedKey::F9,
        Key::F10 => NamedKey::F10,
        Key::F11 => NamedKey::F11,
        Key::F12 => NamedKey::F12,
        Key::F13 => NamedKey::F13,
        Key::F14 => NamedKey::F14,
        Key::F15 => NamedKey::F15,
        Key::F16 => NamedKey::F16,
        Key::F17 => NamedKey::F17,
        Key::F18 => NamedKey::F18,
        Key::F19 => NamedKey::F19,
        Key::F20 => NamedKey::F20,
        Key::F21 => NamedKey::F21,
        Key::F22 => NamedKey::F22,
        Key::F23 => NamedKey::F23,
        Key::F24 => NamedKey::F24,
        Key::F25 => NamedKey::F25,
        Key::F26 => NamedKey::F26,
        Key::F27 => NamedKey::F27,
        Key::F28 => NamedKey::F28,
        Key::F29 => NamedKey::F29,
        Key::F30 => NamedKey::F30,
        Key::F31 => NamedKey::F31,
        Key::F32 => NamedKey::F32,
        Key::F33 => NamedKey::F33,
        Key::F34 => NamedKey::F34,
        Key::F35 => NamedKey::F35,
        _ => NamedKey::Unidentified,
    }
}
