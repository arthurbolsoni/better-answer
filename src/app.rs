use crate::config::Config;
use crate::llm::{self, Msg};
use crate::win;
use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

pub const WINDOW_W: f32 = 580.0;
pub const WINDOW_H: f32 = 500.0;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(88, 140, 255);
const BG: egui::Color32 = egui::Color32::from_rgb(24, 26, 32);
const PANEL: egui::Color32 = egui::Color32::from_rgb(32, 35, 43);

enum Status {
    Idle,
    NoSelection,
    Loading,
    Done,
    Error(String),
}

/// Pedido de exibicao vindo da thread do atalho global.
struct ShowRequest {
    hwnd: isize,
    text: String,
    previous_clipboard: Option<String>,
    cursor: (i32, i32),
}

enum HotkeyMsg {
    Show(Box<ShowRequest>),
    Hide,
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
    visible: Arc<AtomicBool>,
    target_hwnd: isize,
    previous_clipboard: Option<String>,
    show_settings: bool,
    show_original: bool,
    toast: Option<(String, std::time::Instant)>,

    llm_tx: Sender<(u64, Msg)>,
    llm_rx: Receiver<(u64, Msg)>,
    hotkey_rx: Receiver<HotkeyMsg>,

    tray_open_id: MenuId,
    tray_quit_id: MenuId,
    _tray: Option<TrayIcon>,
    _hotkeys: Option<GlobalHotKeyManager>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, cfg: Config, cfg_error: Option<String>) -> Self {
        let ctx = cc.egui_ctx.clone();
        style(&ctx);

        let (llm_tx, llm_rx) = channel();
        let (hotkey_tx, hotkey_rx) = channel();
        let visible = Arc::new(AtomicBool::new(false));

        let mut status = match cfg_error {
            Some(err) => Status::Error(err),
            None => Status::Idle,
        };

        // O manager precisa nascer na thread que roda o event loop win32 (a main).
        let hotkeys = match register_hotkey(&cfg.hotkey) {
            Ok(manager) => Some(manager),
            Err(err) => {
                status = Status::Error(format!("atalho '{}' nao registrou: {err:#}", cfg.hotkey));
                None
            }
        };

        spawn_hotkey_listener(ctx.clone(), hotkey_tx, visible.clone());

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
            visible,
            target_hwnd: 0,
            previous_clipboard: None,
            show_settings: false,
            show_original: false,
            toast: None,
            llm_tx,
            llm_rx,
            hotkey_rx,
            tray_open_id,
            tray_quit_id,
            _tray: tray,
            _hotkeys: hotkeys,
        }
    }

    fn is_visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
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
                Some(monitor) => egui::pos2(
                    (monitor.x - WINDOW_W) / 2.0,
                    (monitor.y - WINDOW_H) / 2.0,
                ),
                None => egui::pos2(200.0, 200.0),
            },
        };

        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.visible.store(true, Ordering::SeqCst);
    }

    fn hide_window(&mut self, ctx: &egui::Context, restore_clipboard: bool) {
        if restore_clipboard {
            if let Some(previous) = self.previous_clipboard.take() {
                let _ = win::set_clipboard_text(&previous);
            }
        }
        self.previous_clipboard = None;
        // Uma geracao nova invalida o stream em andamento.
        self.generation += 1;
        self.show_settings = false;
        self.visible.store(false, Ordering::SeqCst);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    fn start_generation(&mut self, ctx: &egui::Context) {
        if self.original.trim().is_empty() {
            self.status = Status::NoSelection;
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.output.clear();
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

    fn drain_channels(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.hotkey_rx.try_recv() {
            match msg {
                HotkeyMsg::Hide => self.hide_window(ctx, true),
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

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.tray_quit_id {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if event.id == self.tray_open_id {
                self.draft = self.cfg.clone();
                self.show_settings = true;
                self.show_window(ctx, None);
            }
        }

        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::DoubleClick { .. } = event {
                self.draft = self.cfg.clone();
                self.show_settings = true;
                self.show_window(ctx, None);
            }
        }
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
        self.toast = Some((message.to_string(), std::time::Instant::now()));
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// Roda tambem com a janela escondida — e aqui que o atalho global e atendido.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_channels(ctx);
        // Janela oculta ainda precisa acordar para ler os eventos da bandeja.
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();

        if !self.is_visible() {
            return;
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            // O X da janela so esconde; sair e pela bandeja.
            self.hide_window(ctx, true);
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            return;
        }

        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if esc {
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
            .corner_radius(12.0)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 62, 74)))
            .inner_margin(egui::Margin::same(14));

        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            self.header(ui, ctx);
            ui.add_space(8.0);
            if self.show_settings {
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
            let title = ui.add(
                egui::Label::new(
                    egui::RichText::new("better-answer")
                        .strong()
                        .color(ACCENT)
                        .size(15.0),
                )
                .sense(egui::Sense::drag()),
            );
            if title.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").on_hover_text("Esc").clicked() {
                    self.hide_window(ctx, true);
                }
                let gear = if self.show_settings { "▲" } else { "⚙" };
                if ui.button(gear).on_hover_text("Configuração").clicked() {
                    self.show_settings = !self.show_settings;
                    if self.show_settings {
                        self.draft = self.cfg.clone();
                    }
                }
                ui.label(
                    egui::RichText::new(&self.cfg.model)
                        .small()
                        .color(egui::Color32::from_gray(130)),
                );
            });
        });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // Tons: clique ou Alt+1..9.
        let tones: Vec<String> = self.cfg.tones.iter().map(|t| t.name.clone()).collect();
        let mut requested_tone: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            for (index, name) in tones.iter().enumerate() {
                let selected = index == self.tone_index;
                let label = format!("{}  {}", index + 1, name);
                if ui.selectable_label(selected, label).clicked() && !selected {
                    requested_tone = Some(index);
                }
            }
        });

        let digit_pressed = ctx.input(|i| {
            [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
                egui::Key::Num9,
            ]
            .iter()
            .position(|key| i.modifiers.alt && i.key_pressed(*key))
        });
        if let Some(index) = digit_pressed {
            if index < tones.len() {
                requested_tone = Some(index);
            }
        }
        if let Some(index) = requested_tone {
            self.tone_index = index;
            self.start_generation(ctx);
        }

        ui.add_space(6.0);

        egui::CollapsingHeader::new("Texto original")
            .default_open(false)
            .open(if self.original.trim().is_empty() {
                Some(true)
            } else {
                None
            })
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.original)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("Nada foi selecionado. Cole ou digite aqui e aperte Ctrl+R."),
                );
            });

        ui.add_space(6.0);
        let instruction = ui.add(
            egui::TextEdit::singleline(&mut self.instruction)
                .desired_width(f32::INFINITY)
                .hint_text("Instrução extra (opcional) — ex.: 'mais curto', 'para o cliente'"),
        );
        if instruction.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.start_generation(ctx);
        }

        ui.add_space(8.0);

        let available = ui.available_height() - 46.0;
        egui::Frame::new()
            .fill(PANEL)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(available.max(80.0))
                    .stick_to_bottom(matches!(self.status, Status::Loading))
                    .show(ui, |ui| {
                        match &self.status {
                            Status::NoSelection if self.output.is_empty() => {
                                ui.colored_label(
                                    egui::Color32::from_gray(150),
                                    "Nenhum texto selecionado. Selecione algo e chame o atalho de novo, \
                                     ou escreva no campo 'Texto original'.",
                                );
                            }
                            Status::Error(err) => {
                                ui.colored_label(egui::Color32::from_rgb(240, 120, 120), err);
                            }
                            _ => {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.output)
                                        .frame(egui::Frame::NONE)
                                        .desired_rows(10)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(if matches!(self.status, Status::Loading) {
                                            "gerando..."
                                        } else {
                                            ""
                                        }),
                                );
                            }
                        }
                    });
            });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match &self.status {
                Status::Loading => {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(egui::RichText::new("gerando").small());
                }
                Status::Done => {
                    let words = self.output.split_whitespace().count();
                    ui.label(
                        egui::RichText::new(format!("{words} palavras"))
                            .small()
                            .color(egui::Color32::from_gray(130)),
                    );
                }
                Status::Error(_) => {
                    ui.label(
                        egui::RichText::new("erro")
                            .small()
                            .color(egui::Color32::from_rgb(240, 120, 120)),
                    );
                }
                _ => {}
            }

            if let Some((message, at)) = &self.toast {
                if at.elapsed() < Duration::from_secs(2) {
                    ui.label(egui::RichText::new(message).small().color(ACCENT));
                } else {
                    self.toast = None;
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !self.output.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("Substituir"))
                    .on_hover_text("Ctrl+Enter — cola no app de origem")
                    .clicked()
                {
                    self.apply_replace(ctx);
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Copiar"))
                    .on_hover_text("Ctrl+Shift+C")
                    .clicked()
                {
                    self.apply_copy(ctx);
                }
                if ui
                    .button("Refazer")
                    .on_hover_text("Ctrl+R")
                    .clicked()
                {
                    self.start_generation(ctx);
                }
            });
        });
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 40.0)
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

                        ui.label("Atalho");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.hotkey)
                                .desired_width(f32::INFINITY)
                                .hint_text("ctrl+alt+e"),
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

                ui.add_space(6.0);
                ui.label("Contexto fixo (some em todos os tons)");
                ui.add(
                    egui::TextEdit::multiline(&mut self.draft.extra_context)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("ex.: escrevo para o time de suporte da WMC; evite jargão técnico"),
                );

                ui.add_space(6.0);
                if let Ok(path) = Config::path() {
                    ui.label(
                        egui::RichText::new(format!("Tons editáveis em {}", path.display()))
                            .small()
                            .color(egui::Color32::from_gray(120)),
                    );
                }
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Salvar").clicked() {
                let hotkey_changed = self.draft.hotkey != self.cfg.hotkey;
                self.cfg = self.draft.clone();
                match self.cfg.save() {
                    Ok(()) => {
                        if hotkey_changed {
                            self.toast("salvo — reinicie para o novo atalho valer");
                        } else {
                            self.toast("salvo");
                        }
                        self.show_settings = false;
                    }
                    Err(err) => self.status = Status::Error(format!("{err:#}")),
                }
            }
            if ui.button("Cancelar").clicked() {
                self.draft = self.cfg.clone();
                self.show_settings = false;
            }
            if let Some((message, at)) = &self.toast {
                if at.elapsed() < Duration::from_secs(3) {
                    ui.label(egui::RichText::new(message).small().color(ACCENT));
                }
            }
            let _ = ctx;
        });
    }
}

fn register_hotkey(spec: &str) -> anyhow::Result<GlobalHotKeyManager> {
    let hotkey = crate::hotkey::parse(spec)?;
    let manager = GlobalHotKeyManager::new()?;
    manager.register(hotkey)?;
    Ok(manager)
}

fn spawn_hotkey_listener(
    ctx: egui::Context,
    tx: Sender<HotkeyMsg>,
    visible: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            // Com o popup aberto, o atalho fecha em vez de copiar a selecao da propria janela.
            let msg = if visible.load(Ordering::SeqCst) {
                HotkeyMsg::Hide
            } else {
                let hwnd = win::foreground_window();
                let capture = win::capture_selection();
                HotkeyMsg::Show(Box::new(ShowRequest {
                    hwnd,
                    text: capture.text,
                    previous_clipboard: capture.previous_clipboard,
                    cursor: win::cursor_pos(),
                }))
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
        .with_tooltip(format!("better-answer — {}", cfg.hotkey))
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
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}
