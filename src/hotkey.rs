//! Parse de atalho no formato "ctrl+alt+e", "win+b", "ctrl+shift+space".
//!
//! O resultado sai em virtual-key do Windows porque quem escuta e um hook WH_KEYBOARD_LL
//! (veja [`crate::hook`]), nao o `RegisterHotKey`. O `RegisterHotKey` nao serve aqui: combos
//! proprios do shell, como Win+B, sao tratados por ele antes de chegarem ao app, mesmo com o
//! registro bem-sucedido.

use anyhow::{anyhow, Result};

pub const CTRL: u8 = 1 << 0;
pub const ALT: u8 = 1 << 1;
pub const SHIFT: u8 = 1 << 2;
pub const WIN: u8 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// Combinacao exata de modificadores: ctrl+b nao dispara com ctrl+shift+b pressionado.
    pub mods: u8,
    pub vk: u16,
}

pub fn parse(spec: &str) -> Result<Shortcut> {
    let mut mods = 0u8;
    let mut vk: Option<u16> = None;

    for raw in spec.split('+') {
        let part = raw.trim().to_ascii_lowercase();
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "ctrl" | "control" => mods |= CTRL,
            "alt" | "option" => mods |= ALT,
            "shift" => mods |= SHIFT,
            "win" | "super" | "meta" | "cmd" => mods |= WIN,
            other => {
                if vk.is_some() {
                    return Err(anyhow!("hotkey '{spec}' tem mais de uma tecla principal"));
                }
                vk = Some(
                    parse_vk(other)
                        .ok_or_else(|| anyhow!("tecla '{other}' desconhecida em '{spec}'"))?,
                );
            }
        }
    }

    let vk = vk.ok_or_else(|| anyhow!("hotkey '{spec}' nao tem tecla principal"))?;
    if mods == 0 {
        return Err(anyhow!("hotkey '{spec}' precisa de pelo menos um modificador"));
    }
    Ok(Shortcut { mods, vk })
}

/// As teclas de pontuacao usam os codigos OEM, que seguem o layout US. Num teclado ABNT2 a tecla
/// fisica pode ser outra; letras, digitos e teclas nomeadas nao tem esse problema.
fn parse_vk(key: &str) -> Option<u16> {
    if key.len() == 1 {
        let ch = key.chars().next()?;
        if ch.is_ascii_lowercase() {
            return Some(0x41 + (ch as u16 - 'a' as u16));
        }
        if ch.is_ascii_digit() {
            return Some(0x30 + (ch as u16 - '0' as u16));
        }
    }

    if let Some(number) = key.strip_prefix('f') {
        if let Ok(index) = number.parse::<u16>() {
            if (1..=24).contains(&index) {
                return Some(0x70 + index - 1);
            }
        }
    }

    let vk = match key {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "backspace" => 0x08,
        "insert" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "," => 0xBC,
        "." => 0xBE,
        ";" => 0xBA,
        "'" => 0xDE,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        "/" => 0xBF,
        "-" => 0xBD,
        "=" => 0xBB,
        "`" => 0xC0,
        _ => return None,
    };
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_alt_letter() {
        let hk = parse("ctrl+alt+e").unwrap();
        assert_eq!(hk.mods, CTRL | ALT);
        assert_eq!(hk.vk, 0x45);
    }

    #[test]
    fn parses_the_shipped_defaults() {
        let open = parse("ctrl+b").unwrap();
        assert_eq!(open.mods, CTRL);
        assert_eq!(open.vk, 0x42);

        let quick = parse("ctrl+s").unwrap();
        assert_eq!(quick.mods, CTRL);
        assert_eq!(quick.vk, 0x53);

        let ticket = parse("ctrl+d").unwrap();
        assert_eq!(ticket.mods, CTRL);
        assert_eq!(ticket.vk, 0x44);

        // O roteamento e por atalho: os tres precisam ser distinguiveis.
        assert_ne!(open, quick);
        assert_ne!(open, ticket);
        assert_ne!(quick, ticket);
    }

    /// Combinacao exata: quem pede ctrl+b nao quer disparar em ctrl+shift+b.
    #[test]
    fn modifiers_are_exact() {
        assert_ne!(parse("ctrl+b").unwrap(), parse("ctrl+shift+b").unwrap());
    }

    #[test]
    fn parses_function_and_named_keys() {
        assert_eq!(parse("alt+f2").unwrap().vk, 0x71);
        assert_eq!(parse("ctrl+shift+space").unwrap().vk, 0x20);
        assert_eq!(parse("ctrl+f13").unwrap().vk, 0x7C);
    }

    #[test]
    fn rejects_missing_modifier() {
        assert!(parse("e").is_err());
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(parse("ctrl+banana").is_err());
    }

    #[test]
    fn rejects_out_of_range_function_key() {
        assert!(parse("ctrl+f25").is_err());
    }

    /// O Windows registra modificador + UMA tecla. Acorde tipo "ctrl+x depois 1" nao existe
    /// nesse formato, e falhar aqui e melhor do que registrar algo diferente do que foi escrito.
    #[test]
    fn rejects_chord_with_two_main_keys() {
        let err = parse("ctrl+x+1").unwrap_err().to_string();
        assert!(err.contains("mais de uma tecla principal"), "{err}");
    }
}
