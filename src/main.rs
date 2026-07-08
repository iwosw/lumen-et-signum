#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{
    self, Align, Button, CentralPanel, Color32, ComboBox, Context, FontId, Frame, Layout, Margin,
    RichText, Rounding, ScrollArea, SidePanel, Stroke, TextEdit, TopBottomPanel, Vec2,
    ViewportBuilder,
};
use espscript::ScriptStep;
use serialport::{SerialPortInfo, SerialPortType};
use std::io::{Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod espscript;

const BG: Color32 = Color32::from_rgb(8, 10, 14);
const PANEL: Color32 = Color32::from_rgb(12, 15, 22);
const CARD: Color32 = Color32::from_rgb(17, 21, 31);
const CARD_SOFT: Color32 = Color32::from_rgb(24, 30, 43);
const SURFACE: Color32 = Color32::from_rgb(13, 16, 24);
const TERMINAL: Color32 = Color32::from_rgb(5, 7, 11);
const BORDER: Color32 = Color32::from_rgb(38, 46, 64);
const BORDER_SOFT: Color32 = Color32::from_rgb(28, 34, 48);
const ACCENT: Color32 = Color32::from_rgb(125, 211, 252);
const ACCENT_2: Color32 = Color32::from_rgb(167, 139, 250);
const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
const WARN: Color32 = Color32::from_rgb(251, 191, 36);
const BAD: Color32 = Color32::from_rgb(251, 113, 133);
const TEXT: Color32 = Color32::from_rgb(236, 241, 247);
const TEXT_DIM: Color32 = Color32::from_rgb(148, 163, 184);
const SCRIPT_STRICT_COMMAND_TIMEOUT_MS: u64 = 5_000;
const SCRIPT_STRICT_SLEEP_GRACE_MS: u64 = 1_000;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1220.0, 780.0])
            .with_min_inner_size([1040.0, 660.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Lumen et Signum",
        native_options,
        Box::new(|cc| Ok(Box::new(EspApp::new(cc)))),
    )
}

#[derive(Clone)]
struct PortEntry {
    name: String,
    detail: String,
    is_usb: bool,
}

#[derive(Default, Clone)]
struct BoardInfo {
    chip: String,
    crystal: String,
    flash: String,
    features: String,
    mac: String,
    security: String,
}

impl BoardInfo {
    fn is_empty(&self) -> bool {
        self.chip.is_empty()
            && self.crystal.is_empty()
            && self.flash.is_empty()
            && self.features.is_empty()
            && self.mac.is_empty()
            && self.security.is_empty()
    }
}

#[derive(Default, Clone)]
struct FirmwareCaps {
    name: String,
    version: String,
    protocol: String,
    features: Vec<String>,
    raw: String,
}

impl FirmwareCaps {
    fn supports(&self, feature: &str) -> bool {
        self.features.iter().any(|value| value == feature)
    }

    fn title(&self) -> String {
        match (self.name.is_empty(), self.version.is_empty()) {
            (true, true) => "unknown".to_owned(),
            (false, true) => self.name.clone(),
            (true, false) => format!("v{}", self.version),
            (false, false) => format!("{} v{}", self.name, self.version),
        }
    }

    fn protocol_label(&self) -> String {
        if self.protocol.is_empty() {
            "unknown".to_owned()
        } else {
            self.protocol.clone()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    BoardInfo,
    Reset,
}

impl CommandKind {
    fn title(self) -> &'static str {
        match self {
            Self::BoardInfo => "Проверка платы",
            Self::Reset => "Reset платы",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MonitorState {
    Stopped,
    Starting,
    Running,
    Stopping,
}

impl MonitorState {
    fn label(self) -> &'static str {
        match self {
            Self::Stopped => "monitor off",
            Self::Starting => "monitor starting",
            Self::Running => "monitor live",
            Self::Stopping => "monitor stopping",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Stopped => TEXT_DIM,
            Self::Starting => WARN,
            Self::Running => GOOD,
            Self::Stopping => WARN,
        }
    }
}

enum AppEvent {
    CommandFinished {
        kind: CommandKind,
        ok: bool,
        output: String,
    },
    MonitorStarted,
    MonitorStopped,
    SerialData(String),
    SerialEcho(String),
    SerialError(String),
    ScriptFinished {
        ok: bool,
        sent: usize,
        message: String,
    },
}

enum ScriptWaitResult {
    Matched,
    Timeout,
    Stopped,
}

#[derive(Debug, PartialEq, Eq)]
enum StrictCommandResult {
    Ok(String),
    Err(String),
    Timeout,
    Stopped,
}

struct EspApp {
    ports: Vec<PortEntry>,
    selected_port: String,
    baud_rate: String,
    send_text: String,
    script_window_open: bool,
    script_text: String,
    script_delay_ms: String,
    script_strict_mode: bool,
    script_running: bool,
    script_stop: Option<Arc<AtomicBool>>,
    script_response_tx: Option<mpsc::Sender<String>>,
    firmware_caps: Option<FirmwareCaps>,
    caps_line_buffer: String,
    caps_query_pending: bool,
    caps_query_started: Option<Instant>,

    board: BoardInfo,
    board_status: String,
    board_status_color: Color32,
    command_running: Option<CommandKind>,
    last_command_output: String,

    monitor_state: MonitorState,
    monitor_stop: Option<Arc<AtomicBool>>,
    monitor_tx: Option<mpsc::Sender<Vec<u8>>>,
    serial_log: String,

    activity: Vec<String>,
    events_tx: mpsc::Sender<AppEvent>,
    events_rx: mpsc::Receiver<AppEvent>,
}

impl EspApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);

        let (events_tx, events_rx) = mpsc::channel();
        let mut app = Self {
            ports: Vec::new(),
            selected_port: String::new(),
            baud_rate: "115200".to_owned(),
            send_text: String::new(),
            script_window_open: false,
            script_text: espscript::default_script_text(),
            script_delay_ms: "80".to_owned(),
            script_strict_mode: true,
            script_running: false,
            script_stop: None,
            script_response_tx: None,
            firmware_caps: None,
            caps_line_buffer: String::new(),
            caps_query_pending: false,
            caps_query_started: None,
            board: BoardInfo::default(),
            board_status: "не проверяли".to_owned(),
            board_status_color: TEXT_DIM,
            command_running: None,
            last_command_output: String::new(),
            monitor_state: MonitorState::Stopped,
            monitor_stop: None,
            monitor_tx: None,
            serial_log: String::new(),
            activity: Vec::new(),
            events_tx,
            events_rx,
        };

        app.refresh_ports();
        app.log("Приложение запущено");
        app
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                AppEvent::CommandFinished { kind, ok, output } => {
                    self.command_running = None;
                    self.last_command_output = output.clone();

                    if ok {
                        self.log(format!("{}: готово", kind.title()));
                    } else {
                        self.log(format!("{}: ошибка", kind.title()));
                    }

                    match kind {
                        CommandKind::BoardInfo => self.finish_board_info(ok, &output),
                        CommandKind::Reset => {
                            if ok {
                                self.board_status = "reset выполнен".to_owned();
                                self.board_status_color = GOOD;
                            } else {
                                self.board_status = "reset не прошел".to_owned();
                                self.board_status_color = BAD;
                            }
                        }
                    }
                }
                AppEvent::MonitorStarted => {
                    self.monitor_state = MonitorState::Running;
                    self.log("Serial monitor открыт");
                    self.query_firmware_caps();
                }
                AppEvent::MonitorStopped => {
                    self.monitor_state = MonitorState::Stopped;
                    self.monitor_stop = None;
                    self.monitor_tx = None;
                    self.caps_query_pending = false;
                    self.caps_query_started = None;
                    if let Some(stop) = &self.script_stop {
                        stop.store(true, Ordering::SeqCst);
                    }
                    self.log("Serial monitor закрыт");
                }
                AppEvent::SerialData(data) => {
                    self.capture_firmware_caps(&data);
                    self.forward_script_serial(&data);
                    self.append_serial(&data);
                }
                AppEvent::SerialEcho(data) => self.append_serial(&data),
                AppEvent::SerialError(error) => {
                    self.log(format!("Serial: {error}"));
                    self.append_serial(&format!("\n[serial error] {error}\n"));
                }
                AppEvent::ScriptFinished { ok, sent, message } => {
                    self.script_running = false;
                    self.script_stop = None;
                    self.script_response_tx = None;
                    if ok {
                        self.board_status = "script отправлен".to_owned();
                        self.board_status_color = GOOD;
                        self.log(format!("Скрипт: {sent} команд отправлено"));
                    } else if message.contains("останов") {
                        self.board_status = "script stopped".to_owned();
                        self.board_status_color = WARN;
                        self.log(format!("Скрипт: {message}, отправлено команд: {sent}"));
                    } else {
                        self.board_status = "script ошибка".to_owned();
                        self.board_status_color = BAD;
                        self.log(format!("Скрипт: {message}"));
                    }
                }
            }
        }

        if self
            .caps_query_started
            .is_some_and(|started| started.elapsed() >= Duration::from_secs(2))
        {
            self.caps_query_pending = false;
            self.caps_query_started = None;
            self.log("Firmware caps: timeout, можно повторить запрос");
        }
    }

    fn refresh_ports(&mut self) {
        self.ports = match serialport::available_ports() {
            Ok(ports) => ports.into_iter().map(port_entry).collect(),
            Err(error) => {
                self.log(format!("Не удалось получить список портов: {error}"));
                Vec::new()
            }
        };

        if self.selected_port.is_empty() || !self.ports.iter().any(|p| p.name == self.selected_port)
        {
            self.selected_port = self
                .ports
                .iter()
                .find(|p| p.is_usb)
                .or_else(|| self.ports.first())
                .map(|p| p.name.clone())
                .unwrap_or_default();
        }
    }

    fn finish_board_info(&mut self, ok: bool, output: &str) {
        if !ok {
            self.board_status = "нет связи".to_owned();
            self.board_status_color = BAD;
            return;
        }

        let board = parse_board_info(output);
        if board.is_empty() {
            self.board_status = "ответ не распознан".to_owned();
            self.board_status_color = WARN;
        } else {
            self.board = board;
            self.board_status = "ESP32 подключена".to_owned();
            self.board_status_color = GOOD;
        }
    }

    fn selected_port(&self) -> Option<String> {
        if self.selected_port.trim().is_empty() {
            None
        } else {
            Some(self.selected_port.trim().to_owned())
        }
    }

    fn start_board_check(&mut self) {
        let Some(port) = self.selected_port() else {
            self.log("Сначала выбери COM-порт");
            return;
        };

        if self.port_is_busy_by_monitor() {
            self.log("Останови serial monitor перед проверкой платы");
            return;
        }

        self.command_running = Some(CommandKind::BoardInfo);
        self.board_status = "проверяю...".to_owned();
        self.board_status_color = WARN;
        self.log(format!("Проверяю ESP32 на {port}"));

        run_espflash(
            self.events_tx.clone(),
            CommandKind::BoardInfo,
            vec![
                "board-info".to_owned(),
                "--chip".to_owned(),
                "esp32".to_owned(),
                "--port".to_owned(),
                port,
                "--non-interactive".to_owned(),
                "--skip-update-check".to_owned(),
            ],
        );
    }

    fn reset_board(&mut self) {
        let Some(port) = self.selected_port() else {
            self.log("Сначала выбери COM-порт");
            return;
        };

        if self.port_is_busy_by_monitor() {
            self.log("Останови serial monitor перед reset через espflash");
            return;
        }

        self.command_running = Some(CommandKind::Reset);
        self.board_status = "reset...".to_owned();
        self.board_status_color = WARN;
        self.log(format!("Reset ESP32 на {port}"));

        run_espflash(
            self.events_tx.clone(),
            CommandKind::Reset,
            vec![
                "reset".to_owned(),
                "--chip".to_owned(),
                "esp32".to_owned(),
                "--port".to_owned(),
                port,
                "--non-interactive".to_owned(),
                "--skip-update-check".to_owned(),
            ],
        );
    }

    fn start_monitor(&mut self) {
        let Some(port) = self.selected_port() else {
            self.log("Сначала выбери COM-порт");
            return;
        };

        let Ok(baud) = self.baud_rate.trim().parse::<u32>() else {
            self.log("Baud rate должен быть числом");
            return;
        };

        if self.monitor_state != MonitorState::Stopped {
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let (monitor_tx, monitor_rx) = mpsc::channel::<Vec<u8>>();
        self.monitor_stop = Some(stop.clone());
        self.monitor_tx = Some(monitor_tx);
        self.monitor_state = MonitorState::Starting;
        self.firmware_caps = None;
        self.caps_line_buffer.clear();
        self.caps_query_pending = false;
        self.caps_query_started = None;
        self.log(format!("Открываю serial monitor {port} @ {baud}"));

        start_monitor_thread(self.events_tx.clone(), port, baud, stop, monitor_rx);
    }

    fn stop_monitor(&mut self) {
        if let Some(stop) = &self.monitor_stop {
            stop.store(true, Ordering::SeqCst);
            self.monitor_state = MonitorState::Stopping;
            self.log("Останавливаю serial monitor");
        }
    }

    fn send_serial_line(&mut self) {
        if self.send_text.is_empty() {
            return;
        }

        let Some(tx) = self.ready_serial_sender() else {
            return;
        };

        let mut data = self.send_text.clone().into_bytes();
        data.extend_from_slice(b"\r\n");

        if tx.send(data).is_ok() {
            self.append_serial(&format!("\n> {}\n", self.send_text));
            self.send_text.clear();
        } else {
            self.log("Не удалось отправить строку в serial monitor");
        }
    }

    fn query_firmware_caps(&mut self) {
        let Some(tx) = self.ready_serial_sender() else {
            return;
        };

        if tx.send(b"caps\r\n".to_vec()).is_ok() {
            self.caps_query_pending = true;
            self.caps_query_started = Some(Instant::now());
            self.append_serial("\n> caps\n");
            self.log("Запрашиваю firmware caps");
        } else {
            self.log("Не удалось отправить caps в serial monitor");
        }
    }

    fn ready_serial_sender(&mut self) -> Option<mpsc::Sender<Vec<u8>>> {
        if self.monitor_state != MonitorState::Running {
            self.log("Открой serial monitor перед отправкой на плату");
            return None;
        }

        let Some(tx) = &self.monitor_tx else {
            self.log("Serial monitor не готов к отправке");
            return None;
        };

        Some(tx.clone())
    }

    fn send_script_raw(&mut self) {
        if self.script_running {
            self.log("Дождись завершения текущего скрипта");
            return;
        }

        let payload = normalize_script_payload(&self.script_text);
        if payload.is_empty() {
            self.log("Скрипт пустой");
            return;
        }

        let Some(tx) = self.ready_serial_sender() else {
            return;
        };

        let bytes = payload.into_bytes();
        let byte_count = bytes.len();
        if tx.send(bytes).is_ok() {
            self.append_serial(&format!("\n> [script raw: {byte_count} bytes]\n"));
            self.log(format!("Скрипт отправлен как текст: {byte_count} байт"));
        } else {
            self.log("Не удалось отправить скрипт в serial monitor");
        }
    }

    fn run_script_lines(&mut self) {
        if self.script_running {
            self.log("Скрипт уже выполняется");
            return;
        }

        let delay_ms = match self.script_delay_ms.trim().parse::<u64>() {
            Ok(delay_ms) => delay_ms,
            Err(_) => {
                self.log("Задержка между строками должна быть числом");
                return;
            }
        };

        let steps = match espscript::compile_script(&self.script_text) {
            Ok(steps) => steps,
            Err(error) => {
                self.board_status = "script parse error".to_owned();
                self.board_status_color = BAD;
                self.log(format!("EspScript: {error}"));
                return;
            }
        };

        if steps.is_empty() {
            self.log("Нет команд для отправки: скрипт пустой");
            return;
        }

        if let Some(error) = self.script_compatibility_error(&steps) {
            self.board_status = "caps mismatch".to_owned();
            self.board_status_color = BAD;
            self.log(error);
            return;
        }

        if self.firmware_caps.is_none() && has_explicit_requirements(&steps) {
            self.board_status = "caps required".to_owned();
            self.board_status_color = BAD;
            self.log("EspScript: requires(...) требует сначала проверить firmware caps");
            return;
        }

        if self.firmware_caps.is_none() {
            self.log("Firmware caps не проверены: отправляю без проверки совместимости");
        }

        let Some(tx) = self.ready_serial_sender() else {
            return;
        };

        let events_tx = self.events_tx.clone();
        let (response_tx, response_rx) = mpsc::channel::<String>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = script_stats(&steps);
        let total_commands = stats.commands;
        let total_expects = stats.expects;
        let total_branches = stats.branches;
        let strict_mode = self.script_strict_mode;
        let strict_done_markers = self
            .firmware_caps
            .as_ref()
            .is_some_and(|caps| caps.supports("script_done"));
        self.script_running = true;
        self.script_stop = Some(stop.clone());
        self.script_response_tx = Some(response_tx);
        self.board_status = "script running".to_owned();
        self.board_status_color = WARN;
        self.log(format!(
            "Запускаю EspScript: {total_commands} команд, {total_expects} expect, {total_branches} if, strict={}",
            if strict_mode { "on" } else { "off" }
        ));

        thread::spawn(move || {
            let mut sent = 0_usize;
            let mut serial_buffer = String::new();
            let mut last_strict_response = None;

            if let Err(message) = run_script_steps(
                &steps,
                &tx,
                &events_tx,
                &response_rx,
                stop.as_ref(),
                delay_ms,
                strict_mode,
                strict_done_markers,
                &mut serial_buffer,
                &mut sent,
                &mut last_strict_response,
            ) {
                let _ = events_tx.send(AppEvent::ScriptFinished {
                    ok: false,
                    sent,
                    message,
                });
                return;
            }

            let _ = events_tx.send(AppEvent::ScriptFinished {
                ok: true,
                sent,
                message: "готово".to_owned(),
            });
        });
    }

    fn clear_serial(&mut self) {
        self.serial_log.clear();
    }

    fn stop_script(&mut self) {
        if let Some(stop) = &self.script_stop {
            stop.store(true, Ordering::SeqCst);
            self.log("Останавливаю EspScript");
        }
    }

    fn port_is_busy_by_monitor(&self) -> bool {
        self.monitor_state != MonitorState::Stopped
    }

    fn forward_script_serial(&mut self, data: &str) {
        let Some(tx) = self.script_response_tx.as_ref().cloned() else {
            return;
        };

        if tx.send(data.to_owned()).is_err() {
            self.script_response_tx = None;
        }
    }

    fn append_serial(&mut self, data: &str) {
        self.serial_log.push_str(data);
        trim_string_to_last_bytes(&mut self.serial_log, 30_000);
    }

    fn capture_firmware_caps(&mut self, data: &str) {
        self.caps_line_buffer.push_str(data);

        while let Some(index) = self.caps_line_buffer.find('\n') {
            let line: String = self.caps_line_buffer.drain(..=index).collect();
            self.handle_caps_line(line.trim());
        }

        trim_string_to_last_bytes(&mut self.caps_line_buffer, 1_000);
    }

    fn handle_caps_line(&mut self, line: &str) {
        if let Some(caps) = parse_firmware_caps(line) {
            let summary = caps.title();
            self.firmware_caps = Some(caps);
            self.caps_query_pending = false;
            self.caps_query_started = None;
            self.board_status = format!("fw {summary}");
            self.board_status_color = GOOD;
            self.log(format!("Firmware caps: {summary}"));
            return;
        }

        if self.caps_query_pending && line.contains("unknown_command") && line.contains("caps") {
            self.caps_query_pending = false;
            self.caps_query_started = None;
            self.firmware_caps = None;
            self.board_status = "caps unsupported".to_owned();
            self.board_status_color = WARN;
            self.log("Прошивка не поддерживает caps; совместимость нельзя проверить");
        }
    }

    fn script_compatibility_error(&self, steps: &[ScriptStep]) -> Option<String> {
        let caps = self.firmware_caps.as_ref()?;
        let missing = missing_firmware_features(steps, caps);
        if missing.is_empty() {
            None
        } else {
            Some(format!(
                "Firmware {} не сообщает поддержку: {}",
                caps.title(),
                missing.join(", ")
            ))
        }
    }

    fn log(&mut self, message: impl Into<String>) {
        self.activity
            .push(format!("{}  {}", time_stamp(), message.into()));
        if self.activity.len() > 300 {
            self.activity.drain(0..self.activity.len() - 300);
        }
    }

    fn draw_header(&mut self, ctx: &Context) {
        TopBottomPanel::top("header")
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(26.0, 18.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Lumen et Signum")
                                .font(FontId::proportional(30.0))
                                .color(TEXT)
                                .strong(),
                        );
                        ui.add_space(5.0);
                        ui.label(
                            RichText::new(
                                "Минималистичная панель для проверки платы, UART-мониторинга и EspScript",
                            )
                            .color(TEXT_DIM),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let monitor_pulsate = self.monitor_state == MonitorState::Running;
                        pulsating_pill(
                            ui,
                            self.monitor_state.label(),
                            self.monitor_state.color(),
                            monitor_pulsate,
                        );
                        ui.add_space(8.0);

                        let command_pulsate = self.command_running.is_some();
                        pulsating_pill(
                            ui,
                            &self.board_status,
                            self.board_status_color,
                            command_pulsate,
                        );
                        ui.add_space(8.0);

                        let port = if self.selected_port.is_empty() {
                            "нет порта"
                        } else {
                            &self.selected_port
                        };
                        pill(ui, port, ACCENT);
                    });
                });

                ui.add_space(8.0);
                let (rect, _response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(rect, Rounding::ZERO, BORDER_SOFT);
            });
    }

    fn draw_side_panel(&mut self, ctx: &Context) {
        SidePanel::left("side")
            .exact_width(326.0)
            .frame(
                Frame::none()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(18.0, 16.0)),
            )
            .show(ctx, |ui| {
                card(ui, "Подключение", ACCENT, |ui| {
                    ui.label(RichText::new("COM-порт").color(TEXT_DIM));
                    ui.add_space(5.0);
                    ComboBox::from_id_salt("port_selector")
                        .width(ui.available_width())
                        .selected_text(if self.selected_port.is_empty() {
                            "порт не найден".to_owned()
                        } else {
                            self.selected_port.clone()
                        })
                        .show_ui(ui, |ui| {
                            for port in &self.ports {
                                ui.selectable_value(
                                    &mut self.selected_port,
                                    port.name.clone(),
                                    format!("{}  {}", port.name, port.detail),
                                );
                            }
                        });

                    ui.add_space(10.0);
                    if ui
                        .add_sized([ui.available_width(), 36.0], Button::new("Обновить порты"))
                        .clicked()
                    {
                        self.refresh_ports();
                        self.log("Список портов обновлен");
                    }

                    ui.add_space(12.0);
                    for port in &self.ports {
                        ui.label(
                            RichText::new(format!("{}  {}", port.name, port.detail))
                                .small()
                                .color(if port.name == self.selected_port {
                                    ACCENT
                                } else {
                                    TEXT_DIM
                                }),
                        );
                    }
                });

                ui.add_space(14.0);

                card(ui, "Действия", ACCENT_2, |ui| {
                    let command_busy = self.command_running.is_some();
                    let port_busy = self.port_is_busy_by_monitor();

                    ui.add_enabled_ui(!command_busy && !port_busy, |ui| {
                        if ui
                            .add_sized(
                                [ui.available_width(), 42.0],
                                Button::new(RichText::new("Проверить плату").strong())
                                    .fill(ACCENT.linear_multiply(0.18)),
                            )
                            .clicked()
                        {
                            self.start_board_check();
                        }

                        ui.add_space(8.0);

                        if ui
                            .add_sized(
                                [ui.available_width(), 38.0],
                                Button::new("Reset через espflash"),
                            )
                            .clicked()
                        {
                            self.reset_board();
                        }
                    });

                    ui.add_space(8.0);

                    if ui
                        .add_sized(
                            [ui.available_width(), 38.0],
                            Button::new(RichText::new("Открыть скрипты").strong())
                                .fill(ACCENT_2.linear_multiply(0.18)),
                        )
                        .clicked()
                    {
                        self.script_window_open = true;
                    }

                    if command_busy {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.add_space(4.0);
                            ui.label(RichText::new("Команда выполняется...").color(WARN));
                        });
                    }

                    if port_busy {
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Остановите монитор перед прошивкой.")
                                    .small()
                                    .color(WARN),
                            );
                        });
                    }
                });

                ui.add_space(14.0);

                card(ui, "Serial", GOOD, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Baud rate").color(TEXT_DIM));
                        ui.add_sized([92.0, 24.0], TextEdit::singleline(&mut self.baud_rate));
                    });

                    ui.add_space(10.0);

                    match self.monitor_state {
                        MonitorState::Stopped => {
                            if ui
                                .add_sized(
                                    [ui.available_width(), 40.0],
                                    Button::new(RichText::new("Открыть monitor").strong())
                                        .fill(GOOD.linear_multiply(0.18)),
                                )
                                .clicked()
                            {
                                self.start_monitor();
                            }
                        }
                        MonitorState::Starting | MonitorState::Running => {
                            if ui
                                .add_sized(
                                    [ui.available_width(), 40.0],
                                    Button::new(RichText::new("Закрыть monitor").strong())
                                        .fill(BAD.linear_multiply(0.18)),
                                )
                                .clicked()
                            {
                                self.stop_monitor();
                            }
                        }
                        MonitorState::Stopping => {
                            ui.add_enabled(
                                false,
                                Button::new("Закрываю monitor...")
                                    .min_size(Vec2::new(ui.available_width(), 40.0)),
                            );
                        }
                    }

                    ui.add_space(10.0);
                    if ui
                        .add_enabled(
                            self.monitor_state == MonitorState::Running && !self.caps_query_pending,
                            Button::new(if self.caps_query_pending {
                                "Проверяю firmware..."
                            } else {
                                "Проверить firmware caps"
                            })
                            .min_size(Vec2::new(ui.available_width(), 32.0)),
                        )
                        .clicked()
                    {
                        self.query_firmware_caps();
                    }

                    ui.add_space(10.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            Button::new("Очистить serial log"),
                        )
                        .clicked()
                    {
                        self.clear_serial();
                    }
                });
            });
    }

    fn draw_main_panel(&mut self, ctx: &Context) {
        CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin::same(18.0)))
            .show(ctx, |ui| {
                ui.columns(2, |columns| {
                    columns[0].vertical(|ui| {
                        card(ui, "Состояние платы", ACCENT, |ui| {
                            let firmware_title = self
                                .firmware_caps
                                .as_ref()
                                .map(FirmwareCaps::title)
                                .unwrap_or_else(|| "не проверено".to_owned());
                            let firmware_protocol = self
                                .firmware_caps
                                .as_ref()
                                .map(FirmwareCaps::protocol_label)
                                .unwrap_or_else(|| "-".to_owned());
                            egui::Grid::new("metrics_grid")
                                .num_columns(2)
                                .spacing([12.0, 12.0])
                                .show(ui, |ui| {
                                    metric(ui, "Микроконтроллер", empty_dash(&self.board.chip), ACCENT);
                                    metric(ui, "MAC-адрес", empty_dash(&self.board.mac), GOOD);
                                    ui.end_row();
                                    metric(ui, "Flash-память", empty_dash(&self.board.flash), ACCENT_2);
                                    metric(ui, "Кварц (Clock)", empty_dash(&self.board.crystal), WARN);
                                    ui.end_row();
                                    metric(ui, "Прошивка", &firmware_title, GOOD);
                                    metric(ui, "Caps protocol", &firmware_protocol, ACCENT_2);
                                    ui.end_row();
                                });

                            ui.add_space(14.0);
                            section_label(ui, "Поддерживаемые функции");
                            ui.add_space(4.0);
                            let features_frame = surface_frame();
                            features_frame.show(ui, |ui| {
                                ui.label(
                                    RichText::new(empty_dash(&self.board.features))
                                        .color(TEXT)
                                        .monospace(),
                                );
                            });

                            ui.add_space(10.0);
                            section_label(ui, "Firmware Caps");
                            ui.add_space(4.0);
                            let caps_frame = surface_frame();
                            caps_frame.show(ui, |ui| {
                                ui.label(
                                    RichText::new(firmware_caps_text(self.firmware_caps.as_ref()))
                                        .color(TEXT)
                                        .monospace(),
                                );
                            });

                            ui.add_space(10.0);
                            section_label(ui, "Безопасность платы");
                            ui.add_space(4.0);
                            let security_frame = surface_frame();
                            security_frame.show(ui, |ui| {
                                ui.label(
                                    RichText::new(empty_dash(&self.board.security))
                                        .color(TEXT)
                                        .monospace(),
                                );
                            });
                        });

                        ui.add_space(14.0);

                        card(ui, "Журнал событий", ACCENT_2, |ui| {
                            ScrollArea::vertical()
                                .id_salt("activity_log_scroll")
                                .max_height(235.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    if self.activity.is_empty() {
                                        ui.label(RichText::new("Пока пусто").color(TEXT_DIM));
                                    } else {
                                        ui.vertical(|ui| {
                                            for line in &self.activity {
                                                ui.horizontal(|ui| {
                                                    ui.label(RichText::new("•").color(BORDER));
                                                    ui.label(RichText::new(line).monospace().color(TEXT_DIM));
                                                });
                                            }
                                        });
                                    }
                                });
                        });
                    });

                    columns[1].vertical(|ui| {
                        card(ui, "Serial Monitor", GOOD, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [ui.available_width() - 110.0, 28.0],
                                    TextEdit::singleline(&mut self.send_text)
                                        .hint_text("Команда в UART, например: status"),
                                );

                                let can_send = self.monitor_state == MonitorState::Running;
                                if ui
                                    .add_enabled(
                                        can_send,
                                        Button::new("Send").min_size(Vec2::new(90.0, 28.0)),
                                    )
                                    .clicked()
                                  {
                                      self.send_serial_line();
                                  }
                            });

                            ui.add_space(10.0);

                            let serial_frame = terminal_frame();

                            serial_frame.show(ui, |ui| {
                                ScrollArea::vertical()
                                    .id_salt("serial_monitor_scroll")
                                    .max_height(375.0)
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        if self.serial_log.is_empty() {
                                            ui.label(
                                                RichText::new(
                                                    "Запустите монитор, чтобы видеть вывод ESP32...",
                                                )
                                                .color(TEXT_DIM)
                                                .italics(),
                                            );
                                        } else {
                                            ui.vertical(|ui| {
                                                for line in self.serial_log.lines() {
                                                    let text = RichText::new(line).monospace();
                                                    let text = if line.starts_with('>') {
                                                        text.color(ACCENT).strong()
                                                    } else if line.contains("error") || line.contains("ERROR") || line.contains("failed") || line.contains("[serial error]") {
                                                        text.color(BAD)
                                                    } else if line.contains("warn") || line.contains("WARN") || line.contains("warning") {
                                                        text.color(WARN)
                                                    } else if line.contains("info") || line.contains("INFO") {
                                                        text.color(GOOD)
                                                    } else {
                                                        text.color(TEXT)
                                                    };
                                                    ui.label(text);
                                                }
                                            });
                                        }
                                    });
                            });
                        });

                        ui.add_space(14.0);

                        card(ui, "Последняя команда", WARN, |ui| {
                            let cmd_frame = terminal_frame();

                            cmd_frame.show(ui, |ui| {
                                ScrollArea::vertical()
                                    .id_salt("last_command_scroll")
                                    .max_height(190.0)
                                    .stick_to_bottom(true)
                                    .show(ui, |ui| {
                                        if self.last_command_output.is_empty() {
                                            ui.label(
                                                RichText::new("Вывод espflash появится здесь...")
                                                    .color(TEXT_DIM)
                                                    .italics(),
                                            );
                                        } else {
                                            ui.vertical(|ui| {
                                                for line in self.last_command_output.lines() {
                                                    let text = RichText::new(line).monospace();
                                                    let text = if line.contains("error") || line.contains("ERROR") || line.contains("failed") || line.contains("Failed") {
                                                        text.color(BAD)
                                                    } else if line.contains("warn") || line.contains("WARN") || line.contains("Warning") {
                                                        text.color(WARN)
                                                    } else if line.contains("info") || line.contains("INFO") || line.contains("Success") || line.contains("success") {
                                                        text.color(GOOD)
                                                    } else {
                                                        text.color(TEXT)
                                                    };
                                                    ui.label(text);
                                                }
                                            });
                                        }
                                    });
                            });
                        });
                    });
                });
            });
    }

    fn draw_scripts_window(&mut self, ctx: &Context) {
        if !self.script_window_open {
            return;
        }

        let mut open = self.script_window_open;
        egui::Window::new("EspScript")
            .id(egui::Id::new("script_editor_window"))
            .open(&mut open)
            .default_size(Vec2::new(780.0, 620.0))
            .min_width(620.0)
            .resizable(true)
            .show(ctx, |ui| {
                let monitor_ready = self.monitor_state == MonitorState::Running;
                ui.horizontal(|ui| {
                    pill(
                        ui,
                        if monitor_ready { "UART готов" } else { "UART закрыт" },
                        if monitor_ready { GOOD } else { WARN },
                    );
                    ui.label(
                        RichText::new(
                                "EspScript компилируется в UART-команды. Типы: ms/hz/volt/pin. Совместимость: requires(feature: pwm_real).",
                        )
                        .color(TEXT_DIM),
                    );
                });

                ui.add_space(12.0);

                terminal_frame().show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut self.script_text)
                                .id_source("script_editor_text")
                                .font(FontId::monospace(13.0))
                                .desired_rows(18)
                                .desired_width(f32::INFINITY)
                                .code_editor(),
                        );
                    });

                ui.add_space(10.0);

                let compiled_preview = espscript::compile_script(&self.script_text);
                let (stats, parse_error) = match compiled_preview.as_ref() {
                    Ok(steps) => (script_stats(steps), None),
                    Err(error) => (ScriptStats::default(), Some(error.as_str())),
                };
                let compatibility_error = compiled_preview
                    .as_ref()
                    .ok()
                    .and_then(|steps| self.script_compatibility_error(steps));
                let required_features = compiled_preview
                    .as_ref()
                    .ok()
                    .map(|steps| required_firmware_features(steps))
                    .unwrap_or_default();
                let has_explicit_requirements = compiled_preview
                    .as_ref()
                    .ok()
                    .is_some_and(|steps| has_explicit_requirements(steps));
                let lint_warnings = compiled_preview
                    .as_ref()
                    .ok()
                    .map(|steps| espscript::lint_script(&self.script_text, steps))
                    .unwrap_or_default();
                let raw_bytes = normalize_script_payload(&self.script_text).len();

                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Команд: {}", stats.commands)).color(TEXT_DIM));
                    ui.label(RichText::new(format!("Wait: {}", stats.waits)).color(TEXT_DIM));
                    ui.label(RichText::new(format!("Expect: {}", stats.expects)).color(TEXT_DIM));
                    ui.label(RichText::new(format!("If: {}", stats.branches)).color(TEXT_DIM));
                    ui.label(
                        RichText::new(format!("Features: {}", required_features.len()))
                            .color(TEXT_DIM),
                    );
                    ui.label(
                        RichText::new(format!("Warnings: {}", lint_warnings.len()))
                            .color(if lint_warnings.is_empty() { TEXT_DIM } else { WARN }),
                    );
                    ui.label(RichText::new(format!("Raw: {raw_bytes} байт")).color(TEXT_DIM));
                    ui.add_space(16.0);
                    ui.label(RichText::new("Delay ms").color(TEXT_DIM));
                    ui.add_sized(
                        [72.0, 24.0],
                        TextEdit::singleline(&mut self.script_delay_ms),
                    );
                    ui.add_space(8.0);
                    ui.add_enabled_ui(!self.script_running, |ui| {
                        ui.checkbox(&mut self.script_strict_mode, "Strict ok/err");
                    });

                    if self.script_running {
                        ui.add_space(8.0);
                        ui.spinner();
                        ui.label(RichText::new("отправка...").color(WARN));
                    }
                });

                if let Some(error) = parse_error {
                    ui.label(RichText::new(format!("Ошибка EspScript: {error}")).color(BAD).small());
                } else if let Some(error) = &compatibility_error {
                    ui.label(RichText::new(format!("Несовместимо с firmware: {error}")).color(BAD).small());
                    if !required_features.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Скрипт требует: {}",
                                required_features.join(", ")
                            ))
                            .color(TEXT_DIM)
                            .small(),
                        );
                    }
                } else if self.firmware_caps.is_none() && has_explicit_requirements {
                    ui.label(
                        RichText::new("Скрипт содержит requires(...): сначала проверь firmware caps")
                            .color(BAD)
                            .small(),
                    );
                    if !required_features.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Скрипт требует: {}",
                                required_features.join(", ")
                            ))
                            .color(TEXT_DIM)
                            .small(),
                        );
                    }
                } else if self.firmware_caps.is_none() {
                    ui.label(
                        RichText::new("Firmware caps не проверены: запуск разрешен, но совместимость неизвестна")
                            .color(WARN)
                            .small(),
                    );
                    if !required_features.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Скрипт требует: {}",
                                required_features.join(", ")
                            ))
                            .color(TEXT_DIM)
                            .small(),
                        );
                    }
                } else if !required_features.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "Совместимо. Скрипт требует: {}",
                            required_features.join(", ")
                        ))
                        .color(GOOD)
                        .small(),
                    );
                }

                if !lint_warnings.is_empty() {
                    for warning in lint_warnings.iter().take(6) {
                        ui.label(
                            RichText::new(format!(
                                "warning L{:03}: {}",
                                warning.line, warning.message
                            ))
                            .color(WARN)
                            .small(),
                        );
                    }

                    if lint_warnings.len() > 6 {
                        ui.label(
                            RichText::new(format!("... еще {} warnings", lint_warnings.len() - 6))
                                .color(TEXT_DIM)
                                .small(),
                        );
                    }
                }

                ui.add_space(8.0);
                terminal_frame().show(ui, |ui| {
                        ui.label(
                            RichText::new("UART preview компиляции")
                                .color(TEXT_DIM)
                                .small(),
                        );
                        ui.label(
                            RichText::new(
                                "L### - строка EspScript; uart уйдет на плату; wait/expect/if contains выполняет приложение",
                            )
                            .color(TEXT_DIM)
                            .small(),
                        );
                        ui.add_space(4.0);
                        ScrollArea::vertical()
                            .id_salt("script_compile_preview_scroll")
                            .max_height(150.0)
                            .show(ui, |ui| match compiled_preview.as_ref() {
                                Ok(steps) if steps.is_empty() => {
                                    ui.label(RichText::new("Нет шагов").color(TEXT_DIM).italics());
                                }
                                Ok(steps) => {
                                    let preview_lines = build_script_preview(steps, 120);
                                    for (line, color) in &preview_lines {
                                        ui.label(RichText::new(line).monospace().color(*color));
                                    }

                                    if stats.preview_lines > preview_lines.len() {
                                        ui.label(
                                            RichText::new(format!(
                                                "... еще {} шагов",
                                                stats.preview_lines - preview_lines.len()
                                            ))
                                            .color(TEXT_DIM)
                                            .italics(),
                                        );
                                    }
                                }
                                Err(_) => {
                                    ui.label(
                                        RichText::new("Preview недоступен, пока есть ошибка")
                                            .color(TEXT_DIM)
                                            .italics(),
                                    );
                                }
                            });
                    });

                ui.add_space(12.0);

                ui.horizontal_wrapped(|ui| {
                    let can_send = monitor_ready
                        && !self.script_running
                        && compatibility_error.is_none()
                        && !(self.firmware_caps.is_none() && has_explicit_requirements);
                    if self.script_running
                        && ui
                            .add(
                                Button::new(RichText::new("Остановить скрипт").strong())
                                    .fill(BAD.linear_multiply(0.18)),
                            )
                            .clicked()
                    {
                        self.stop_script();
                    }

                    if ui
                        .add_enabled(
                            can_send,
                            Button::new(RichText::new("Запустить EspScript").strong())
                                .fill(GOOD.linear_multiply(0.18)),
                        )
                        .clicked()
                    {
                        self.run_script_lines();
                    }

                    if ui
                        .add_enabled(can_send, Button::new("Отправить как есть"))
                        .clicked()
                    {
                        self.send_script_raw();
                    }

                    if ui.button("Пример").clicked() {
                        self.script_text = espscript::default_script_text();
                    }

                    if ui.button("Format").clicked() {
                        self.script_text = espscript::format_script(&self.script_text);
                        self.log("EspScript отформатирован");
                    }

                    if ui.button("Очистить").clicked() {
                        self.script_text.clear();
                    }

                    if !monitor_ready {
                        ui.label(
                            RichText::new("Открой monitor слева, чтобы отправлять на плату")
                                .color(WARN)
                                .small(),
                        );
                    }
                });
            });

        self.script_window_open = open;
    }
}

impl eframe::App for EspApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        self.draw_header(ctx);
        self.draw_side_panel(ctx);
        self.draw_main_panel(ctx);
        self.draw_scripts_window(ctx);

        if self.monitor_state != MonitorState::Stopped
            || self.command_running.is_some()
            || self.script_running
        {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }
}

fn configure_style(ctx: &Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.window_fill = BG;
    style.visuals.panel_fill = PANEL;
    style.visuals.extreme_bg_color = TERMINAL;
    style.visuals.faint_bg_color = SURFACE;
    style.visuals.code_bg_color = TERMINAL;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.hyperlink_color = ACCENT;

    // Custom widget roundings
    style.visuals.widgets.noninteractive.rounding = Rounding::same(14.0);
    style.visuals.widgets.inactive.rounding = Rounding::same(10.0);
    style.visuals.widgets.hovered.rounding = Rounding::same(10.0);
    style.visuals.widgets.active.rounding = Rounding::same(10.0);

    // Custom noninteractive / background shapes (e.g. Card fallback)
    style.visuals.widgets.noninteractive.bg_fill = CARD;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER_SOFT);

    // Inactive elements (e.g. standard buttons in default state)
    style.visuals.widgets.inactive.bg_fill = CARD_SOFT;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);

    // Hovered elements
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(31, 38, 54);
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);

    // Active elements (when clicked/selected)
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(37, 46, 66);
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.5, TEXT);

    // Text selections
    style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.25);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Spacing
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);

    // Font settings
    use egui::{FontFamily, FontId, TextStyle};
    let mut text_styles = std::collections::BTreeMap::new();
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    text_styles.insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles = text_styles;

    ctx.set_style(style);
}

fn surface_frame() -> Frame {
    Frame::none()
        .fill(SURFACE)
        .rounding(Rounding::same(12.0))
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .inner_margin(Margin::same(12.0))
}

fn terminal_frame() -> Frame {
    Frame::none()
        .fill(TERMINAL)
        .rounding(Rounding::same(12.0))
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .inner_margin(Margin::same(12.0))
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(TEXT_DIM));
}

fn card<R>(
    ui: &mut egui::Ui,
    title: &str,
    color: Color32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    Frame::none()
        .fill(CARD)
        .rounding(Rounding::same(18.0))
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .shadow(egui::Shadow {
            offset: Vec2::new(0.0, 10.0),
            blur: 24.0,
            spread: 0.0,
            color: Color32::from_black_alpha(28),
        })
        .inner_margin(Margin::same(18.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _response) =
                    ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(title)
                        .font(FontId::proportional(16.5))
                        .color(TEXT)
                        .strong(),
                );
            });
            ui.add_space(12.0);
            add(ui)
        })
        .inner
}

fn metric(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    Frame::none()
        .fill(SURFACE)
        .rounding(Rounding::same(14.0))
        .stroke(Stroke::new(1.0, BORDER_SOFT))
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.set_min_width(120.0);
            ui.horizontal(|ui| {
                let (rect, _response) =
                    ui.allocate_exact_size(Vec2::new(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 3.5, color);
                ui.add_space(7.0);
                ui.label(RichText::new(label).small().color(TEXT_DIM));
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(value)
                    .font(FontId::monospace(14.0))
                    .color(TEXT)
                    .strong(),
            );
        });
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    pulsating_pill(ui, text, color, false);
}

fn pulsating_pill(ui: &mut egui::Ui, text: &str, color: Color32, pulsate: bool) {
    let alpha = if pulsate {
        let time = ui.ctx().input(|i| i.time);
        (((time * 4.0).sin() + 1.0) / 2.0 * 0.2 + 0.12) as f32
    } else {
        0.13
    };

    let stroke_alpha = if pulsate {
        let time = ui.ctx().input(|i| i.time);
        (((time * 4.0).sin() + 1.0) / 2.0 * 0.4 + 0.4) as f32
    } else {
        0.45
    };

    Frame::none()
        .fill(color.linear_multiply(alpha))
        .rounding(Rounding::same(999.0))
        .stroke(Stroke::new(1.0, color.linear_multiply(stroke_alpha)))
        .inner_margin(Margin::symmetric(12.0, 5.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if pulsate {
                    let (rect, _response) =
                        ui.allocate_exact_size(Vec2::new(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                    ui.add_space(4.0);
                }
                ui.label(RichText::new(text).color(color).strong().small());
            });
        });
}

fn port_entry(info: SerialPortInfo) -> PortEntry {
    match info.port_type {
        SerialPortType::UsbPort(usb) => {
            let product = usb.product.unwrap_or_else(|| "USB serial".to_owned());
            let manufacturer = usb.manufacturer.unwrap_or_default();
            let manufacturer = if manufacturer.is_empty() {
                String::new()
            } else {
                format!(" / {manufacturer}")
            };

            PortEntry {
                name: info.port_name,
                detail: format!(
                    "USB {:04X}:{:04X} {product}{manufacturer}",
                    usb.vid, usb.pid
                ),
                is_usb: true,
            }
        }
        SerialPortType::BluetoothPort => PortEntry {
            name: info.port_name,
            detail: "Bluetooth serial".to_owned(),
            is_usb: false,
        },
        SerialPortType::PciPort => PortEntry {
            name: info.port_name,
            detail: "PCI serial".to_owned(),
            is_usb: false,
        },
        SerialPortType::Unknown => PortEntry {
            name: info.port_name,
            detail: "Serial".to_owned(),
            is_usb: false,
        },
    }
}

fn run_espflash(tx: mpsc::Sender<AppEvent>, kind: CommandKind, args: Vec<String>) {
    thread::spawn(move || {
        let output = build_command("espflash", &args).output();
        let (ok, text) = match output {
            Ok(output) => {
                let mut text = String::new();
                text.push_str(&String::from_utf8_lossy(&output.stdout));
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                (output.status.success(), text)
            }
            Err(error) => (
                false,
                format!("Не удалось запустить espflash. Проверь PATH.\n{error}"),
            ),
        };

        let _ = tx.send(AppEvent::CommandFinished {
            kind,
            ok,
            output: text,
        });
    });
}

fn build_command(program: &str, args: &[String]) -> Command {
    let mut command = Command::new(program);
    command.args(args);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    command
}

fn start_monitor_thread(
    tx: mpsc::Sender<AppEvent>,
    port: String,
    baud: u32,
    stop: Arc<AtomicBool>,
    monitor_rx: mpsc::Receiver<Vec<u8>>,
) {
    thread::spawn(move || {
        let open_result = serialport::new(&port, baud)
            .timeout(Duration::from_millis(80))
            .open();

        let mut serial = match open_result {
            Ok(serial) => serial,
            Err(error) => {
                let _ = tx.send(AppEvent::SerialError(format!(
                    "не удалось открыть {port}: {error}"
                )));
                let _ = tx.send(AppEvent::MonitorStopped);
                return;
            }
        };

        let _ = tx.send(AppEvent::MonitorStarted);
        let mut buffer = [0_u8; 512];

        while !stop.load(Ordering::SeqCst) {
            while let Ok(data) = monitor_rx.try_recv() {
                if let Err(error) = serial.write_all(&data) {
                    let _ = tx.send(AppEvent::SerialError(format!("write failed: {error}")));
                    break;
                }
            }

            match serial.read(&mut buffer) {
                Ok(0) => {}
                Ok(size) => {
                    let text = String::from_utf8_lossy(&buffer[..size]).into_owned();
                    let _ = tx.send(AppEvent::SerialData(text));
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => {
                    let _ = tx.send(AppEvent::SerialError(error.to_string()));
                    break;
                }
            }
        }

        let _ = tx.send(AppEvent::MonitorStopped);
    });
}

fn parse_board_info(output: &str) -> BoardInfo {
    let mut board = BoardInfo::default();

    for line in output.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Chip type:") {
            board.chip = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("Crystal frequency:") {
            board.crystal = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("Flash size:") {
            board.flash = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("Features:") {
            board.features = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("MAC address:") {
            board.mac = value.trim().to_owned();
        } else if let Some(value) = line.strip_prefix("Security features:") {
            board.security = value.trim().to_owned();
        }
    }

    board
}

fn parse_firmware_caps(line: &str) -> Option<FirmwareCaps> {
    let rest = line.strip_prefix("ok caps ")?;
    let mut caps = FirmwareCaps {
        raw: line.to_owned(),
        ..Default::default()
    };

    for field in rest.split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };

        match name {
            "name" => caps.name = value.to_owned(),
            "version" => caps.version = value.to_owned(),
            "protocol" => caps.protocol = value.to_owned(),
            "features" => {
                caps.features = value
                    .split(',')
                    .filter(|feature| !feature.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            _ => {}
        }
    }

    Some(caps)
}

fn firmware_caps_text(caps: Option<&FirmwareCaps>) -> String {
    let Some(caps) = caps else {
        return "не проверено".to_owned();
    };

    if caps.raw.is_empty() {
        caps.title()
    } else {
        caps.raw.clone()
    }
}

#[derive(Default)]
struct ScriptStats {
    commands: usize,
    waits: usize,
    expects: usize,
    branches: usize,
    preview_lines: usize,
}

fn script_stats(steps: &[ScriptStep]) -> ScriptStats {
    let mut stats = ScriptStats::default();

    for step in steps {
        stats.preview_lines += 1;
        match step {
            ScriptStep::Send { .. } => stats.commands += 1,
            ScriptStep::Requires { .. } => {}
            ScriptStep::Wait { .. } => stats.waits += 1,
            ScriptStep::Expect { .. } => stats.expects += 1,
            ScriptStep::IfContains {
                then_steps,
                else_steps,
                ..
            } => {
                stats.branches += 1;
                let then_stats = script_stats(then_steps);
                let else_stats = script_stats(else_steps);
                stats.commands += then_stats.commands + else_stats.commands;
                stats.waits += then_stats.waits + else_stats.waits;
                stats.expects += then_stats.expects + else_stats.expects;
                stats.branches += then_stats.branches + else_stats.branches;
                stats.preview_lines += then_stats.preview_lines + else_stats.preview_lines;
                if !else_steps.is_empty() {
                    stats.preview_lines += 1;
                }
            }
        }
    }

    stats
}

fn missing_firmware_features(steps: &[ScriptStep], caps: &FirmwareCaps) -> Vec<String> {
    let required = required_firmware_features(steps);
    required
        .into_iter()
        .filter(|feature| !caps.supports(feature))
        .collect()
}

fn has_explicit_requirements(steps: &[ScriptStep]) -> bool {
    steps.iter().any(|step| match step {
        ScriptStep::Requires { features, .. } => !features.is_empty(),
        ScriptStep::IfContains {
            then_steps,
            else_steps,
            ..
        } => has_explicit_requirements(then_steps) || has_explicit_requirements(else_steps),
        ScriptStep::Send { .. } | ScriptStep::Wait { .. } | ScriptStep::Expect { .. } => false,
    })
}

fn required_firmware_features(steps: &[ScriptStep]) -> Vec<String> {
    let mut required = Vec::new();
    collect_required_features(steps, &mut required);
    required.sort_unstable();
    required.dedup();
    required
}

fn collect_required_features(steps: &[ScriptStep], required: &mut Vec<String>) {
    for step in steps {
        match step {
            ScriptStep::Send { command, .. } => {
                add_required_features_for_command(command, required)
            }
            ScriptStep::Requires { features, .. } => {
                required.extend(features.iter().cloned());
            }
            ScriptStep::Wait { .. } | ScriptStep::Expect { .. } => {}
            ScriptStep::IfContains {
                then_steps,
                else_steps,
                ..
            } => {
                collect_required_features(then_steps, required);
                collect_required_features(else_steps, required);
            }
        }
    }
}

fn add_required_features_for_command(command: &str, required: &mut Vec<String>) {
    add_required_features_for_command_inner(command, required, 0);
}

fn add_required_features_for_command_inner(
    command: &str,
    required: &mut Vec<String>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }

    let command = command.trim();
    let first = command.split_whitespace().next().unwrap_or_default();

    match first {
        "status" => add_required_feature(required, "status"),
        "ping" => add_required_feature(required, "ping"),
        "help" => add_required_feature(required, "help"),
        "caps" => add_required_feature(required, "caps"),
        "vars" => add_required_feature(required, "vars"),
        "programs" => add_required_feature(required, "programs"),
        "save" => add_required_feature(required, "save"),
        "run" => add_required_feature(required, "run"),
        "delete" => add_required_feature(required, "delete"),
        "autorun" => add_required_feature(required, "autorun"),
        "boot" => add_required_feature(required, "boot"),
        "led" => add_required_feature(required, "led"),
        "blink" => add_required_feature(required, "blink"),
        "heartbeat" => add_required_feature(required, "heartbeat"),
        "echo" => add_required_feature(required, "echo"),
        "pin" => add_required_feature(required, "pin"),
        "pwm" => {
            add_required_feature(required, "pwm");
            add_required_feature(required, "pwm_real");
        }
        "adc" => add_required_feature(required, "adc"),
        "i2c" => add_required_feature(required, "i2c"),
        "spi" => add_required_feature(required, "spi"),
        "uart" => add_required_feature(required, "uart"),
        "wifi" => add_required_feature(required, "wifi"),
        "on" if command.starts_with("on pin ") => {
            add_required_feature(required, "on_pin");
            if command.contains(" debounce ") {
                add_required_feature(required, "on_pin_debounce");
            }
        }
        "timer" => {
            add_required_feature(required, "timer");
            if command.contains(" do {") {
                add_required_feature(required, "timer_do");
            }
        }
        "sleep" => add_required_feature(required, "sleep"),
        "if" => {
            add_required_feature(required, "board_if");
            add_required_features_for_if_condition(command, required);
        }
        "repeat" => add_required_feature(required, "repeat"),
        "let" => add_required_feature(required, "let"),
        _ => {}
    }

    collect_required_features_from_braces(command, required, depth + 1);
}

fn add_required_features_for_if_condition(command: &str, required: &mut Vec<String>) {
    let condition = command
        .strip_prefix("if ")
        .and_then(|value| value.split_once('{').map(|(condition, _)| condition.trim()))
        .unwrap_or_default();

    if condition.starts_with("led ") {
        add_required_feature(required, "led");
    } else if condition.starts_with("pin ") {
        add_required_feature(required, "pin");
    } else if condition.starts_with("heartbeat ") {
        add_required_feature(required, "heartbeat");
    } else if condition.starts_with("wifi ") {
        add_required_feature(required, "wifi");
    }
}

fn add_required_feature(required: &mut Vec<String>, feature: &str) {
    required.push(feature.to_owned());
}

fn collect_required_features_from_braces(command: &str, required: &mut Vec<String>, depth: usize) {
    let mut block_start = None;
    let mut brace_depth = 0_usize;

    for (index, byte) in command.bytes().enumerate() {
        match byte {
            b'{' => {
                if brace_depth == 0 {
                    block_start = Some(index + 1);
                }
                brace_depth += 1;
            }
            b'}' => {
                if brace_depth == 0 {
                    continue;
                }
                brace_depth -= 1;
                if brace_depth == 0
                    && let Some(start) = block_start.take()
                {
                    for nested in split_board_script_commands(&command[start..index]) {
                        add_required_features_for_command_inner(nested, required, depth);
                    }
                }
            }
            _ => {}
        }
    }
}

fn split_board_script_commands(script: &str) -> Vec<&str> {
    let mut commands = Vec::new();
    let mut start = 0_usize;
    let mut brace_depth = 0_usize;

    for (index, byte) in script.bytes().enumerate() {
        match byte {
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b';' if brace_depth == 0 => {
                let command = script[start..index].trim();
                if !command.is_empty() {
                    commands.push(command);
                }
                start = index + 1;
            }
            _ => {}
        }
    }

    let command = script[start..].trim();
    if !command.is_empty() {
        commands.push(command);
    }

    commands
}

fn build_script_preview(steps: &[ScriptStep], max_lines: usize) -> Vec<(String, Color32)> {
    let mut lines = Vec::new();
    append_script_preview(steps, 0, max_lines, &mut lines);
    lines
}

fn append_script_preview(
    steps: &[ScriptStep],
    indent: usize,
    max_lines: usize,
    lines: &mut Vec<(String, Color32)>,
) {
    for step in steps {
        if lines.len() >= max_lines {
            return;
        }

        let prefix = "  ".repeat(indent);
        match step {
            ScriptStep::Send { command, line } => {
                push_preview_line(
                    lines,
                    max_lines,
                    *line,
                    format!("{prefix}uart  {command}"),
                    ACCENT,
                );
            }
            ScriptStep::Requires { features, line } => {
                push_preview_line(
                    lines,
                    max_lines,
                    *line,
                    format!("{prefix}require  {}", features.join(", ")),
                    TEXT_DIM,
                );
            }
            ScriptStep::Wait { ms, line } => {
                push_preview_line(
                    lines,
                    max_lines,
                    *line,
                    format!("{prefix}wait  {ms} ms"),
                    WARN,
                );
            }
            ScriptStep::Expect {
                text,
                timeout_ms,
                line,
            } => {
                push_preview_line(
                    lines,
                    max_lines,
                    *line,
                    format!("{prefix}expect  \"{text}\" / {timeout_ms} ms"),
                    GOOD,
                );
            }
            ScriptStep::IfContains {
                text,
                line,
                then_steps,
                else_steps,
            } => {
                push_preview_line(
                    lines,
                    max_lines,
                    *line,
                    format!("{prefix}if contains  \"{text}\""),
                    ACCENT_2,
                );
                append_script_preview(then_steps, indent + 1, max_lines, lines);

                if !else_steps.is_empty() {
                    push_preview_line(lines, max_lines, *line, format!("{prefix}else"), ACCENT_2);
                    append_script_preview(else_steps, indent + 1, max_lines, lines);
                }
            }
        }
    }
}

fn push_preview_line(
    lines: &mut Vec<(String, Color32)>,
    max_lines: usize,
    source_line: usize,
    text: String,
    color: Color32,
) {
    if lines.len() >= max_lines {
        return;
    }

    lines.push((format!("L{source_line:03}  {text}"), color));
}

#[allow(clippy::too_many_arguments)]
fn run_script_steps(
    steps: &[ScriptStep],
    tx: &mpsc::Sender<Vec<u8>>,
    events_tx: &mpsc::Sender<AppEvent>,
    response_rx: &mpsc::Receiver<String>,
    stop: &AtomicBool,
    delay_ms: u64,
    strict_mode: bool,
    strict_done_markers: bool,
    serial_buffer: &mut String,
    sent: &mut usize,
    last_strict_response: &mut Option<String>,
) -> Result<(), String> {
    for step in steps {
        if stop.load(Ordering::SeqCst) {
            return Err(format!("строка {}: остановлено пользователем", step.line()));
        }

        match step {
            ScriptStep::Send { command, line } => {
                *last_strict_response = None;

                if strict_mode {
                    drain_script_serial(response_rx, serial_buffer);
                }

                let mut data = command.as_bytes().to_vec();
                data.extend_from_slice(b"\r\n");

                if tx.send(data).is_err() {
                    return Err(format!(
                        "строка {line}: serial monitor закрыт во время отправки `{command}`"
                    ));
                }

                *sent += 1;
                let _ = events_tx.send(AppEvent::SerialEcho(format!("\n> L{line:03} {command}\n")));

                if strict_mode && should_strict_check_command(command) {
                    let timeout_ms = strict_command_timeout_ms(command);
                    let expected_ok_prefix =
                        strict_expected_ok_prefix(command, strict_done_markers);
                    let _ = events_tx.send(AppEvent::SerialEcho(format!(
                        "\n? strict ok/err{} timeout={timeout_ms}ms\n",
                        expected_ok_prefix
                            .map(|prefix| format!(" final={prefix}"))
                            .unwrap_or_default()
                    )));

                    match wait_for_firmware_result(
                        timeout_ms,
                        expected_ok_prefix,
                        response_rx,
                        stop,
                        serial_buffer,
                    ) {
                        StrictCommandResult::Ok(reply) => {
                            *last_strict_response = Some(reply.clone());
                            let _ = events_tx
                                .send(AppEvent::SerialEcho(format!("\n< strict {reply}\n")));
                        }
                        StrictCommandResult::Err(reply) => {
                            return Err(format!(
                                "строка {line}: firmware error после `{command}`: {reply}"
                            ));
                        }
                        StrictCommandResult::Timeout => {
                            return Err(format!(
                                "строка {line}: strict timeout: нет ok/err после `{command}` за {timeout_ms} ms"
                            ));
                        }
                        StrictCommandResult::Stopped => {
                            return Err("остановлено пользователем".to_owned());
                        }
                    }
                }

                if delay_ms > 0 && !sleep_script(delay_ms, stop) {
                    return Err("остановлено пользователем".to_owned());
                }
            }
            ScriptStep::Requires { .. } => {}
            ScriptStep::Wait { ms, line } => {
                if !sleep_script(*ms, stop) {
                    return Err(format!(
                        "строка {line}: остановлено пользователем во время wait"
                    ));
                }
            }
            ScriptStep::Expect {
                text,
                timeout_ms,
                line,
            } => {
                let _ = events_tx.send(AppEvent::SerialEcho(format!(
                    "\n? L{line:03} expect \"{text}\" timeout={timeout_ms}ms\n"
                )));

                if last_strict_response
                    .as_deref()
                    .is_some_and(|reply| reply.contains(text))
                {
                    let _ = events_tx.send(AppEvent::SerialEcho(format!(
                        "\n< matched strict response \"{text}\"\n"
                    )));
                    *last_strict_response = None;
                    continue;
                }

                *last_strict_response = None;

                match wait_for_serial_text(text, *timeout_ms, response_rx, stop, serial_buffer) {
                    ScriptWaitResult::Matched => {
                        let _ = events_tx
                            .send(AppEvent::SerialEcho(format!("\n< matched \"{text}\"\n")));
                    }
                    ScriptWaitResult::Timeout => {
                        return Err(format!(
                            "строка {line}: expect timeout: не найдено '{text}' за {timeout_ms} ms"
                        ));
                    }
                    ScriptWaitResult::Stopped => {
                        return Err(format!(
                            "строка {line}: остановлено пользователем во время expect"
                        ));
                    }
                }
            }
            ScriptStep::IfContains {
                text,
                line,
                then_steps,
                else_steps,
            } => {
                drain_script_serial(response_rx, serial_buffer);
                let matched = serial_buffer.contains(text);
                let branch_name = if matched { "then" } else { "else" };
                let _ = events_tx.send(AppEvent::SerialEcho(format!(
                    "\n? L{line:03} if contains \"{text}\" => {branch_name}\n"
                )));

                let branch = if matched { then_steps } else { else_steps };
                run_script_steps(
                    branch,
                    tx,
                    events_tx,
                    response_rx,
                    stop,
                    delay_ms,
                    strict_mode,
                    strict_done_markers,
                    serial_buffer,
                    sent,
                    last_strict_response,
                )?;
            }
        }
    }

    Ok(())
}

fn drain_script_serial(response_rx: &mpsc::Receiver<String>, serial_buffer: &mut String) {
    while let Ok(data) = response_rx.try_recv() {
        serial_buffer.push_str(&data);
        trim_string_to_last_bytes(serial_buffer, 30_000);
    }
}

fn should_strict_check_command(command: &str) -> bool {
    !matches!(command.trim(), "help" | "?")
}

fn strict_command_timeout_ms(command: &str) -> u64 {
    let command = command.trim();
    if command == "boot" || command.starts_with("run ") {
        return SCRIPT_STRICT_COMMAND_TIMEOUT_MS
            .saturating_add(SCRIPT_STRICT_SLEEP_GRACE_MS)
            .max(60_000 + SCRIPT_STRICT_SLEEP_GRACE_MS);
    }

    if let Some(duration_ms) = command
        .strip_prefix("sleep ")
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        return duration_ms
            .saturating_add(SCRIPT_STRICT_SLEEP_GRACE_MS)
            .max(SCRIPT_STRICT_COMMAND_TIMEOUT_MS);
    }

    SCRIPT_STRICT_COMMAND_TIMEOUT_MS
}

fn strict_expected_ok_prefix(command: &str, strict_done_markers: bool) -> Option<&'static str> {
    if !strict_done_markers {
        return None;
    }

    let command = command.trim();
    if command == "boot" {
        Some("ok boot_done ")
    } else if command.starts_with("run ") {
        Some("ok run_done ")
    } else {
        None
    }
}

fn wait_for_firmware_result(
    timeout_ms: u64,
    expected_ok_prefix: Option<&str>,
    response_rx: &mpsc::Receiver<String>,
    stop: &AtomicBool,
    serial_buffer: &mut String,
) -> StrictCommandResult {
    let timeout = Duration::from_millis(timeout_ms);
    let started = Instant::now();
    let mut result_buffer = String::new();

    loop {
        if stop.load(Ordering::SeqCst) {
            return StrictCommandResult::Stopped;
        }

        if let Some(result) = firmware_result_from_buffer(&result_buffer, expected_ok_prefix) {
            return result;
        }

        if started.elapsed() >= timeout {
            return StrictCommandResult::Timeout;
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        let wait = remaining.min(Duration::from_millis(25));

        match response_rx.recv_timeout(wait) {
            Ok(data) => {
                result_buffer.push_str(&data);
                serial_buffer.push_str(&data);
                trim_string_to_last_bytes(&mut result_buffer, 30_000);
                trim_string_to_last_bytes(serial_buffer, 30_000);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return StrictCommandResult::Timeout,
        }
    }
}

fn firmware_result_from_buffer(
    buffer: &str,
    expected_ok_prefix: Option<&str>,
) -> Option<StrictCommandResult> {
    for line in buffer.lines() {
        let line = line.trim();
        if line == "ok" || line.starts_with("ok ") {
            if expected_ok_prefix.is_none_or(|prefix| line.starts_with(prefix)) {
                return Some(StrictCommandResult::Ok(line.to_owned()));
            }
        }
        if line == "err" || line.starts_with("err ") {
            return Some(StrictCommandResult::Err(line.to_owned()));
        }
    }

    None
}

fn sleep_script(ms: u64, stop: &AtomicBool) -> bool {
    let timeout = Duration::from_millis(ms);
    let started = Instant::now();

    while started.elapsed() < timeout {
        if stop.load(Ordering::SeqCst) {
            return false;
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(25)));
    }

    !stop.load(Ordering::SeqCst)
}

fn wait_for_serial_text(
    text: &str,
    timeout_ms: u64,
    response_rx: &mpsc::Receiver<String>,
    stop: &AtomicBool,
    serial_buffer: &mut String,
) -> ScriptWaitResult {
    let timeout = Duration::from_millis(timeout_ms);
    let started = Instant::now();
    let mut expect_buffer = String::new();

    loop {
        if stop.load(Ordering::SeqCst) {
            return ScriptWaitResult::Stopped;
        }

        if expect_buffer.contains(text) {
            return ScriptWaitResult::Matched;
        }

        if started.elapsed() >= timeout {
            return ScriptWaitResult::Timeout;
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        let wait = remaining.min(Duration::from_millis(25));

        match response_rx.recv_timeout(wait) {
            Ok(data) => {
                expect_buffer.push_str(&data);
                serial_buffer.push_str(&data);
                trim_string_to_last_bytes(&mut expect_buffer, 30_000);
                trim_string_to_last_bytes(serial_buffer, 30_000);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return ScriptWaitResult::Timeout,
        }
    }
}

fn normalize_script_payload(script: &str) -> String {
    if script.trim().is_empty() {
        return String::new();
    }

    let mut normalized = script.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }

    normalized.replace('\n', "\r\n")
}

fn empty_dash(value: &str) -> &str {
    if value.trim().is_empty() { "-" } else { value }
}

fn time_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 86_400;
    let h = secs / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn trim_string_to_last_bytes(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }

    let excess = value.len() - max_bytes;
    let cut = value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= excess)
        .unwrap_or(excess);
    value.drain(..cut);
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn parses_firmware_caps_line() {
        let caps = parse_firmware_caps(
            "ok caps name=esp32-rust-fw version=0.1.0 protocol=1 features=led,timer,timer_do",
        )
        .unwrap();

        assert_eq!(caps.name, "esp32-rust-fw");
        assert_eq!(caps.version, "0.1.0");
        assert_eq!(caps.protocol, "1");
        assert!(caps.supports("led"));
        assert!(caps.supports("timer_do"));
        assert!(!caps.supports("on_pin"));
    }

    #[test]
    fn detects_missing_firmware_features() {
        let caps = parse_firmware_caps(
            "ok caps name=esp32-rust-fw version=0.1.0 protocol=1 features=led,timer",
        )
        .unwrap();
        let steps = vec![
            ScriptStep::Send {
                command: "timer 0 every 1000 do { led toggle }".to_owned(),
                line: 1,
            },
            ScriptStep::Send {
                command: "on pin 0 falling debounce 30 do { led toggle }".to_owned(),
                line: 2,
            },
        ];

        assert_eq!(
            missing_firmware_features(&steps, &caps),
            vec![
                "on_pin".to_owned(),
                "on_pin_debounce".to_owned(),
                "timer_do".to_owned()
            ]
        );
    }

    #[test]
    fn detects_explicit_required_features() {
        let caps = parse_firmware_caps(
            "ok caps name=esp32-rust-fw version=0.1.0 protocol=1 features=status",
        )
        .unwrap();
        let steps = vec![
            ScriptStep::Requires {
                features: vec!["pwm_real".to_owned(), "custom_cap".to_owned()],
                line: 1,
            },
            ScriptStep::Send {
                command: "status".to_owned(),
                line: 2,
            },
        ];

        assert_eq!(
            missing_firmware_features(&steps, &caps),
            vec!["custom_cap".to_owned(), "pwm_real".to_owned()]
        );
    }

    #[test]
    fn detects_required_features_inside_board_blocks() {
        let steps = vec![
            ScriptStep::Send {
                command: "save blink { led toggle; sleep 100 }".to_owned(),
                line: 1,
            },
            ScriptStep::Send {
                command: "if pin 0 == on { pwm 2 freq=1000 duty=512 } else { led off }".to_owned(),
                line: 2,
            },
        ];

        assert_eq!(
            required_firmware_features(&steps),
            vec![
                "board_if".to_owned(),
                "led".to_owned(),
                "pin".to_owned(),
                "pwm".to_owned(),
                "pwm_real".to_owned(),
                "save".to_owned(),
                "sleep".to_owned(),
            ]
        );
    }

    #[test]
    fn detects_required_features_for_on_boot_sugar() {
        let steps = espscript::compile_script(
            r#"
            on.boot() {
                led.on();
                sleep(duration: ms(50));
            }
            "#,
        )
        .unwrap();

        assert_eq!(
            required_firmware_features(&steps),
            vec![
                "autorun".to_owned(),
                "led".to_owned(),
                "save".to_owned(),
                "sleep".to_owned(),
            ]
        );
    }

    #[test]
    fn detects_strict_firmware_result_lines() {
        assert_eq!(
            firmware_result_from_buffer("ready\r\nok status led=off\r\n", None),
            Some(StrictCommandResult::Ok("ok status led=off".to_owned()))
        );
        assert_eq!(
            firmware_result_from_buffer("noise\nerr unknown_command 'x' try help\n", None),
            Some(StrictCommandResult::Err(
                "err unknown_command 'x' try help".to_owned()
            ))
        );
        assert_eq!(firmware_result_from_buffer("ready only\n", None), None);
    }

    #[test]
    fn strict_firmware_result_can_wait_for_done_marker() {
        assert_eq!(
            firmware_result_from_buffer(
                "ok led=on\nok run_done name=blink\n",
                Some("ok run_done ")
            ),
            Some(StrictCommandResult::Ok("ok run_done name=blink".to_owned()))
        );
        assert_eq!(
            firmware_result_from_buffer("ok led=on\n", Some("ok run_done ")),
            None
        );
        assert_eq!(
            firmware_result_from_buffer("ok led=on\nerr program_failed\n", Some("ok run_done ")),
            Some(StrictCommandResult::Err("err program_failed".to_owned()))
        );
    }

    #[test]
    fn strict_done_markers_are_feature_gated() {
        assert_eq!(strict_expected_ok_prefix("run blink", false), None);
        assert_eq!(
            strict_expected_ok_prefix("run blink", true),
            Some("ok run_done ")
        );
        assert_eq!(
            strict_expected_ok_prefix("boot", true),
            Some("ok boot_done ")
        );
        assert_eq!(strict_expected_ok_prefix("status", true), None);
    }

    #[test]
    fn strict_timeout_accounts_for_board_sleep() {
        assert_eq!(
            strict_command_timeout_ms("status"),
            SCRIPT_STRICT_COMMAND_TIMEOUT_MS
        );
        assert_eq!(
            strict_command_timeout_ms("sleep 6000"),
            6000 + SCRIPT_STRICT_SLEEP_GRACE_MS
        );
        assert_eq!(
            strict_command_timeout_ms("run blink"),
            60_000 + SCRIPT_STRICT_SLEEP_GRACE_MS
        );
        assert_eq!(
            strict_command_timeout_ms("boot"),
            60_000 + SCRIPT_STRICT_SLEEP_GRACE_MS
        );
    }
}
