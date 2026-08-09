//! Testes ponta a ponta: sobem o binario de verdade e olham as janelas pelo Win32.
//!
//! Cobrem as tres regressoes que so aparecem com o app rodando:
//! - a janela nasce escondida (o eframe forca `set_visible(true)` depois do primeiro frame);
//! - o atalho normal abre o popup;
//! - o atalho rapido nao abre janela nenhuma;
//! - fechar a janela apenas esconde, sem matar o processo.
//!
//! Cada teste usa seu proprio `config.toml` (via `BETTER_ANSWER_CONFIG`) com atalhos improvaveis,
//! entao nao briga com a instancia real do usuario. A chave da API fica vazia de proposito: o erro
//! e instantaneo e nenhuma requisicao sai da maquina.
//!
//! Os atalhos disparam um Ctrl+C sintetico na janela em foco. Para nao mandar Ctrl+C para o
//! console que roda os testes (o que abortaria a suite) nem para a janela em que a pessoa estava
//! trabalhando, cada teste que dispara atalho poe em foco uma janela inerte propria.

#![cfg(windows)]

use std::io::Write as _;
use std::os::windows::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_F10, VK_F9, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows,
    GetForegroundWindow, GetMessageW, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TranslateMessage, MSG,
    WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

/// Sem console piscando quando o binario de debug (subsistema console) e lancado.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Atalhos globais e SendInput sao recursos da maquina inteira: um teste por vez.
static SERIAL: Mutex<()> = Mutex::new(());

fn serialize() -> MutexGuard<'static, ()> {
    // Um teste que falhou nao pode envenenar os outros.
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------- janelas

struct Found {
    pid: u32,
    windows: Vec<RECT>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Found) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == found.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
            found.windows.push(rect);
        }
    }
    TRUE
}

/// Janelas visiveis do processo que realmente ocupam espaco na tela.
///
/// Filtra os enxames de janelas auxiliares (1x1, 16x16) que winit, wgpu e a bandeja criam, e
/// tambem a janela estacionada fora do monitor enquanto o popup esta escondido.
fn onscreen_windows(pid: u32) -> Vec<(i32, i32)> {
    let mut found = Found {
        pid,
        windows: Vec::new(),
    };
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut found as *mut Found as isize)) };
    found
        .windows
        .into_iter()
        .filter(|r| r.left > -10_000 && r.top > -10_000)
        .map(|r| (r.right - r.left, r.bottom - r.top))
        .filter(|(w, h)| *w >= 300 && *h >= 300)
        .collect()
}

fn has_onscreen_window(pid: u32) -> bool {
    !onscreen_windows(pid).is_empty()
}

struct Pick {
    pid: u32,
    min_side: i32,
    hit: HWND,
}

unsafe extern "system" fn pick_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let pick = unsafe { &mut *(lparam.0 as *mut Pick) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != pick.pid || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return TRUE;
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
        && rect.right - rect.left >= pick.min_side
        && rect.bottom - rect.top >= pick.min_side
    {
        pick.hit = hwnd;
        return BOOL(0); // achou: para a enumeracao
    }
    TRUE
}

/// Primeira janela visivel do processo com pelo menos `min_side` de lado.
fn first_window(pid: u32, min_side: i32) -> Option<HWND> {
    let mut pick = Pick {
        pid,
        min_side,
        hit: HWND::default(),
    };
    let _ = unsafe { EnumWindows(Some(pick_window), LPARAM(&mut pick as *mut Pick as isize)) };
    (!pick.hit.is_invalid()).then_some(pick.hit)
}

/// Espera a condicao virar verdadeira. Devolve false se estourar o prazo.
fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Confirma que a condicao segue falsa durante toda a janela de tempo.
fn stays_false(window: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        if condition() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

// ---------------------------------------------------------------- teclado

fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
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

/// Dispara Ctrl+Alt+Shift+<tecla>, que e como os atalhos dos testes sao registrados.
fn press_test_hotkey(vk: VIRTUAL_KEY) {
    let down = KEYBD_EVENT_FLAGS(0);
    let inputs = [
        key(VK_CONTROL, down),
        key(VK_MENU, down),
        key(VK_SHIFT, down),
        key(vk, down),
        key(vk, KEYEVENTF_KEYUP),
        key(VK_SHIFT, KEYEVENTF_KEYUP),
        key(VK_MENU, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

// ---------------------------------------------------------------- processos

/// Mata o processo mesmo se o teste entrar em panico no meio.
struct Running(Child);

impl Running {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_config(name: &str, quick_hotkey: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("better-answer-e2e-{name}.toml"));
    let mut file = std::fs::File::create(&path).expect("criando config do teste");
    // api_key vazia: o erro e local e imediato, sem chamada de rede.
    write!(
        file,
        r#"api_key = ""
model = "e2e/nao-usado"
hotkey = "ctrl+alt+shift+f9"
quick_hotkey = "{quick_hotkey}"
temperature = 0.4
max_tokens = 256
signature = ""
extra_context = ""

[[tones]]
name = "Teste"
prompt = "reescreva"
"#
    )
    .expect("escrevendo config do teste");
    path
}

fn launch(name: &str, quick_hotkey: &str) -> Running {
    let config = write_config(name, quick_hotkey);
    let child = Command::new(env!("CARGO_BIN_EXE_better-answer"))
        .env("BETTER_ANSWER_CONFIG", &config)
        // A env var ganha do arquivo; removendo, a chave fica mesmo vazia.
        .env_remove("OPENROUTER_API_KEY")
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("subindo better-answer");
    Running(child)
}

/// Janela inerte criada pelo proprio teste, so para absorver o Ctrl+C sintetico da captura.
///
/// Depender de um app externo nao funciona: o `notepad.exe` do Windows 11 repassa o trabalho para
/// outro processo, entao o PID que o teste conhece nunca chega a ter janela.
struct Sink {
    hwnd: isize,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = unsafe { PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

unsafe extern "system" fn sink_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

fn focus_sink() -> Sink {
    let (tx, rx) = std::sync::mpsc::channel();

    // A janela precisa viver na thread que roda o loop de mensagens dela.
    let pump = std::thread::spawn(move || {
        let class = windows::core::w!("BetterAnswerE2ESink");
        let wnd_class = WNDCLASSW {
            lpfnWndProc: Some(sink_proc),
            lpszClassName: class,
            ..Default::default()
        };
        unsafe { RegisterClassW(&wnd_class) };

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                class,
                windows::core::w!("better-answer e2e"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                80,
                80,
                420,
                320,
                None,
                None,
                None,
                None,
            )
        };

        let hwnd = match hwnd {
            Ok(hwnd) => hwnd,
            Err(err) => {
                let _ = tx.send(Err(format!("{err}")));
                return;
            }
        };
        let _ = tx.send(Ok(hwnd.0 as isize));

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            let _ = unsafe { TranslateMessage(&msg) };
            unsafe { DispatchMessageW(&msg) };
        }
    });

    let hwnd = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a thread da janela alvo nao respondeu")
        .expect("nao consegui criar a janela alvo");

    let target = HWND(hwnd as *mut _);
    // SetForegroundWindow so obedece quem ja tem direito de foreground. Grudar a fila de entrada
    // na thread que hoje esta em primeiro plano e o caminho que funciona sem privilegio.
    let focused = wait_until(Duration::from_secs(5), || unsafe {
        let foreground = GetForegroundWindow();
        let other = GetWindowThreadProcessId(foreground, None);
        let me = GetCurrentThreadId();
        let attached = other != 0 && other != me && AttachThreadInput(other, me, true).as_bool();
        let _ = BringWindowToTop(target);
        let ok = SetForegroundWindow(target).as_bool();
        if attached {
            let _ = AttachThreadInput(other, me, false);
        }
        ok && GetForegroundWindow() == target
    });
    assert!(
        focused,
        "nao consegui por a janela alvo em foco — sem isso o Ctrl+C sintetico iria para a janela \
         em que a pessoa esta trabalhando"
    );
    std::thread::sleep(Duration::from_millis(300));

    Sink {
        hwnd,
        pump: Some(pump),
    }
}

// ---------------------------------------------------------------- testes

/// A regressao da "tela preta": o eframe reexibe a janela depois do primeiro frame, e o app
/// precisa desfazer isso. Sem a correcao, sobra um retangulo preto de 560x430 na tela.
#[test]
fn janela_nasce_escondida() {
    let _serial = serialize();
    let mut app = launch("hidden", "");

    // Tempo de sobra para o primeiro frame ser pintado e o eframe forcar o set_visible(true).
    let clean = stays_false(Duration::from_secs(5), || has_onscreen_window(app.pid()));
    assert!(app.is_alive(), "o app morreu sozinho durante o startup");
    assert!(
        clean,
        "janela apareceu na tela sem ninguem pedir: {:?}",
        onscreen_windows(app.pid())
    );
}

/// O atalho normal precisa trazer o popup para a tela.
#[test]
#[ignore = "sequestra o teclado e o foco da maquina: rode sozinho com --ignored"]
fn atalho_normal_abre_popup() {
    let _serial = serialize();
    let mut app = launch("popup", "");
    assert!(
        wait_until(Duration::from_secs(5), || app.is_alive()),
        "o app nao subiu"
    );
    std::thread::sleep(Duration::from_secs(2));

    let _sink = focus_sink();
    press_test_hotkey(VK_F9);

    assert!(
        wait_until(Duration::from_secs(6), || has_onscreen_window(app.pid())),
        "o popup nao apareceu depois do atalho"
    );
}

/// O atalho rapido e silencioso por contrato: nem no sucesso, nem no erro ele abre janela.
/// Aqui a chave esta vazia, entao o caminho de erro e o exercitado.
#[test]
#[ignore = "sequestra o teclado e o foco da maquina: rode sozinho com --ignored"]
fn atalho_rapido_nao_abre_janela() {
    let _serial = serialize();
    let mut app = launch("quick", "ctrl+alt+shift+f10");
    assert!(
        wait_until(Duration::from_secs(5), || app.is_alive()),
        "o app nao subiu"
    );
    std::thread::sleep(Duration::from_secs(2));

    let _sink = focus_sink();
    press_test_hotkey(VK_F10);

    assert!(
        stays_false(Duration::from_secs(6), || has_onscreen_window(app.pid())),
        "o atalho rapido abriu uma janela: {:?}",
        onscreen_windows(app.pid())
    );
    assert!(app.is_alive(), "o app morreu no caminho do atalho rapido");
}

/// Fechar a janela (X, Alt+F4) apenas esconde: quem encerra o app e o menu da bandeja.
#[test]
#[ignore = "sequestra o teclado e o foco da maquina: rode sozinho com --ignored"]
fn fechar_janela_apenas_esconde() {
    let _serial = serialize();
    let mut app = launch("close", "");
    assert!(
        wait_until(Duration::from_secs(5), || app.is_alive()),
        "o app nao subiu"
    );
    std::thread::sleep(Duration::from_secs(2));

    let _sink = focus_sink();
    press_test_hotkey(VK_F9);
    assert!(
        wait_until(Duration::from_secs(6), || has_onscreen_window(app.pid())),
        "o popup nao apareceu, nao da para testar o fechamento"
    );

    let pid = app.pid();
    let popup = first_window(pid, 300).expect("nao achei a janela do popup");
    let _ = unsafe { PostMessageW(Some(popup), WM_CLOSE, WPARAM(0), LPARAM(0)) };

    assert!(
        wait_until(Duration::from_secs(5), || !has_onscreen_window(pid)),
        "a janela continuou na tela depois do WM_CLOSE"
    );
    assert!(
        app.is_alive(),
        "o WM_CLOSE matou o processo — o X deveria apenas esconder"
    );
}
