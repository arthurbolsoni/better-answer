// Em release o app vive na bandeja; sem console piscando ao abrir.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod hotkey;
mod llm;
mod win;

use config::Config;

fn main() -> eframe::Result<()> {
    let (cfg, cfg_error) = match Config::load_or_create() {
        Ok(cfg) => (cfg, None),
        Err(err) => (Config::default(), Some(format!("config: {err:#}"))),
    };

    // Modo linha de comando, util para testar a chave/modelo sem depender do atalho.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        attach_console();
    }
    match args.first().map(String::as_str) {
        Some("--config-path") => {
            match Config::path() {
                Ok(path) => println!("{}", path.display()),
                Err(err) => eprintln!("erro: {err:#}"),
            }
            return Ok(());
        }
        Some("--improve") => {
            let text = args[1..].join(" ");
            cli_improve(&cfg, &text);
            return Ok(());
        }
        Some(other) => {
            eprintln!("uso: better-answer [--improve <texto> | --config-path]");
            eprintln!("argumento desconhecido: {other}");
            return Ok(());
        }
        None => {}
    }

    // `with_visible(false)` nao basta: o eframe forca `set_visible(true)` depois de pintar o
    // primeiro frame (epi_integration::post_rendering). Nascer fora da tela garante que esse
    // frame forcado nao apareca; o App reafirma `Visible(false)` e so entao posiciona.
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("better-answer")
        .with_inner_size([app::WINDOW_W, app::WINDOW_H])
        .with_min_inner_size([420.0, 320.0])
        .with_position(app::OFFSCREEN)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_visible(false);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "better-answer",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg, cfg_error)))),
    )
}

/// Em release o binario e GUI-only; reanexar ao console do shell faz o modo CLI imprimir.
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn cli_improve(cfg: &Config, text: &str) {
    use std::io::Write as _;

    if text.trim().is_empty() {
        eprintln!("uso: better-answer --improve <texto>");
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let tone = cfg.tone(0).clone();
    llm::stream_improve(cfg, &tone, text, "", 0, &tx, || {});
    drop(tx);

    let mut stdout = std::io::stdout();
    for (_, msg) in rx {
        match msg {
            llm::Msg::Delta(delta) => {
                let _ = write!(stdout, "{delta}");
                let _ = stdout.flush();
            }
            llm::Msg::Done => {
                let _ = writeln!(stdout);
            }
            llm::Msg::Error(err) => eprintln!("\nerro: {err}"),
        }
    }
}
