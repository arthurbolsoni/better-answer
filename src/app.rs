use crate::config::Config;
use crate::llm::{self, Msg};
use crate::win;
use eframe::egui;
use egui::{FontFamily, FontId, TextStyle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const WINDOW_W: f32 = 560.0;
pub const WINDOW_H: f32 = 430.0;

/// A caixinha do modo rapido, do tamanho de um menu de contexto.
const HUD_W: f32 = 290.0;
const HUD_H: f32 = 44.0;
/// Quanto tempo a caixinha fica na tela depois de um erro, ja que ninguem vai fecha-la.
const HUD_ERROR_LINGER: Duration = Duration::from_secs(5);

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

/// A janela e uma so; o que muda e o que ela desenha.
#[derive(PartialEq, Clone, Copy)]
enum Mode {
    /// O popup completo, com tons, resultado editavel e acoes.
    Popup,
    /// A caixinha do atalho rapido: so diz em que pe esta o trabalho.
    Hud,
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
    /// Atalho de chamado: abre o popup ja no tom "Chamado".
    Ticket(Box<ShowRequest>),
    Hide,
}

/// Trabalho do modo rapido. Nao toca no estado do popup: acumula em silencio e cola no fim.
struct QuickJob {
    hwnd: isize,
    generation: u64,
    buffer: String,
    previous_clipboard: Option<String>,
    /// Onde a caixinha nasceu, para um erro tardio reaparecer no mesmo lugar.
    cursor: (i32, i32),
}

pub struct App {
    cfg: Config,
    /// Copia editavel enquanto o painel de configuracao esta aberto.
    draft: Config,
    /// Tom da geracao atual.
    tone_index: usize,
    /// Ultimo tom escolhido a mao (pill ou Alt+N). O atalho de chamado forca o tom dele sem mexer
    /// aqui, senao a proxima abertura pelo atalho normal viria com o tom de chamado grudado.
    chosen_tone: usize,
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
    /// Ultimo resultado do modo rapido, mostrado como aviso na proxima abertura do popup.
    quick_note: Option<(String, bool)>,
    quick: Option<QuickJob>,
    mode: Mode,
    hud_message: String,
    /// Quando o erro apareceu na caixinha, para ela sumir sozinha.
    hud_error_since: Option<Instant>,

    llm_tx: Sender<(u64, Msg)>,
    llm_rx: Receiver<(u64, Msg)>,
    quick_tx: Sender<(u64, Msg)>,
    quick_rx: Receiver<(u64, Msg)>,
    hotkey_rx: Receiver<HotkeyMsg>,

    tray_open_id: MenuId,
    tray_quit_id: MenuId,
    tray: Option<TrayIcon>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, cfg_error: Option<String>) -> Self {
        let ctx = cc.egui_ctx.clone();
        style(&ctx);
        win::round_corners(window_handle(cc), (BORDER.r(), BORDER.g(), BORDER.b()));

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

        if let Some(hotkey_rx) = registration.hotkey_rx {
            spawn_hotkey_listener(ctx.clone(), hotkey_tx, visible.clone(), hotkey_rx);
        }

        let (tray, tray_open_id, tray_quit_id) = build_tray(&cfg);

        Self {
            draft: cfg.clone(),
            cfg,
            tone_index: 0,
            chosen_tone: 0,
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
            mode: Mode::Popup,
            hud_message: String::new(),
            hud_error_since: None,
            llm_tx,
            llm_rx,
            quick_tx,
            quick_rx,
            hotkey_rx,
            tray_open_id,
            tray_quit_id,
            tray,
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

    fn show_popup(&mut self, ctx: &egui::Context, cursor: Option<(i32, i32)>) {
        self.mode = Mode::Popup;
        self.hud_error_since = None;
        self.show_window(ctx, cursor, egui::vec2(WINDOW_W, WINDOW_H), true);
    }

    /// A caixinha nao pode roubar o foco: o texto melhorado precisa voltar para a janela de
    /// origem, e ela tem que continuar sendo a janela ativa ate a hora de colar.
    fn show_hud(&mut self, ctx: &egui::Context, cursor: (i32, i32)) {
        self.mode = Mode::Hud;
        self.hud_error_since = None;
        self.show_window(ctx, Some(cursor), egui::vec2(HUD_W, HUD_H), false);
    }

    fn show_window(
        &mut self,
        ctx: &egui::Context,
        cursor: Option<(i32, i32)>,
        size: egui::Vec2,
        focus: bool,
    ) {
        let ppp = ctx.pixels_per_point().max(0.1);

        // Tudo em pixels fisicos, que e a unidade do cursor e da area util; a conversao para
        // pontos logicos acontece so na hora de mandar o comando.
        let anchor = cursor.unwrap_or_else(win::cursor_pos);
        let (left, top, right, bottom) = win::work_area_at(anchor);
        let width = (size.x * ppp).round() as i32;
        let height = (size.y * ppp).round() as i32;

        let (x, y) = match cursor {
            Some((cx, cy)) => {
                let gap = (12.0 * ppp).round() as i32;
                (cx + gap, cy + gap)
            }
            None => (
                left + (right - left - width) / 2,
                top + (bottom - top - height) / 2,
            ),
        };

        // O `.max(left/top)` vem depois do `.min` de proposito: numa janela maior que a area util,
        // ele ganha e a janela encosta no canto superior esquerdo em vez de sumir para cima.
        let x = x.min(right - width).max(left);
        let y = y.min(bottom - height).max(top);

        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            x as f32 / ppp,
            y as f32 / ppp,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        if focus {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
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
        self.hud_error_since = None;
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

    /// Abre o popup com o texto que o atalho capturou, no tom pedido.
    fn open_captured(&mut self, ctx: &egui::Context, req: ShowRequest, tone_index: usize) {
        self.target_hwnd = req.hwnd;
        self.previous_clipboard = req.previous_clipboard;
        self.original = req.text;
        self.instruction.clear();
        self.output.clear();
        self.show_settings = false;
        self.show_original = false;
        self.tone_index = tone_index;
        self.show_popup(ctx, Some(req.cursor));
        self.start_generation(ctx);
    }

    /// Modo rapido: sem popup, so a caixinha ao lado do cursor dizendo em que pe esta. No fim ela
    /// some sozinha e o texto melhorado entra no lugar do original.
    fn start_quick(&mut self, ctx: &egui::Context, req: ShowRequest) {
        let cursor = req.cursor;
        let text = req.text.trim().to_string();
        if text.is_empty() {
            restore_clipboard(req.previous_clipboard);
            self.fail_quick(ctx, cursor, "nada selecionado");
            return;
        }

        self.quick_generation += 1;
        let generation = self.quick_generation;
        self.quick = Some(QuickJob {
            hwnd: req.hwnd,
            generation,
            buffer: String::new(),
            previous_clipboard: req.previous_clipboard,
            cursor,
        });
        // O tom aqui e o escolhido a mao, nunca o que o atalho de chamado forcou no popup.
        let tone = self.cfg.tone(self.chosen_tone).clone();
        self.hud_message = format!("melhorando · {}", tone.name);
        self.note_quick("melhorando...", false);
        self.show_hud(ctx, cursor);

        let cfg = self.cfg.clone();
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

    /// Erro do modo rapido: a caixinha mostra o motivo e se apaga sozinha.
    fn fail_quick(&mut self, ctx: &egui::Context, cursor: (i32, i32), message: &str) {
        self.note_quick(message, true);
        self.hud_message = message.to_string();
        self.show_hud(ctx, cursor);
        self.hud_error_since = Some(Instant::now());
    }

    fn note_quick(&mut self, message: &str, is_error: bool) {
        self.quick_note = Some((message.to_string(), is_error));
        self.sync_tray_tooltip();
    }

    fn sync_tray_tooltip(&self) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let mut tip = tray_tooltip(&self.cfg);
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
                    let tone = self.chosen_tone;
                    self.open_captured(ctx, *req, tone);
                }
                HotkeyMsg::Ticket(req) => {
                    let tone = self.cfg.ticket_tone_index();
                    self.open_captured(ctx, *req, tone);
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

        self.drain_quick(ctx);

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

    fn drain_quick(&mut self, ctx: &egui::Context) {
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
                        self.fail_quick(ctx, job.cursor, "resposta vazia");
                        continue;
                    }
                    // A caixinha sai da frente antes de colar: o foco precisa voltar inteiro para
                    // a janela de origem, senao o Ctrl+V cai no lugar errado.
                    self.hide_window(ctx, false);
                    self.note_quick("colado", false);
                    let hwnd = job.hwnd;
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(120));
                        let _ = win::paste_into(hwnd, &text);
                    });
                }
                Msg::Error(err) => {
                    let Some(job) = self.quick.take() else { continue };
                    restore_clipboard(job.previous_clipboard);
                    self.fail_quick(ctx, job.cursor, &err);
                }
            }
        }
    }

    fn open_settings(&mut self, ctx: &egui::Context) {
        self.draft = self.cfg.clone();
        self.show_settings = true;
        self.show_popup(ctx, None);
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
        let [r, g, b, _] = BG.to_normalized_gamma_f32();
        [r, g, b, 1.0]
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

        // A caixinha de erro do modo rapido nao tem quem a feche: ela se apaga.
        if let Some(since) = self.hud_error_since {
            if since.elapsed() > HUD_ERROR_LINGER {
                self.hide_window(ctx, false);
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

        if self.mode == Mode::Hud {
            self.hud_ui(ui);
            return;
        }

        let (ctrl_enter, ctrl_c, ctrl_r) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(egui::Key::Enter),
                i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::C),
                i.modifiers.command && i.key_pressed(egui::Key::R),
            )
        });

        // Sem canto nem contorno aqui: quem recorta e desenha a borda da janela e o DWM. Um card
        // arredondado por cima de uma janela opaca so deixaria as quinas quadradas aparecendo.
        let frame = egui::Frame::new()
            .fill(BG)
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
    /// A caixinha do modo rapido: uma linha, do tamanho de um menu de contexto.
    fn hud_ui(&mut self, ui: &mut egui::Ui) {
        let failed = self.hud_error_since.is_some();
        let frame = egui::Frame::new()
            .fill(BG)
            .inner_margin(egui::Margin::symmetric(12, 10));

        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                if failed {
                    ui.label(egui::RichText::new(crate::icon::WARNING).size(15.0).color(DANGER));
                } else {
                    ui.add(egui::Spinner::new().size(13.0).color(ACCENT));
                }
                ui.add_space(2.0);
                let color = if failed { DANGER } else { TEXT };
                ui.add(
                    egui::Label::new(egui::RichText::new(&self.hud_message).size(12.0).color(color))
                        .truncate(),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(crate::icon::LIGHTNING)
                            .size(13.0)
                            .color(MUTED),
                    );
                });
            });
        });
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // A janela nao tem barra de titulo, entao a faixa inteira do cabecalho vira alca. Um
        // titulo estreito nao bastava: encostada na borda da tela, sobrava quase nada para agarrar.
        // A area de arrasto e reservada antes do conteudo, e os botoes desenhados depois ficam por
        // cima dela — quem clica no X fecha, quem clica no vazio arrasta.
        let (bar, drag) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 22.0),
            egui::Sense::drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        drag.on_hover_cursor(egui::CursorIcon::Grab);

        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut ui;
        {
            ui.label(
                egui::RichText::new(format!("{}  better-answer", crate::icon::SPARKLE))
                    .size(12.5)
                    .strong()
                    .color(TEXT),
            );
            ui.label(egui::RichText::new(&self.cfg.model).size(11.0).color(MUTED));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, crate::icon::X).on_hover_text("Esc").clicked() {
                    self.hide_window(ctx, true);
                }
                if icon_button(ui, crate::icon::GEAR).on_hover_text("Configuração").clicked() {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.draft = self.cfg.clone();
                    }
                }
                if !self.show_settings {
                    let color = if self.show_original { ACCENT } else { MUTED };
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new(crate::icon::TEXT_ALIGN_LEFT)
                                    .size(15.0)
                                    .color(color),
                            )
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(egui::Stroke::NONE)
                            .corner_radius(6.0),
                        )
                        .on_hover_text("Mostrar/ocultar o texto capturado")
                        .clicked()
                    {
                        self.show_original = !self.show_original;
                    }
                }
            });
        }
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(10.0);

        if let Some(index) = self.tone_bar(ui, ctx) {
            self.tone_index = index;
            self.chosen_tone = index;
            self.start_generation(ctx);
        }

        // Aviso do modo rapido: ele nao tem janela, entao reporta aqui na proxima abertura.
        if let Some((message, true)) = self.quick_note.as_ref().map(|(m, e)| (m.clone(), *e)) {
            ui.add_space(8.0);
            banner(ui, DANGER, &format!("{}  atalho rápido: {message}", crate::icon::WARNING));
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
                    ui.label(egui::RichText::new(crate::icon::CARET_RIGHT).size(13.0).color(ACCENT));
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
                        egui::Button::new(
                            egui::RichText::new(format!("{}  Substituir", crate::icon::CHECK))
                                .size(11.5)
                                .color(BG),
                        )
                            .fill(ACCENT)
                            .corner_radius(8.0),
                    )
                    .on_hover_text("Ctrl+Enter — cola no app de origem")
                    .clicked()
                {
                    self.apply_replace(ctx);
                }
                if ghost_button(ui, &format!("{}  Copiar", crate::icon::COPY), ready).on_hover_text("Ctrl+Shift+C").clicked() {
                    self.apply_copy(ctx);
                    self.toast("copiado");
                }
                if ghost_button(ui, &format!("{}  Refazer", crate::icon::ARROW_CLOCKWISE), true).on_hover_text("Ctrl+R").clicked() {
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

                        ui.label("Atalho chamado");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.ticket_hotkey)
                                .desired_width(f32::INFINITY)
                                .hint_text("ctrl+d — vazio desliga"),
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
                    || self.draft.quick_hotkey != self.cfg.quick_hotkey
                    || self.draft.ticket_hotkey != self.cfg.ticket_hotkey;
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

/// HWND da janela raiz, ou 0 se o backend nao expuser (nenhum caminho depende disso para andar).
fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match cc.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(win32)) => win32.hwnd.get(),
        _ => 0,
    }
}

fn restore_clipboard(previous: Option<String>) {
    if let Some(previous) = previous {
        let _ = win::set_clipboard_text(&previous);
    }
}

/// Botao de icone do cabecalho.
///
/// O glifo tem que existir nas fontes que o egui embute — a familia proporcional e
/// `[Ubuntu-Light, NotoEmoji-Regular, emoji-icon-font]`. Simbolo fora dessas vira quadrado vazio:
/// `✕` (U+2715) e `◧`/`◨` (U+25E7/8), por exemplo, nao estao em nenhuma delas. Dai `×` (U+00D7,
/// Ubuntu-Light) e `⚙` (U+2699, emoji-icon-font).
fn icon_button(ui: &mut egui::Ui, glyph: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(glyph).size(15.0).color(MUTED))
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

/// Ids que o hook devolve. Fixos, so precisam ser distintos entre si.
const OPEN_ID: u32 = 1;
const QUICK_ID: u32 = 2;
const TICKET_ID: u32 = 3;

struct Registration {
    hotkey_rx: Option<Receiver<u32>>,
    error: Option<String>,
}

fn register_hotkeys(cfg: &Config) -> Registration {
    let mut bindings: Vec<crate::hook::Binding> = Vec::new();
    let mut errors = Vec::new();

    let wanted = [
        (OPEN_ID, cfg.hotkey.trim(), "atalho"),
        (QUICK_ID, cfg.quick_hotkey.trim(), "atalho rápido"),
        (TICKET_ID, cfg.ticket_hotkey.trim(), "atalho de chamado"),
    ];

    for (id, spec, label) in wanted {
        // Campo vazio desliga o atalho — menos o do popup, que e o caminho principal e por isso
        // segue para o parse (e reclama).
        if spec.is_empty() && id != OPEN_ID {
            continue;
        }
        match crate::hotkey::parse(spec) {
            Ok(shortcut) => {
                // O hook roteia pelo primeiro binding que casa: atalho repetido deixaria o
                // segundo mudo para sempre. Melhor dizer isso na cara.
                if bindings.iter().any(|b| b.shortcut == shortcut) {
                    errors.push(format!("{label} '{spec}' repete outro atalho"));
                    continue;
                }
                bindings.push(crate::hook::Binding { id, shortcut });
            }
            Err(err) => errors.push(format!("{label} '{spec}' invalido: {err:#}")),
        }
    }

    let (tx, rx) = channel();
    let hotkey_rx = match crate::hook::spawn(bindings, tx) {
        Ok(()) => Some(rx),
        Err(err) => {
            errors.push(format!("{err:#}"));
            None
        }
    };

    Registration {
        hotkey_rx,
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join(" · "))
        },
    }
}

/// Traduz o aviso do hook em trabalho de verdade. Precisa ser uma thread separada da do hook:
/// capturar a selecao dorme ate 600ms esperando o app de origem responder ao Ctrl+C.
fn spawn_hotkey_listener(
    ctx: egui::Context,
    tx: Sender<HotkeyMsg>,
    visible: Arc<AtomicBool>,
    hotkey_rx: Receiver<u32>,
) {
    std::thread::spawn(move || {
        while let Ok(id) = hotkey_rx.recv() {
            let is_quick = id == QUICK_ID;

            // Com o popup aberto, os atalhos que abrem popup fecham em vez de copiar a propria
            // janela (o Ctrl+C sintetico cairia na janela do proprio app).
            let msg = if !is_quick && visible.load(Ordering::SeqCst) {
                HotkeyMsg::Hide
            } else {
                // O alvo e lido antes de qualquer tecla sintetica: se algo roubar o foco no meio,
                // a colagem ainda volta para a janela certa.
                let hwnd = win::foreground_window();
                let capture = win::capture_selection();
                let request = Box::new(ShowRequest {
                    hwnd,
                    text: capture.text,
                    previous_clipboard: capture.previous_clipboard,
                    cursor: win::cursor_pos(),
                });
                match id {
                    QUICK_ID => HotkeyMsg::Quick(request),
                    TICKET_ID => HotkeyMsg::Ticket(request),
                    _ => HotkeyMsg::Show(request),
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

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tray_tooltip(cfg))
        .with_icon(tray_icon_image())
        .build()
        .ok();

    (tray, open_id, quit_id)
}

/// Tooltip da bandeja: e o unico lugar onde os atalhos ficam visiveis sem abrir a configuracao.
fn tray_tooltip(cfg: &Config) -> String {
    let optional = |spec: &str| {
        let spec = spec.trim();
        if spec.is_empty() {
            "desligado".to_string()
        } else {
            spec.to_string()
        }
    };
    format!(
        "better-answer\n{} — popup\n{} — rápido\n{} — chamado",
        cfg.hotkey,
        optional(&cfg.quick_hotkey),
        optional(&cfg.ticket_hotkey),
    )
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
    let mut fonts = egui::FontDefinitions::default();
    crate::icon::install(&mut fonts);
    ctx.set_fonts(fonts);

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
