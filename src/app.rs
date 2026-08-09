use crate::config::Config;
use crate::llm::{self, Msg};
use crate::win;
use eframe::egui;
use egui::{FontFamily, FontId, TextStyle};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const WINDOW_W: f32 = 560.0;
pub const WINDOW_H: f32 = 430.0;

/// Onde a janela estaciona enquanto esta escondida: fora de qualquer monitor.
///
/// Nao da para confiar so no `Visible(false)`. O eframe forca `set_visible(true)` assim que o
/// primeiro frame e pintado (`epi_integration::post_rendering`), entao a janela sempre aparece
/// uma vez. Estacionada fora da tela, esse frame forcado nao chega aos olhos de ninguem.
pub const OFFSCREEN: [f32; 2] = [-32000.0, -32000.0];

const BG: egui::Color32 = egui::Color32::from_rgb(14, 15, 19);
const PANEL: egui::Color32 = egui::Color32::from_rgb(23, 25, 31);
const PANEL_HI: egui::Color32 = egui::Color32::from_rgb(33, 36, 45);
const BORDER: egui::Color32 = egui::Color32::from_rgb(45, 49, 60);
const TEXT: egui::Color32 = egui::Color32::from_rgb(228, 231, 238);
const MUTED: egui::Color32 = egui::Color32::from_rgb(124, 131, 147);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(122, 162, 255);
const DANGER: egui::Color32 = egui::Color32::from_rgb(242, 118, 118);

/// Se o `Close` do menu nao derrubar o loop nesse prazo, o processo sai na marra.
const QUIT_GRACE: Duration = Duration::from_millis(1500);

enum Status {
    Idle,
    NoSelection,
    Loading,
    Done,
    Error(String),
}

/// Pedido vindo da thread do atalho global.
struct ShowRequest {
    hwnd: isize,
    text: String,
    previous_clipboard: Option<String>,
    cursor: (i32, i32),
}

enum HotkeyMsg {
    /// Atalho normal: abre o popup.
    Show(Box<ShowRequest>),
    /// Atalho rapido: melhora e cola sem abrir janela nenhuma.
    Quick(Box<ShowRequest>),
    Hide,
}

/// Trabalho do modo rapido. Nao toca no estado do popup: acumula em silencio e cola no fim.
struct QuickJob {
    hwnd: isize,
    generation: u64,
    buffer: String,
    previous_clipboard: Option<String>,
}

pub struct App {
    cfg: Config,
    /// Copia editavel enquanto o painel de configuracao esta aberto.
    draft: Config,
    tone_index: usize,
    original: String,
    instruction: String,
    output: String,
    status: Status,
    generation: u64,
    quick_generation: u64,
    visible: Arc<AtomicBool>,
    /// `ui()` so roda quando a janela esta visivel para o SO. Serve de sensor de dessincronia.
    ui_ran: bool,
    quitting: Option<Instant>,
    target_hwnd: isize,
    previous_clipboard: Option<String>,
    show_settings: bool,
    show_original: bool,
    toast: Option<(String, Instant)>,
    /// Ultimo resultado do modo rapido, que nao tem janela para reportar sozinho.
    quick_note: Option<(String, bool)>,
    quick: Option<QuickJob>,

    llm_tx: Sender<(u64, Msg)>,
    llm_rx: Receiver<(u64, Msg)>,
    quick_tx: Sender<(u64, Msg)>,
    quick_rx: Receiver<(u64, Msg)>,
    hotkey_rx: Receiver<HotkeyMsg>,

    tray_open_id: MenuId,
    tray_quit_id: MenuId,
    tray: Option<TrayIcon>,
    _hotkeys: Option<GlobalHotKeyManager>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, cfg_error: Option<String>) -> Self {
        let ctx = cc.egui_ctx.clone();
        style(&ctx);

        let (llm_tx, llm_rx) = channel();
        let (quick_tx, quick_rx) = channel();
        let (hotkey_tx, hotkey_rx) = channel();
        let visible = Arc::new(AtomicBool::new(false));

        let mut status = match cfg_error {
            Some(err) => Status::Error(err),
            None => Status::Idle,
        };

        // O manager precisa nascer na thread que roda o event loop win32 (a main).
        let registration = register_hotkeys(&cfg);
        if let Some(err) = registration.error {
            status = Status::Error(err);
        }

        spawn_hotkey_listener(
            ctx.clone(),
            hotkey_tx,
            visible.clone(),
            registration.open_id,
            registration.quick_id,
        );

        let (tray, tray_open_id, tray_quit_id) = build_tray(&cfg);

        Self {
            draft: cfg.clone(),
            cfg,
            tone_index: 0,
            original: String::new(),
            instruction: String::new(),
            output: String::new(),
            status,
            generation: 0,
            quick_generation: 0,
            visible,
            ui_ran: false,
            quitting: None,
            target_hwnd: 0,
            previous_clipboard: None,
            show_settings: false,
            show_original: false,
            toast: None,
            quick_note: None,
            quick: None,
            llm_tx,
            llm_rx,
            quick_tx,
            quick_rx,
            hotkey_rx,
            tray_open_id,
            tray_quit_id,
            tray,
            _hotkeys: registration.manager,
        }
    }

    fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    /// Esconder de verdade: some da tela E sai da area visivel, porque o eframe reexibe a
    /// janela sozinho depois de pintar.
    fn park_offscreen(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            OFFSCREEN[0],
            OFFSCREEN[1],
        )));
    }

    fn show_window(&mut self, ctx: &egui::Context, cursor: Option<(i32, i32)>) {
        let ppp = ctx.pixels_per_point().max(0.1);
        let monitor = ctx.input(|i| i.viewport().monitor_size);

        let pos = match cursor {
            Some((x, y)) => {
                let mut px = x as f32 / ppp + 12.0;
                let mut py = y as f32 / ppp + 12.0;
                if let Some(monitor) = monitor {
                    px = px.min(monitor.x - WINDOW_W - 8.0).max(8.0);
                    py = py.min(monitor.y - WINDOW_H - 8.0).max(8.0);
                }
                egui::pos2(px, py)
            }
            None => match monitor {
                Some(monitor) => egui::pos2((monitor.x - WINDOW_W) / 2.0, (monitor.y - WINDOW_H) / 2.0),
                None => egui::pos2(200.0, 200.0),
            },
        };

        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.visible.store(true, Ordering::SeqCst);
        self.ui_ran = false;
    }

    fn hide_window(&mut self, ctx: &egui::Context, restore_clipboard: bool) {
        if restore_clipboard {
            if let Some(previous) = self.previous_clipboard.take() {
                let _ = win::set_clipboard_text(&previous);
            }
        }
        self.previous_clipboard = None;
        // Uma geracao nova invalida o stream do popup em andamento (o modo rapido tem a sua).
        self.generation += 1;
        self.show_settings = false;
        self.visible.store(false, Ordering::SeqCst);
        self.ui_ran = false;
        self.park_offscreen(ctx);
    }

    fn start_generation(&mut self, ctx: &egui::Context) {
        if self.original.trim().is_empty() {
            self.status = Status::NoSelection;
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.output.clear();
        self.quick_note = None;
        self.status = Status::Loading;

        let cfg = self.cfg.clone();
        let tone = cfg.tone(self.tone_index).clone();
        let input = self.original.clone();
        let instruction = self.instruction.clone();
        let tx = self.llm_tx.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let repaint = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            llm::stream_improve(&cfg, &tone, &input, &instruction, generation, &tx, repaint);
        });
    }

    /// Modo rapido: nenhuma janela abre, nem no erro. O retorno vai para o tooltip da bandeja e
    /// para o aviso mostrado da proxima vez que o popup abrir.
    fn start_quick(&mut self, ctx: &egui::Context, req: ShowRequest) {
        let text = req.text.trim().to_string();
        if text.is_empty() {
            restore_clipboard(req.previous_clipboard);
            self.note_quick("nada selecionado", true);
            return;
        }

        self.quick_generation += 1;
        let generation = self.quick_generation;
        self.quick = Some(QuickJob {
            hwnd: req.hwnd,
            generation,
            buffer: String::new(),
            previous_clipboard: req.previous_clipboard,
        });
        self.note_quick("melhorando...", false);

        let cfg = self.cfg.clone();
        let tone = cfg.tone(self.tone_index).clone();
        let tx = self.quick_tx.clone();
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let repaint = {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            };
            llm::stream_improve(&cfg, &tone, &text, "", generation, &tx, repaint);
        });
    }

    fn note_quick(&mut self, message: &str, is_error: bool) {
        self.quick_note = Some((message.to_string(), is_error));
        self.sync_tray_tooltip();
    }

    fn sync_tray_tooltip(&self) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let quick = if self.cfg.quick_hotkey.trim().is_empty() {
            "desligado".to_string()
        } else {
            self.cfg.quick_hotkey.clone()
        };
        let mut tip = format!("better-answer\n{} — popup\n{} — rápido", self.cfg.hotkey, quick);
        if let Some((message, is_error)) = &self.quick_note {
            let prefix = if *is_error { "erro" } else { "rápido" };
            tip.push_str(&format!("\n{prefix}: {message}"));
        }
        let _ = tray.set_tooltip(Some(tip));
    }

    fn drain_channels(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.hotkey_rx.try_recv() {
            match msg {
                HotkeyMsg::Hide => self.hide_window(ctx, true),
                HotkeyMsg::Quick(req) => self.start_quick(ctx, *req),
                HotkeyMsg::Show(req) => {
                    self.target_hwnd = req.hwnd;
                    self.previous_clipboard = req.previous_clipboard;
                    self.original = req.text;
                    self.instruction.clear();
                    self.output.clear();
                    self.show_settings = false;
                    self.show_original = false;
                    self.show_window(ctx, Some(req.cursor));
                    self.start_generation(ctx);
                }
            }
        }

        while let Ok((generation, msg)) = self.llm_rx.try_recv() {
            if generation != self.generation {
                continue; // resposta de um pedido ja descartado
            }
            match msg {
                Msg::Delta(delta) => self.output.push_str(&delta),
                Msg::Done => {
                    self.output = self.output.trim().to_string();
                    self.status = Status::Done;
                }
                Msg::Error(err) => self.status = Status::Error(err),
            }
        }

        self.drain_quick();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.tray_quit_id {
                self.begin_quit(ctx);
            } else if event.id == self.tray_open_id {
                self.open_settings(ctx);
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::DoubleClick { .. } = event {
                self.open_settings(ctx);
            }
        }
    }

    fn drain_quick(&mut self) {
        while let Ok((generation, msg)) = self.quick_rx.try_recv() {
            if self.quick.as_ref().map(|job| job.generation) != Some(generation) {
                continue;
            }
            match msg {
                Msg::Delta(delta) => {
                    if let Some(job) = self.quick.as_mut() {
                        job.buffer.push_str(&delta);
                    }
                }
                Msg::Done => {
                    let Some(job) = self.quick.take() else { continue };
                    let text = job.buffer.trim().to_string();
                    if text.is_empty() {
                        restore_clipboard(job.previous_clipboard);
                        self.note_quick("resposta vazia", true);
                    } else {
                        let hwnd = job.hwnd;
                        std::thread::spawn(move || {
                            let _ = win::paste_into(hwnd, &text);
                        });
                        self.note_quick("colado", false);
                    }
                }
                Msg::Error(err) => {
                    let Some(job) = self.quick.take() else { continue };
                    restore_clipboard(job.previous_clipboard);
                    self.note_quick(&err, true);
                }
            }
        }
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.draft = self.cfg.clone();
        self.show_settings = true;
        self.show_window(ctx, None);
    }

    fn begin_quit(&mut self, ctx: &egui::Context) {
        self.quitting = Some(Instant::now());
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        ctx.request_repaint();
    }

    fn apply_replace(&mut self, ctx: &egui::Context) {
        let text = self.output.trim().to_string();
        if text.is_empty() {
            return;
        }
        let hwnd = self.target_hwnd;
        self.previous_clipboard = None; // o resultado fica no clipboard de proposito
        self.hide_window(ctx, false);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            let _ = win::paste_into(hwnd, &text);
        });
    }

    fn apply_copy(&mut self, ctx: &egui::Context) {
        let text = self.output.trim().to_string();
        if text.is_empty() {
            return;
        }
        let _ = win::set_clipboard_text(&text);
        self.previous_clipboard = None;
        self.hide_window(ctx, false);
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some((message.to_string(), Instant::now()));
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Roda tambem com a janela escondida — e aqui que o atalho global e atendido.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_channels(ctx);

        // Sair pela bandeja nao pode depender do loop cooperar: se o `Close` nao derrubar o
        // processo no prazo, encerra na marra.
        if let Some(since) = self.quitting {
            if since.elapsed() > QUIT_GRACE {
                std::process::exit(0);
            }
        }

        // `ui()` rodou mas o app se considera escondido: alguem exibiu a janela pelas costas
        // (o eframe faz isso depois do primeiro frame). Reafirma o estado desejado.
        if self.ui_ran && !self.is_visible() {
            self.ui_ran = false;
            self.park_offscreen(ctx);
            ctx.request_repaint();
        }

        // Janela oculta ainda precisa acordar para ler os eventos da bandeja.
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_ran = true;
        let ctx = &ui.ctx().clone();

        if ctx.input(|i| i.viewport().close_requested()) {
            // Sem CancelClose quando o pedido veio da bandeja: ai e para fechar mesmo.
            if self.quitting.is_some() {
                return;
            }
            // O X da janela so esconde.
            self.hide_window(ctx, true);
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }

        if !self.is_visible() {
            return;
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.hide_window(ctx, true);
            return;
        }

        let (ctrl_enter, ctrl_c, ctrl_r) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::Enter),
                i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::C),
                i.modifiers.command && i.key_pressed(egui::Key::R),
            )
        });

        let frame = egui::Frame::new()
            .fill(BG)
            .corner_radius(14.0)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .inner_margin(egui::Margin::same(12));

        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            self.header(ui, ctx);
            if self.show_settings {
                ui.add_space(10.0);
                self.settings_ui(ui, ctx);
            } else {
                self.main_ui(ui, ctx);
            }
        });

        if !self.show_settings {
            if ctrl_enter {
                self.apply_replace(ctx);
            } else if ctrl_c {
                self.apply_copy(ctx);
                self.toast("copiado");
            } else if ctrl_r {
                self.start_generation(ctx);
            }
        }
    }
}

impl App {
    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            // A marca inteira e a alca de arrasto: a janela nao tem barra de titulo.
            let brand = ui.add(
                egui::Label::new(
                    egui::RichText::new("better-answer")
                        .size(12.5)
                        .strong()
                        .color(TEXT),
                )
                .sense(egui::Sense::drag()),
            );
            if brand.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            ui.label(egui::RichText::new(&self.cfg.model).size(11.0).color(MUTED));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "✕").on_hover_text("Esc").clicked() {
                    self.hide_window(ctx, true);
                }
                if icon_button(ui, "⚙").on_hover_text("Configuração").clicked() {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.draft = self.cfg.clone();
                    }
                }
                if !self.show_settings {
                    let glyph = if self.show_original { "◧" } else { "◨" };
                    if icon_button(ui, glyph)
                        .on_hover_text("Mostrar/ocultar o texto original")
                        .clicked()
                    {
                        self.show_original = !self.show_original;
                    }
                }
            });
        });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(10.0);

        if let Some(index) = self.tone_bar(ui, ctx) {
            self.tone_index = index;
            self.start_generation(ctx);
        }

        // Aviso do modo rapido: ele nao tem janela, entao reporta aqui na proxima abertura.
        if let Some((message, true)) = self.quick_note.as_ref().map(|(m, e)| (m.clone(), *e)) {
            ui.add_space(8.0);
            banner(ui, DANGER, &format!("atalho rápido: {message}"));
        }

        // Sem selecao o campo original vira o caminho principal, entao abre sozinho.
        let force_original = self.original.trim().is_empty();
        if self.show_original || force_original {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(PANEL)
                .corner_radius(10.0)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("ORIGINAL").size(9.5).color(MUTED));
                    ui.add_space(2.0);
                    ui.add(
                        egui::TextEdit::multiline(&mut self.original)
                            .frame(egui::Frame::NONE)
                            .desired_rows(if force_original { 3 } else { 2 })
                            .desired_width(f32::INFINITY)
                            .hint_text("Nada selecionado. Cole aqui e aperte Ctrl+R."),
                    );
                });
        }

        ui.add_space(8.0);

        // O resultado domina a janela: e o que a pessoa veio ler.
        let reserved = 34.0 + 30.0 + 10.0; // instrucao + rodape + respiros
        let body_height = (ui.available_height() - reserved).max(90.0);
        egui::Frame::new()
            .fill(PANEL)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(body_height)
                    .stick_to_bottom(matches!(self.status, Status::Loading))
                    .show(ui, |ui| match &self.status {
                        Status::NoSelection if self.output.is_empty() => {
                            ui.label(
                                egui::RichText::new(
                                    "Selecione um texto em qualquer app e chame o atalho.",
                                )
                                .color(MUTED),
                            );
                        }
                        Status::Error(err) => {
                            ui.label(egui::RichText::new(err).color(DANGER));
                        }
                        _ => {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.output)
                                    .frame(egui::Frame::NONE)
                                    .desired_rows(8)
                                    .desired_width(f32::INFINITY)
                                    .hint_text(if matches!(self.status, Status::Loading) {
                                        "gerando..."
                                    } else {
                                        ""
                                    }),
                            );
                        }
                    });
            });

        ui.add_space(8.0);
        self.instruction_bar(ui, ctx);
        ui.add_space(8.0);
        self.footer(ui, ctx);
    }

    /// Pills de tom. Devolve o indice pedido, por clique ou por Alt+1..9.
    fn tone_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> Option<usize> {
        let tones: Vec<String> = self.cfg.tones.iter().map(|t| t.name.clone()).collect();
        let mut requested = None;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            for (index, name) in tones.iter().enumerate() {
                let selected = index == self.tone_index;
                let (fill, stroke, color) = if selected {
                    (ACCENT.gamma_multiply(0.20), ACCENT.gamma_multiply(0.55), ACCENT)
                } else {
                    (PANEL, BORDER, MUTED)
                };
                let pill = ui.add(
                    egui::Button::new(egui::RichText::new(name).size(11.5).color(color))
                        .fill(fill)
                        .stroke(egui::Stroke::new(1.0, stroke))
                        .corner_radius(999.0),
                );
                if pill.on_hover_text(format!("Alt+{}", index + 1)).clicked() && !selected {
                    requested = Some(index);
                }
            }
        });

        let digit = ctx.input(|i| {
            const KEYS: [egui::Key; 9] = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
                egui::Key::Num9,
            ];
            KEYS.iter().position(|key| i.modifiers.alt && i.key_pressed(*key))
        });
        if let Some(index) = digit {
            if index < tones.len() && index != self.tone_index {
                requested = Some(index);
            }
        }

        requested
    }

    fn instruction_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Frame::new()
            .fill(PANEL)
            .corner_radius(999.0)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("›").size(14.0).color(ACCENT));
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.instruction)
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .hint_text("ajuste e Enter — 'mais curto', 'para o cliente'"),
                    );
                    if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.start_generation(ctx);
                    }
                });
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            match &self.status {
                Status::Loading => {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(egui::RichText::new("gerando").size(11.0).color(MUTED));
                }
                Status::Done => {
                    let words = self.output.split_whitespace().count();
                    ui.label(egui::RichText::new(format!("{words} palavras")).size(11.0).color(MUTED));
                }
                Status::Error(_) => {
                    ui.label(egui::RichText::new("erro").size(11.0).color(DANGER));
                }
                _ => {}
            }

            if let Some((message, at)) = self.toast.clone() {
                if at.elapsed() < Duration::from_secs(2) {
                    ui.label(egui::RichText::new(message).size(11.0).color(ACCENT));
                } else {
                    self.toast = None;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !self.output.trim().is_empty();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(egui::RichText::new("Substituir").size(11.5).color(BG))
                            .fill(ACCENT)
                            .corner_radius(8.0),
                    )
                    .on_hover_text("Ctrl+Enter — cola no app de origem")
                    .clicked()
                {
                    self.apply_replace(ctx);
                }
                if ghost_button(ui, "Copiar", ready).on_hover_text("Ctrl+Shift+C").clicked() {
                    self.apply_copy(ctx);
                    self.toast("copiado");
                }
                if ghost_button(ui, "Refazer", true).on_hover_text("Ctrl+R").clicked() {
                    self.start_generation(ctx);
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 42.0)
            .show(ui, |ui| {
                egui::Grid::new("settings")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Modelo");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.model)
                                .desired_width(f32::INFINITY),
                        );
                        ui.end_row();

                        ui.label("Atalho (popup)");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.hotkey)
                                .desired_width(f32::INFINITY)
                                .hint_text("ctrl+b"),
                        );
                        ui.end_row();

                        ui.label("Atalho rápido");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.quick_hotkey)
                                .desired_width(f32::INFINITY)
                                .hint_text("alt+b — vazio desliga"),
                        );
                        ui.end_row();

                        ui.label("Chave OpenRouter");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.api_key)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .hint_text("vazio = usa a variavel OPENROUTER_API_KEY"),
                        );
                        ui.end_row();

                        ui.label("Assinatura");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.signature)
                                .desired_width(f32::INFINITY)
                                .hint_text("seu nome / cargo"),
                        );
                        ui.end_row();

                        ui.label("Temperatura");
                        ui.add(egui::Slider::new(&mut self.draft.temperature, 0.0..=1.2));
                        ui.end_row();

                        ui.label("Máx. tokens");
                        ui.add(egui::DragValue::new(&mut self.draft.max_tokens).range(256..=8000));
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.label(egui::RichText::new("CONTEXTO FIXO").size(9.5).color(MUTED));
                ui.add_space(2.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.draft.extra_context)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("ex.: escrevo para o time de suporte da WMC; evite jargão técnico"),
                );

                ui.add_space(8.0);
                if let Ok(path) = Config::path() {
                    ui.label(
                        egui::RichText::new(format!("Tons editáveis em {}", path.display()))
                            .size(10.5)
                            .color(MUTED),
                    );
                }
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Salvar").size(11.5).color(BG))
                        .fill(ACCENT)
                        .corner_radius(8.0),
                )
                .clicked()
            {
                let hotkeys_changed = self.draft.hotkey != self.cfg.hotkey
                    || self.draft.quick_hotkey != self.cfg.quick_hotkey;
                self.cfg = self.draft.clone();
                match self.cfg.save() {
                    Ok(()) => {
                        self.sync_tray_tooltip();
                        if hotkeys_changed {
                            self.toast("salvo — reinicie para os novos atalhos valerem");
                        } else {
                            self.toast("salvo");
                        }
                        self.show_settings = false;
                    }
                    Err(err) => self.status = Status::Error(format!("{err:#}")),
                }
            }
            if ghost_button(ui, "Cancelar", true).clicked() {
                self.draft = self.cfg.clone();
                self.show_settings = false;
            }
            if let Some((message, at)) = self.toast.clone() {
                if at.elapsed() < Duration::from_secs(3) {
                    ui.label(egui::RichText::new(message).size(11.0).color(ACCENT));
                }
            }
            let _ = ctx;
        });
    }
}

fn restore_clipboard(previous: Option<String>) {
    if let Some(previous) = previous {
        let _ = win::set_clipboard_text(&previous);
    }
}

fn icon_button(ui: &mut egui::Ui, glyph: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(glyph).size(12.0).color(MUTED))
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(6.0)
            .min_size(egui::vec2(22.0, 20.0)),
    )
}

fn ghost_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).size(11.5).color(TEXT))
            .fill(PANEL_HI)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(8.0),
    )
}

fn banner(ui: &mut egui::Ui, color: egui::Color32, message: &str) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.14))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(message).size(11.0).color(color));
        });
}

struct Registration {
    manager: Option<GlobalHotKeyManager>,
    open_id: u32,
    quick_id: Option<u32>,
    error: Option<String>,
}

fn register_hotkeys(cfg: &Config) -> Registration {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            return Registration {
                manager: None,
                open_id: 0,
                quick_id: None,
                error: Some(format!("atalhos globais indisponiveis: {err}")),
            }
        }
    };

    let mut errors = Vec::new();

    let open_id = match crate::hotkey::parse(&cfg.hotkey).and_then(|hk| {
        manager.register(hk)?;
        Ok(hk.id())
    }) {
        Ok(id) => id,
        Err(err) => {
            errors.push(format!("atalho '{}' nao registrou: {err:#}", cfg.hotkey));
            0
        }
    };

    let quick_spec = cfg.quick_hotkey.trim();
    let quick_id = if quick_spec.is_empty() {
        None
    } else {
        match crate::hotkey::parse(quick_spec).and_then(|hk| {
            manager.register(hk)?;
            Ok(hk.id())
        }) {
            Ok(id) => Some(id),
            Err(err) => {
                errors.push(format!("atalho rápido '{quick_spec}' nao registrou: {err:#}"));
                None
            }
        }
    };

    Registration {
        manager: Some(manager),
        open_id,
        quick_id,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join(" · "))
        },
    }
}

fn spawn_hotkey_listener(
    ctx: egui::Context,
    tx: Sender<HotkeyMsg>,
    visible: Arc<AtomicBool>,
    open_id: u32,
    quick_id: Option<u32>,
) {
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }

            let is_quick = Some(event.id) == quick_id;
            if !is_quick && event.id != open_id {
                continue;
            }

            // Com o popup aberto, o atalho normal fecha em vez de copiar a propria janela.
            let msg = if !is_quick && visible.load(Ordering::SeqCst) {
                HotkeyMsg::Hide
            } else {
                let hwnd = win::foreground_window();
                let capture = win::capture_selection();
                let request = Box::new(ShowRequest {
                    hwnd,
                    text: capture.text,
                    previous_clipboard: capture.previous_clipboard,
                    cursor: win::cursor_pos(),
                });
                if is_quick {
                    HotkeyMsg::Quick(request)
                } else {
                    HotkeyMsg::Show(request)
                }
            };

            if tx.send(msg).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}

fn build_tray(cfg: &Config) -> (Option<TrayIcon>, MenuId, MenuId) {
    let menu = Menu::new();
    let open = MenuItem::new("Configuração", true, None);
    let quit = MenuItem::new("Sair", true, None);
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();
    let separator = PredefinedMenuItem::separator();

    let built = menu
        .append(&open)
        .and_then(|()| menu.append(&separator))
        .and_then(|()| menu.append(&quit))
        .is_ok();

    if !built {
        return (None, open_id, quit_id);
    }

    let quick = if cfg.quick_hotkey.trim().is_empty() {
        "desligado".to_string()
    } else {
        cfg.quick_hotkey.clone()
    };
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!(
            "better-answer\n{} — popup\n{} — rápido",
            cfg.hotkey, quick
        ))
        .with_icon(tray_icon_image())
        .build()
        .ok();

    (tray, open_id, quit_id)
}

/// Icone 32x32 desenhado em codigo para o binario nao depender de arquivo externo.
fn tray_icon_image() -> tray_icon::Icon {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let radius = 6i32;

    for y in 0..SIZE as i32 {
        for x in 0..SIZE as i32 {
            let index = ((y as u32 * SIZE + x as u32) * 4) as usize;

            // Canto arredondado: fora do raio nos cantos, deixa transparente.
            let cx = if x < radius {
                radius - x
            } else if x >= SIZE as i32 - radius {
                x - (SIZE as i32 - radius - 1)
            } else {
                0
            };
            let cy = if y < radius {
                radius - y
            } else if y >= SIZE as i32 - radius {
                y - (SIZE as i32 - radius - 1)
            } else {
                0
            };
            if cx * cx + cy * cy > radius * radius {
                continue;
            }

            let bars = [(9, 7, 25), (15, 7, 21), (21, 7, 17)];
            let on_bar = bars
                .iter()
                .any(|(by, x0, x1)| (y - by).abs() <= 1 && x >= *x0 && x <= *x1);

            let (r, g, b) = if on_bar {
                (255, 255, 255)
            } else {
                (47, 107, 255)
            };
            rgba[index] = r;
            rgba[index + 1] = g;
            rgba[index + 2] = b;
            rgba[index + 3] = 255;
        }
    }

    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("icone 32x32 valido")
}

fn style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.inactive.weak_bg_fill = PANEL_HI;
    visuals.widgets.hovered.weak_bg_fill = BORDER;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    visuals.widgets.active.weak_bg_fill = BORDER;
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.text_styles = [
            (TextStyle::Small, FontId::new(10.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        ]
        .into();
    });
}
