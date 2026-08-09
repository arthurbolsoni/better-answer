//! Atalhos globais por hook de teclado de baixo nivel (`WH_KEYBOARD_LL`).
//!
//! O `RegisterHotKey` nao da conta: combos que o proprio shell usa — Win+B foca a area de
//! notificacao, Win+E abre o explorer — sao tratados por ele antes de chegarem ao app, mesmo com o
//! registro bem-sucedido. Um hook de baixo nivel ve a tecla antes de todo mundo e pode engoli-la,
//! que e como os utilitarios de atalho fazem.
//!
//! O hook roda na thread que o instalou e o Windows o remove sem avisar se ela demorar demais para
//! responder (`LowLevelHooksTimeout`). Por isso ele mora numa thread dedicada que so bombeia
//! mensagens: o trabalho pesado (capturar selecao, falar com a LLM) acontece em outra ponta do
//! canal.

use crate::hotkey::{Shortcut, ALT, CTRL, SHIFT, WIN};
use anyhow::{anyhow, Result};
use std::cell::RefCell;
use std::sync::mpsc::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
};

/// Atalho registrado e o identificador que volta pelo canal quando ele dispara.
pub struct Binding {
    pub id: u32,
    pub shortcut: Shortcut,
}

struct State {
    bindings: Vec<Binding>,
    tx: Sender<u32>,
    /// Tecla cujo keydown foi engolido: o keyup dela tambem precisa sumir, senao a janela em foco
    /// recebe a soltura de uma tecla que nunca foi pressionada para ela.
    swallowing: Option<u16>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Sobe a thread do hook. O `Sender` recebe o id do atalho a cada disparo.
pub fn spawn(bindings: Vec<Binding>, tx: Sender<u32>) -> Result<()> {
    if bindings.is_empty() {
        return Ok(());
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(move || {
            STATE.with(|state| {
                *state.borrow_mut() = Some(State {
                    bindings,
                    tx,
                    swallowing: None,
                });
            });

            let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) };
            let installed = hook.is_ok();
            let _ = ready_tx.send(match &hook {
                Ok(_) => Ok(()),
                Err(err) => Err(format!("{err}")),
            });
            if !installed {
                return;
            }

            // O hook so e chamado enquanto esta thread bombeia mensagens.
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                let _ = unsafe { TranslateMessage(&msg) };
                unsafe { DispatchMessageW(&msg) };
            }
        })
        .map_err(|err| anyhow!("nao subi a thread do hook: {err}"))?;

    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow!("SetWindowsHookEx falhou: {err}")),
        Err(_) => Err(anyhow!("a thread do hook nao respondeu")),
    }
}

fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

fn current_mods() -> u8 {
    let mut mods = 0;
    if is_down(VK_CONTROL) {
        mods |= CTRL;
    }
    if is_down(VK_MENU) {
        mods |= ALT;
    }
    if is_down(VK_SHIFT) {
        mods |= SHIFT;
    }
    if is_down(VK_LWIN) || is_down(VK_RWIN) {
        mods |= WIN;
    }
    mods
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Ignora o que o proprio app injeta (o Ctrl+C da captura, a mascara de Alt/Win), senao o
        // hook reagiria as suas proprias teclas.
        let injected = event.flags.0 & LLKHF_INJECTED.0 != 0;

        if !injected {
            let message = wparam.0 as u32;
            let vk = event.vkCode as u16;
            let swallow = STATE.with(|state| {
                let mut state = state.borrow_mut();
                let Some(state) = state.as_mut() else {
                    return false;
                };

                if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
                    let mods = current_mods();
                    if let Some(binding) = state
                        .bindings
                        .iter()
                        .find(|b| b.shortcut.vk == vk && b.shortcut.mods == mods)
                    {
                        // O canal e so um aviso: quem faz o trabalho e a outra thread, porque um
                        // hook lento e desinstalado pelo Windows.
                        let _ = state.tx.send(binding.id);
                        state.swallowing = Some(vk);
                        return true;
                    }
                } else if (message == WM_KEYUP || message == WM_SYSKEYUP)
                    && state.swallowing == Some(vk)
                {
                    state.swallowing = None;
                    return true;
                }
                false
            });

            if swallow {
                return LRESULT(1);
            }
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
