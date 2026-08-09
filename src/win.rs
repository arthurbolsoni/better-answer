//! Integracao com o Windows: ler a selecao de qualquer janela e devolver o texto pronto.
//!
//! A selecao e capturada simulando Ctrl+C e observando o clipboard, que e o unico caminho
//! que funciona em qualquer app (Chrome, Outlook, Teams, terminal, Electron...).

use anyhow::{anyhow, Result};
use std::time::Duration;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, SetForegroundWindow};

const VK_C: VIRTUAL_KEY = VIRTUAL_KEY(0x43);
const VK_V: VIRTUAL_KEY = VIRTUAL_KEY(0x56);

/// HWND nao e Send; o app guarda o handle como isize para passar entre threads.
pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

pub fn focus_window(handle: isize) {
    if handle == 0 {
        return;
    }
    unsafe {
        let _ = SetForegroundWindow(HWND(handle as *mut _));
    }
}

pub fn cursor_pos() -> (i32, i32) {
    let mut point = POINT::default();
    unsafe {
        if GetCursorPos(&mut point).is_ok() {
            (point.x, point.y)
        } else {
            (0, 0)
        }
    }
}

fn key_event(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// O atalho global e disparado com os modificadores ainda pressionados fisicamente.
/// Sem soltar antes, o Ctrl+C sintetico vira Ctrl+Alt+C na janela alvo.
fn release_modifiers() {
    let mut up = Vec::new();
    for vk in [VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_CONTROL] {
        if is_down(vk) {
            up.push(key_event(vk, KEYEVENTF_KEYUP));
        }
    }
    if !up.is_empty() {
        send(&up);
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn press_with_ctrl(vk: VIRTUAL_KEY) {
    send(&[
        key_event(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_event(vk, KEYBD_EVENT_FLAGS(0)),
        key_event(vk, KEYEVENTF_KEYUP),
        key_event(VK_CONTROL, KEYEVENTF_KEYUP),
    ]);
}

fn clipboard_text() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    clipboard.get_text().ok()
}

pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("clipboard: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| anyhow!("clipboard: {e}"))
}

pub struct Capture {
    /// Texto selecionado no app de origem.
    pub text: String,
    /// Clipboard anterior, para restaurar caso o usuario cancele.
    pub previous_clipboard: Option<String>,
}

/// Copia a selecao da janela em foco. `None` quando nada foi selecionado.
pub fn capture_selection() -> Capture {
    let previous_clipboard = clipboard_text();
    let seq_before = unsafe { GetClipboardSequenceNumber() };

    release_modifiers();
    press_with_ctrl(VK_C);

    // O app alvo responde ao Ctrl+C de forma assincrona; espera ate ~600ms pela mudanca.
    let mut text = String::new();
    for _ in 0..24 {
        std::thread::sleep(Duration::from_millis(25));
        let seq_now = unsafe { GetClipboardSequenceNumber() };
        if seq_now != seq_before {
            if let Some(copied) = clipboard_text() {
                text = copied;
            }
            break;
        }
    }

    Capture {
        text: text.trim().to_string(),
        previous_clipboard,
    }
}

/// Cola o texto na janela `handle`, devolvendo o foco para ela antes.
pub fn paste_into(handle: isize, text: &str) -> Result<()> {
    set_clipboard_text(text)?;
    focus_window(handle);
    std::thread::sleep(Duration::from_millis(80));
    release_modifiers();
    press_with_ctrl(VK_V);
    Ok(())
}
