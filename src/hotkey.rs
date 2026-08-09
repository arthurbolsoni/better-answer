use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// Parseia "ctrl+alt+e", "ctrl+shift+space", "alt+f2" em um HotKey.
pub fn parse(spec: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;

    for raw in spec.split('+') {
        let part = raw.trim().to_ascii_lowercase();
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "win" | "super" | "meta" | "cmd" => mods |= Modifiers::META,
            other => {
                if code.is_some() {
                    return Err(anyhow!("hotkey '{spec}' tem mais de uma tecla principal"));
                }
                code = Some(parse_code(other).ok_or_else(|| anyhow!("tecla '{other}' desconhecida em '{spec}'"))?);
            }
        }
    }

    let code = code.ok_or_else(|| anyhow!("hotkey '{spec}' nao tem tecla principal"))?;
    if mods.is_empty() {
        return Err(anyhow!("hotkey '{spec}' precisa de pelo menos um modificador"));
    }
    Ok(HotKey::new(Some(mods), code))
}

fn parse_code(key: &str) -> Option<Code> {
    let code = match key {
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "insert" => Code::Insert,
        "delete" | "del" => Code::Delete,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "," => Code::Comma,
        "." => Code::Period,
        ";" => Code::Semicolon,
        "'" => Code::Quote,
        "[" => Code::BracketLeft,
        "]" => Code::BracketRight,
        "\\" => Code::Backslash,
        "/" => Code::Slash,
        "-" => Code::Minus,
        "=" => Code::Equal,
        "`" => Code::Backquote,
        _ => return None,
    };
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_alt_letter() {
        let hk = parse("ctrl+alt+e").unwrap();
        assert_eq!(hk.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(hk.key, Code::KeyE);
    }

    #[test]
    fn rejects_missing_modifier() {
        assert!(parse("e").is_err());
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(parse("ctrl+banana").is_err());
    }
}
