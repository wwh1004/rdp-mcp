use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
    pub scancode: u8,
    pub extended: bool,
    pub shift: bool,
}

impl KeyStroke {
    const fn new(scancode: u8) -> Self {
        Self {
            scancode,
            extended: false,
            shift: false,
        }
    }

    const fn extended(scancode: u8) -> Self {
        Self {
            scancode,
            extended: true,
            shift: false,
        }
    }

    const fn shifted(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }
}

pub fn modifier(name: &str) -> Result<KeyStroke> {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Ok(KeyStroke::new(0x1d)),
        "alt" => Ok(KeyStroke::new(0x38)),
        "shift" => Ok(KeyStroke::new(0x2a)),
        "meta" | "win" | "windows" => Ok(KeyStroke::extended(0x5b)),
        _ => bail!("unsupported modifier: {name}"),
    }
}

pub fn key_stroke(key: &str) -> Result<KeyStroke> {
    let lowered = key.to_ascii_lowercase();
    let named = match lowered.as_str() {
        "ctrl" | "control" => Some(KeyStroke::new(0x1d)),
        "alt" => Some(KeyStroke::new(0x38)),
        "shift" => Some(KeyStroke::new(0x2a)),
        "meta" | "win" | "windows" => Some(KeyStroke::extended(0x5b)),
        "escape" | "esc" => Some(KeyStroke::new(0x01)),
        "backspace" => Some(KeyStroke::new(0x0e)),
        "tab" => Some(KeyStroke::new(0x0f)),
        "enter" | "return" => Some(KeyStroke::new(0x1c)),
        "space" => Some(KeyStroke::new(0x39)),
        "capslock" => Some(KeyStroke::new(0x3a)),
        "f1" => Some(KeyStroke::new(0x3b)),
        "f2" => Some(KeyStroke::new(0x3c)),
        "f3" => Some(KeyStroke::new(0x3d)),
        "f4" => Some(KeyStroke::new(0x3e)),
        "f5" => Some(KeyStroke::new(0x3f)),
        "f6" => Some(KeyStroke::new(0x40)),
        "f7" => Some(KeyStroke::new(0x41)),
        "f8" => Some(KeyStroke::new(0x42)),
        "f9" => Some(KeyStroke::new(0x43)),
        "f10" => Some(KeyStroke::new(0x44)),
        "f11" => Some(KeyStroke::new(0x57)),
        "f12" => Some(KeyStroke::new(0x58)),
        "numlock" => Some(KeyStroke::new(0x45)),
        "scrolllock" => Some(KeyStroke::new(0x46)),
        "home" => Some(KeyStroke::extended(0x47)),
        "arrowup" | "up" => Some(KeyStroke::extended(0x48)),
        "pageup" | "pgup" => Some(KeyStroke::extended(0x49)),
        "arrowleft" | "left" => Some(KeyStroke::extended(0x4b)),
        "arrowright" | "right" => Some(KeyStroke::extended(0x4d)),
        "end" => Some(KeyStroke::extended(0x4f)),
        "arrowdown" | "down" => Some(KeyStroke::extended(0x50)),
        "pagedown" | "pgdn" => Some(KeyStroke::extended(0x51)),
        "insert" => Some(KeyStroke::extended(0x52)),
        "delete" | "del" => Some(KeyStroke::extended(0x53)),
        _ => None,
    };
    if let Some(stroke) = named {
        return Ok(stroke);
    }

    let mut chars = key.chars();
    let character = chars
        .next()
        .ok_or_else(|| anyhow::anyhow!("key must not be empty"))?;
    if chars.next().is_some() || !character.is_ascii() {
        bail!("unsupported key name: {key}");
    }

    if let Some(base) = shifted_base(character) {
        return Ok(ascii_stroke(base)?.shifted());
    }

    let mut stroke = ascii_stroke(character.to_ascii_lowercase())?;
    if character.is_ascii_uppercase() {
        stroke = stroke.shifted();
    }
    Ok(stroke)
}

fn shifted_base(character: char) -> Option<char> {
    Some(match character {
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
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        ':' => ';',
        '"' => '\'',
        '~' => '`',
        '|' => '\\',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return None,
    })
}

fn ascii_stroke(character: char) -> Result<KeyStroke> {
    let scancode = match character {
        '1' => 0x02,
        '2' => 0x03,
        '3' => 0x04,
        '4' => 0x05,
        '5' => 0x06,
        '6' => 0x07,
        '7' => 0x08,
        '8' => 0x09,
        '9' => 0x0a,
        '0' => 0x0b,
        '-' => 0x0c,
        '=' => 0x0d,
        'q' => 0x10,
        'w' => 0x11,
        'e' => 0x12,
        'r' => 0x13,
        't' => 0x14,
        'y' => 0x15,
        'u' => 0x16,
        'i' => 0x17,
        'o' => 0x18,
        'p' => 0x19,
        '[' => 0x1a,
        ']' => 0x1b,
        'a' => 0x1e,
        's' => 0x1f,
        'd' => 0x20,
        'f' => 0x21,
        'g' => 0x22,
        'h' => 0x23,
        'j' => 0x24,
        'k' => 0x25,
        'l' => 0x26,
        ';' => 0x27,
        '\'' => 0x28,
        '`' => 0x29,
        '\\' => 0x2b,
        'z' => 0x2c,
        'x' => 0x2d,
        'c' => 0x2e,
        'v' => 0x2f,
        'b' => 0x30,
        'n' => 0x31,
        'm' => 0x32,
        ',' => 0x33,
        '.' => 0x34,
        '/' => 0x35,
        ' ' => 0x39,
        '\n' | '\r' => 0x1c,
        '\t' => 0x0f,
        _ => bail!("unsupported US-layout ASCII character: {character:?}"),
    };
    Ok(KeyStroke::new(scancode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_hello_world_characters() {
        for character in "Hello World!".chars() {
            key_stroke(&character.to_string()).unwrap();
        }
        assert_eq!(key_stroke(" ").unwrap(), KeyStroke::new(0x39));
        assert_eq!(key_stroke("Control").unwrap(), KeyStroke::new(0x1d));
        assert_eq!(key_stroke("Windows").unwrap(), KeyStroke::extended(0x5b));
        assert!(key_stroke("H").unwrap().shift);
        assert!(key_stroke("!").unwrap().shift);
    }
}
