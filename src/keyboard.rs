use crate::config::Target;
use anyhow::{Result, bail};

pub enum Input {
    Optical(Vec<[u8; 16]>),
    Ps2(Vec<Vec<u8>>),
}

fn base(c: char) -> (char, bool) {
    if c.is_ascii_uppercase() {
        return (c.to_ascii_lowercase(), true);
    }
    let plain = match c {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '~' => '`',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return (c, false),
    };
    (plain, true)
}

pub fn empty_snapshot() -> [u8; 16] {
    let mut frame = [0; 16];
    for row in 0..8 {
        frame[row * 2] = (row as u8) << 4;
    }
    frame
}

fn set(frame: &mut [u8; 16], index: usize) {
    frame[(7 - index / 8) * 2 + 1] |= 1 << (7 - index % 8);
}

fn optical_character(c: char) -> Result<(usize, bool)> {
    let (c, shift) = base(c);
    let index = match c {
        'q' => 1,
        ' ' => 3,
        '2' => 4,
        '1' => 7,
        '/' => 8,
        '\t' => 9,
        '\'' => 13,
        ']' => 14,
        '=' => 15,
        ',' => 16,
        '[' => 17,
        ';' => 18,
        '.' => 19,
        'l' => 21,
        'p' => 22,
        '-' => 23,
        'n' => 24,
        'o' => 25,
        'k' => 26,
        'm' => 27,
        '0' => 28,
        'j' => 29,
        'i' => 30,
        '9' => 31,
        'v' => 32,
        'u' => 33,
        'h' => 34,
        'b' => 35,
        '8' => 36,
        'g' => 37,
        'y' => 38,
        '7' => 39,
        'x' => 40,
        't' => 41,
        'f' => 42,
        'c' => 43,
        '6' => 44,
        'd' => 45,
        'r' => 46,
        '5' => 47,
        'e' => 49,
        's' => 50,
        'z' => 51,
        '4' => 52,
        'a' => 53,
        'w' => 54,
        '3' => 55,
        '\n' | '\r' => 62,
        _ => bail!("character {c:?} has no K2 optical keyboard mapping"),
    };
    Ok((index, shift))
}

fn optical_key(name: &str) -> Result<[[u8; 16]; 2]> {
    let (index, shift) = match name.to_ascii_lowercase().as_str() {
        "run-stop" | "runstop" | "pause" | "break" => (0, false),
        "backspace" => (6, false),
        "tab" => (9, false),
        "space" => (3, false),
        "home" => (12, false),
        "up" => (56, false),
        "left" => (61, false),
        "enter" => (62, false),
        _ => optical_character(single(name)?)?,
    };
    let mut frame = empty_snapshot();
    set(&mut frame, index);
    if shift {
        set(&mut frame, 48);
    }
    Ok([frame, empty_snapshot()])
}

fn single(value: &str) -> Result<char> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => bail!("unknown key {value:?}"),
    }
}

fn ps2_character(c: char) -> Result<Vec<u8>> {
    let (c, shift) = base(c);
    let code = match c {
        'a' => 0x1C,
        'b' => 0x32,
        'c' => 0x21,
        'd' => 0x23,
        'e' => 0x24,
        'f' => 0x2B,
        'g' => 0x34,
        'h' => 0x33,
        'i' => 0x43,
        'j' => 0x3B,
        'k' => 0x42,
        'l' => 0x4B,
        'm' => 0x3A,
        'n' => 0x31,
        'o' => 0x44,
        'p' => 0x4D,
        'q' => 0x15,
        'r' => 0x2D,
        's' => 0x1B,
        't' => 0x2C,
        'u' => 0x3C,
        'v' => 0x2A,
        'w' => 0x1D,
        'x' => 0x22,
        'y' => 0x35,
        'z' => 0x1A,
        '0' => 0x45,
        '1' => 0x16,
        '2' => 0x1E,
        '3' => 0x26,
        '4' => 0x25,
        '5' => 0x2E,
        '6' => 0x36,
        '7' => 0x3D,
        '8' => 0x3E,
        '9' => 0x46,
        ' ' => 0x29,
        '\t' => 0x0D,
        '\n' | '\r' => 0x5A,
        '`' => 0x0E,
        '-' => 0x4E,
        '=' => 0x55,
        '[' => 0x54,
        ']' => 0x5B,
        '\\' => 0x5D,
        ';' => 0x4C,
        '\'' => 0x52,
        ',' => 0x41,
        '.' => 0x49,
        '/' => 0x4A,
        _ => bail!("character {c:?} has no PS/2 Set-2 mapping"),
    };
    Ok(if shift {
        vec![0x12, code, 0xF0, code, 0xF0, 0x12]
    } else {
        vec![code, 0xF0, code]
    })
}

fn ps2_key(name: &str) -> Result<Vec<u8>> {
    let code = match name.to_ascii_lowercase().as_str() {
        "enter" => 0x5A,
        "escape" | "esc" => 0x76,
        "backspace" => 0x66,
        "tab" => 0x0D,
        "space" => 0x29,
        "run-stop" | "runstop" | "pause" | "break" => {
            return Ok(vec![0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77]);
        }
        "up" | "down" | "left" | "right" | "home" => {
            let code = match name.to_ascii_lowercase().as_str() {
                "up" => 0x75,
                "down" => 0x72,
                "left" => 0x6B,
                "right" => 0x74,
                _ => 0x6C,
            };
            return Ok(vec![0xE0, code, 0xE0, 0xF0, code]);
        }
        _ => return ps2_character(single(name)?),
    };
    Ok(vec![code, 0xF0, code])
}

pub fn keys(target: Target, names: &[String]) -> Result<Input> {
    match target {
        Target::K2 => {
            let mut frames = Vec::new();
            for name in names {
                frames.extend(optical_key(name)?);
            }
            Ok(Input::Optical(frames))
        }
        Target::Jr2 => Ok(Input::Ps2(
            names
                .iter()
                .map(|name| ps2_key(name))
                .collect::<Result<_>>()?,
        )),
    }
}

pub fn text(target: Target, value: &str) -> Result<Input> {
    let names: Vec<_> = value.chars().map(|c| c.to_string()).collect();
    keys(target, &names)
}
