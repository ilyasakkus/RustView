use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Button, Color32, ColorImage, CornerRadius, Frame, Key, Layout, Margin,
    PointerButton, RichText, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, Vec2,
};
use rustview_core::{
    AccessPassword, DeviceId,
    protocol::{ButtonState, InputEvent, MouseButton},
};

use crate::{
    identity::load_or_create_device_id,
    network::{self, Command, Event, Worker},
    settings::{self, MAX_RELAY_ADDRESS_LEN},
};

const ACCENT: Color32 = Color32::from_rgb(232, 111, 45);
const ACCENT_DARK: Color32 = Color32::from_rgb(184, 76, 26);
const ACCENT_SOFT: Color32 = Color32::from_rgb(255, 238, 228);
const BACKGROUND: Color32 = Color32::from_rgb(247, 245, 242);
const SIDEBAR: Color32 = Color32::from_rgb(239, 236, 232);
const PANEL: Color32 = Color32::WHITE;
const PANEL_RAISED: Color32 = Color32::WHITE;
const BORDER: Color32 = Color32::from_rgb(221, 217, 211);
const TEXT: Color32 = Color32::from_rgb(36, 39, 44);
const MUTED: Color32 = Color32::from_rgb(103, 106, 113);
const SUCCESS: Color32 = Color32::from_rgb(38, 154, 91);
const DANGER: Color32 = Color32::from_rgb(198, 65, 69);
const HOST_RESTART_AFTER_SESSION: Duration = Duration::from_secs(1);
const HOST_RESTART_AFTER_REFRESH: Duration = Duration::from_millis(1_200);
const SCROLL_POINTS_PER_DETENT: f32 = 40.0;
const MAX_SCROLL_DETENTS_PER_FRAME: f32 = 8.0;
const TWO_COLUMN_MIN_WIDTH: f32 = 800.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    RemoteAccess,
    Settings,
}

pub struct RustViewApp {
    page: Page,
    relay_address: String,
    relay_draft: String,
    device_id: Option<DeviceId>,
    access_password: Option<AccessPassword>,
    host_worker: Option<Worker>,
    host_status: String,
    host_error: Option<String>,
    host_retry_at: Option<Instant>,
    host_retry_attempt: u32,
    host_session_active: bool,
    viewer_worker: Option<Worker>,
    viewer_status: String,
    remote_id_input: String,
    remote_password_input: String,
    request_control: bool,
    password_dialog_open: bool,
    error: Option<String>,
    notice: Option<String>,
    pending_peer: Option<(String, bool)>,
    grant_control: bool,
    control_enabled: bool,
    texture: Option<TextureHandle>,
    remote_size: [usize; 2],
    last_pointer: Option<(u16, u16)>,
    remote_modifiers: egui::Modifiers,
    remote_keys: Vec<u16>,
    remote_buttons: Vec<MouseButton>,
    remote_canvas_focused: bool,
    scroll_remainder: Vec2,
}

impl std::fmt::Debug for RustViewApp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RustViewApp")
            .field("page", &self.page)
            .field("host_status", &self.host_status)
            .field("viewer_status", &self.viewer_status)
            .field("host_session_active", &self.host_session_active)
            .field("viewer_active", &self.viewer_worker.is_some())
            .finish_non_exhaustive()
    }
}

impl RustViewApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let initial_relay = settings::initial_relay_address();
        let relay_address = initial_relay.address;
        let mut app = Self {
            page: Page::RemoteAccess,
            relay_draft: relay_address.clone(),
            relay_address,
            device_id: None,
            access_password: None,
            host_worker: None,
            host_status: "Preparing local identity".to_owned(),
            host_error: None,
            host_retry_at: None,
            host_retry_attempt: 0,
            host_session_active: false,
            viewer_worker: None,
            viewer_status: "Not connected".to_owned(),
            remote_id_input: String::new(),
            remote_password_input: String::new(),
            request_control: false,
            password_dialog_open: false,
            error: initial_relay.warning,
            notice: None,
            pending_peer: None,
            grant_control: false,
            control_enabled: false,
            texture: None,
            remote_size: [0, 0],
            last_pointer: None,
            remote_modifiers: egui::Modifiers::NONE,
            remote_keys: Vec::new(),
            remote_buttons: Vec::new(),
            remote_canvas_focused: false,
            scroll_remainder: Vec2::ZERO,
        };
        app.initialize_local_access();
        app
    }

    fn initialize_local_access(&mut self) {
        match load_or_create_device_id() {
            Ok(device_id) => self.device_id = Some(device_id),
            Err(error) => {
                self.host_status = "Could not create local identity".to_owned();
                self.error = Some(format!(
                    "Could not prepare the RustView identity: {error:#}"
                ));
                return;
            }
        }

        match AccessPassword::generate() {
            Ok(password) => self.access_password = Some(password),
            Err(error) => {
                self.host_status = "Could not generate a temporary password".to_owned();
                self.error = Some(format!(
                    "Could not generate a temporary access password: {error}"
                ));
                return;
            }
        }
        self.start_host_now();
    }

    fn start_host_now(&mut self) {
        self.host_retry_at = None;
        self.pending_peer = None;
        self.grant_control = false;
        self.host_session_active = false;

        let (Some(device_id), Some(password)) = (&self.device_id, &self.access_password) else {
            self.host_status = "Local access is unavailable".to_owned();
            return;
        };
        let relay_address = self.relay_address.trim();
        if relay_address.is_empty() {
            self.host_status = "Relay address required".to_owned();
            self.host_error = Some("The relay address cannot be empty.".to_owned());
            return;
        }

        let invitation = password.invitation(device_id);
        match network::start_host(relay_address.to_owned(), invitation) {
            Ok(worker) => {
                self.host_worker = Some(worker);
                self.host_status = "Connecting to the relay".to_owned();
                self.host_error = None;
            }
            Err(error) => {
                self.schedule_host_restart(
                    format!("Could not start relay registration: {error:#}"),
                    true,
                );
            }
        }
    }

    fn schedule_host_restart(&mut self, reason: String, after_error: bool) {
        self.host_worker = None;
        self.pending_peer = None;
        self.grant_control = false;
        self.host_session_active = false;
        self.host_status = reason.clone();
        let delay = if after_error {
            let delay = host_retry_delay(self.host_retry_attempt);
            self.host_retry_attempt = self.host_retry_attempt.saturating_add(1);
            self.host_error = Some(reason);
            delay
        } else {
            self.host_retry_attempt = 0;
            HOST_RESTART_AFTER_SESSION
        };
        self.host_retry_at = Some(Instant::now() + delay);
    }

    fn restart_host_after(&mut self, delay: Duration) {
        if let Some(worker) = self.host_worker.take() {
            let _ = worker.commands.send(Command::Stop);
        }
        self.pending_peer = None;
        self.grant_control = false;
        self.host_session_active = false;
        self.host_retry_attempt = 0;
        self.host_status = "Restarting local access".to_owned();
        self.host_retry_at = Some(Instant::now() + delay);
    }

    fn rotate_password(&mut self) {
        self.error = None;
        match AccessPassword::generate() {
            Ok(password) => {
                if let Some(worker) = self.host_worker.take() {
                    let _ = worker.commands.send(Command::Stop);
                }
                self.access_password = Some(password);
                self.host_retry_attempt = 0;
                self.notice = Some("A new temporary password was generated.".to_owned());
                self.start_host_now();
            }
            Err(error) => {
                self.error = Some(format!("Could not generate a new access password: {error}"));
            }
        }
    }

    fn apply_relay_settings(&mut self) {
        let relay = self.relay_draft.trim().to_owned();
        match settings::save_relay_address(&relay) {
            Ok(address) => {
                self.relay_address.clone_from(&address);
                self.relay_draft = address;
                self.error = None;
                self.notice =
                    Some("Relay settings saved. Local access is reconnecting.".to_owned());
                self.restart_host_after(HOST_RESTART_AFTER_REFRESH);
            }
            Err(error) => {
                self.error = Some(format!("Could not save relay settings: {error:#}"));
            }
        }
    }

    fn open_password_dialog(&mut self) {
        self.error = None;
        if self.remote_id_input.trim().parse::<DeviceId>().is_err() {
            self.error = Some("Enter a valid RustView device ID.".to_owned());
            return;
        }
        self.remote_password_input.clear();
        self.password_dialog_open = true;
    }

    fn start_viewer(&mut self) {
        self.error = None;
        let device_id = match self.remote_id_input.trim().parse::<DeviceId>() {
            Ok(device_id) => device_id,
            Err(error) => {
                self.error = Some(format!("Invalid device ID: {error}"));
                return;
            }
        };
        let password = match self.remote_password_input.trim().parse::<AccessPassword>() {
            Ok(password) => password,
            Err(error) => {
                self.error = Some(format!("Invalid access password: {error}"));
                return;
            }
        };
        let invitation = password.invitation(&device_id);
        match network::start_viewer(
            self.relay_address.trim().to_owned(),
            invitation,
            self.request_control,
        ) {
            Ok(worker) => {
                self.viewer_worker = Some(worker);
                self.viewer_status = "Connecting to the remote device".to_owned();
                self.password_dialog_open = false;
                self.remote_password_input.clear();
                self.notice = None;
                self.texture = None;
                self.remote_size = [0, 0];
            }
            Err(error) => self.error = Some(format!("Could not start the connection: {error:#}")),
        }
    }

    fn send_host(&self, command: Command) {
        if let Some(worker) = &self.host_worker {
            let _ = worker.commands.send(command);
        }
    }

    fn send_viewer(&self, command: Command) {
        if let Some(worker) = &self.viewer_worker {
            let _ = worker.commands.send(command);
        }
    }

    fn stop_viewer(&mut self, notify_worker: bool) {
        self.release_remote_input();
        if notify_worker {
            self.send_viewer(Command::Stop);
        }
        self.viewer_worker = None;
        self.texture = None;
        self.remote_size = [0, 0];
        self.control_enabled = false;
        self.last_pointer = None;
        self.remote_modifiers = egui::Modifiers::NONE;
        self.remote_canvas_focused = false;
        self.scroll_remainder = Vec2::ZERO;
    }

    fn poll_workers(&mut self, ctx: &egui::Context) {
        let mut host_events = Vec::new();
        if let Some(worker) = &self.host_worker {
            while let Ok(event) = worker.events.try_recv() {
                host_events.push(event);
            }
        }

        for event in host_events {
            match event {
                Event::Status(status) => {
                    if status.starts_with("Waiting for a connection") {
                        self.host_retry_attempt = 0;
                        self.host_error = None;
                    }
                    self.host_status = status;
                }
                Event::IncomingRequest {
                    peer_name,
                    requested_control,
                } => {
                    self.pending_peer = Some((peer_name, requested_control));
                    self.grant_control = false;
                    self.host_status = "Waiting for local approval".to_owned();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Event::Active { remote_control } => {
                    self.pending_peer = None;
                    self.host_session_active = true;
                    self.host_retry_attempt = 0;
                    self.host_status = if remote_control {
                        "Incoming session active · control enabled"
                    } else {
                        "Incoming session active · view only"
                    }
                    .to_owned();
                }
                Event::Stopped(reason) => self.schedule_host_restart(reason, false),
                Event::Error(message) => self.schedule_host_restart(message, true),
            }
        }

        let mut viewer_events = Vec::new();
        let mut latest_frame = None;
        if let Some(worker) = &self.viewer_worker {
            while let Ok(event) = worker.events.try_recv() {
                viewer_events.push(event);
            }
            latest_frame = worker.take_latest_frame();
        }

        for event in viewer_events {
            match event {
                Event::Status(status) => self.viewer_status = status,
                Event::Active { remote_control } => {
                    self.control_enabled = remote_control;
                    self.viewer_status = if remote_control {
                        "Connected · keyboard and mouse control enabled"
                    } else {
                        "Connected · view only"
                    }
                    .to_owned();
                }
                Event::Stopped(reason) => {
                    self.stop_viewer(false);
                    self.viewer_status = reason.clone();
                    self.notice = Some(reason);
                }
                Event::Error(message) => {
                    self.stop_viewer(false);
                    self.viewer_status = "Connection closed".to_owned();
                    self.error = Some(message);
                }
                Event::IncomingRequest { .. } => {
                    self.stop_viewer(false);
                    self.error = Some(
                        "Received an unexpected session request from the remote peer.".to_owned(),
                    );
                }
            }
        }

        if self.viewer_worker.is_some()
            && let Some(frame) = latest_frame
        {
            let size = [frame.width as usize, frame.height as usize];
            let image = ColorImage::from_rgba_unmultiplied(size, &frame.rgba);
            if let Some(texture) = &mut self.texture {
                texture.set(image, TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("rustview-remote-frame", image, TextureOptions::LINEAR));
            }
            self.remote_size = size;
        }

        if self.host_worker.is_none()
            && self
                .host_retry_at
                .is_some_and(|retry_at| Instant::now() >= retry_at)
        {
            self.start_host_now();
        }
    }

    fn header(&mut self, root: &mut egui::Ui) {
        egui::Panel::top("rustview_header")
            .frame(
                Frame::new()
                    .fill(ACCENT)
                    .inner_margin(Margin::symmetric(18, 0)),
            )
            .show(root, |ui| {
                ui.set_height(64.0);
                ui.horizontal_centered(|ui| {
                    Frame::new()
                        .fill(Color32::WHITE)
                        .corner_radius(CornerRadius::same(7))
                        .inner_margin(Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.label(RichText::new("RV").strong().color(ACCENT_DARK).size(16.0));
                        });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("RustView")
                            .strong()
                            .color(Color32::WHITE)
                            .size(21.0),
                    );
                    ui.label(
                        RichText::new("Open-source remote desktop")
                            .color(Color32::from_white_alpha(205)),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.viewer_worker.is_some()
                            && ui
                                .add(
                                    Button::new(RichText::new("End session").color(Color32::WHITE))
                                        .fill(Color32::from_black_alpha(55))
                                        .stroke(Stroke::new(1.0, Color32::from_white_alpha(90))),
                                )
                                .clicked()
                        {
                            self.stop_viewer(true);
                            self.notice = Some("The remote session was closed.".to_owned());
                        }
                        let (status, healthy) = self.header_status();
                        Frame::new()
                            .fill(Color32::from_black_alpha(45))
                            .corner_radius(CornerRadius::same(15))
                            .inner_margin(Margin::symmetric(12, 5))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new("●").color(if healthy {
                                        Color32::from_rgb(180, 255, 205)
                                    } else {
                                        Color32::from_rgb(255, 225, 180)
                                    }));
                                    ui.label(RichText::new(status).color(Color32::WHITE));
                                });
                            });
                    });
                });
            });
    }

    fn header_status(&self) -> (String, bool) {
        if self.viewer_worker.is_some() {
            return (self.viewer_status.clone(), true);
        }
        let retrying = self.host_worker.is_none() && self.host_retry_at.is_some();
        let status = if retrying {
            "Reconnecting local access".to_owned()
        } else {
            self.host_status.clone()
        };
        (status, self.host_worker.is_some())
    }

    fn sidebar(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("rustview_sidebar")
            .exact_size(218.0)
            .frame(
                Frame::new()
                    .fill(SIDEBAR)
                    .inner_margin(Margin::symmetric(16, 22)),
            )
            .show(root, |ui| {
                ui.label(RichText::new("WORKSPACE").small().strong().color(MUTED));
                ui.add_space(10.0);
                if nav_button(ui, "Remote access", self.page == Page::RemoteAccess) {
                    self.page = Page::RemoteAccess;
                }
                if nav_button(ui, "Settings", self.page == Page::Settings) {
                    self.page = Page::Settings;
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.label(
                        RichText::new("RustView · early access")
                            .small()
                            .color(Color32::from_rgb(113, 119, 131)),
                    );
                    ui.add_space(10.0);
                    Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(160.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("●").color(if self.host_worker.is_some() {
                                    SUCCESS
                                } else {
                                    ACCENT
                                }));
                                ui.label(RichText::new("This device").strong());
                            });
                            ui.label(RichText::new(&self.host_status).small().color(MUTED));
                            if let Some(retry_at) = self.host_retry_at {
                                let seconds = retry_at
                                    .saturating_duration_since(Instant::now())
                                    .as_secs_f32()
                                    .ceil() as u64;
                                ui.label(
                                    RichText::new(format!("Retrying in {seconds}s"))
                                        .small()
                                        .color(ACCENT),
                                );
                            }
                            if ui.small_button("Retry now").clicked() {
                                self.restart_host_after(HOST_RESTART_AFTER_REFRESH);
                            }
                        });
                    ui.add_space(8.0);
                });
            });
    }

    fn central_content(&mut self, ui: &mut egui::Ui) {
        if self.viewer_worker.is_some() {
            self.transient_message(ui);
            self.viewer_page(ui);
            return;
        }

        egui::ScrollArea::vertical()
            .id_salt("rustview_dashboard_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.transient_message(ui);
                match self.page {
                    Page::RemoteAccess => self.remote_access_page(ui),
                    Page::Settings => self.settings_page(ui),
                }
                ui.add_space(8.0);
            });
    }

    fn transient_message(&mut self, ui: &mut egui::Ui) {
        if let Some(message) = self.error.clone() {
            message_banner(ui, &message, DANGER, || self.error = None);
            ui.add_space(12.0);
        } else if let Some(message) = self.notice.clone() {
            message_banner(ui, &message, SUCCESS, || self.notice = None);
            ui.add_space(12.0);
        }
    }

    fn remote_access_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(RichText::new("Remote access").size(27.0));
                ui.label(
                    RichText::new("Share this device or connect to another RustView device.")
                        .color(MUTED),
                );
            });
        });
        ui.add_space(22.0);

        let available_width = ui.available_width();
        if use_two_columns(available_width) {
            ui.horizontal(|ui| {
                let card_width = (available_width - 18.0) / 2.0;
                self.local_access_card(ui, card_width);
                ui.add_space(18.0);
                self.remote_access_card(ui, card_width);
            });
        } else {
            self.local_access_card(ui, available_width);
            ui.add_space(18.0);
            self.remote_access_card(ui, available_width);
        }
    }

    fn local_access_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let device_id = self
            .device_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "Unavailable".to_owned());
        let password = self.access_password.as_ref().map(AccessPassword::expose);
        let mut rotate = false;

        surface(ui, width, |ui| {
            ui.set_min_height(370.0);
            ui.horizontal(|ui| {
                section_badge(ui, "THIS DEVICE", ACCENT);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new("●").color(if self.host_worker.is_some() {
                        SUCCESS
                    } else {
                        ACCENT
                    }));
                    ui.label(
                        RichText::new(if self.host_session_active {
                            "In session"
                        } else if self.host_worker.is_some() {
                            "Available"
                        } else {
                            "Reconnecting"
                        })
                        .small()
                        .color(MUTED),
                    );
                });
            });
            ui.add_space(16.0);
            ui.heading("Allow remote access");
            ui.label(
                RichText::new("Share the ID and temporary password only with someone you trust.")
                    .color(MUTED),
            );
            ui.add_space(22.0);

            ui.label(
                RichText::new("YOUR RUSTVIEW ID")
                    .small()
                    .strong()
                    .color(MUTED),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new(&device_id).monospace().strong().size(25.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(device_id.clone());
                    }
                });
            });
            ui.add_space(18.0);

            ui.label(
                RichText::new("TEMPORARY PASSWORD")
                    .small()
                    .strong()
                    .color(MUTED),
            );
            ui.label(
                RichText::new(password.unwrap_or("Unavailable"))
                    .monospace()
                    .strong()
                    .size(23.0)
                    .color(TEXT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Generate new").clicked() {
                    rotate = true;
                }
                if ui
                    .add_enabled(password.is_some(), Button::new("Copy").small())
                    .clicked()
                    && let Some(password) = password
                {
                    ui.ctx().copy_text(password.to_owned());
                }
            });
            ui.add_space(18.0);
            Frame::new()
                .fill(ACCENT_SOFT)
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(
                            "This password is valid only while the app is running. You must also approve every incoming connection.",
                        )
                        .small()
                        .color(ACCENT_DARK),
                    );
                });
        });
        if rotate {
            self.rotate_password();
        }
    }

    fn remote_access_card(&mut self, ui: &mut egui::Ui, width: f32) {
        let mut continue_to_password = false;
        surface(ui, width, |ui| {
            ui.set_min_height(370.0);
            section_badge(ui, "REMOTE DEVICE", ACCENT);
            ui.add_space(16.0);
            ui.heading("Connect to a remote device");
            ui.label(
                RichText::new("Enter the RustView ID shown on the remote device.").color(MUTED),
            );
            ui.add_space(22.0);
            ui.label(
                RichText::new("REMOTE DEVICE ID")
                    .small()
                    .strong()
                    .color(MUTED),
            );
            let response = ui.add(
                TextEdit::singleline(&mut self.remote_id_input)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. 123 456 789"),
            );
            if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                continue_to_password = true;
            }
            ui.add_space(10.0);
            ui.checkbox(
                &mut self.request_control,
                "Request keyboard and mouse control",
            );
            ui.label(
                RichText::new("When control is off, you can only view the remote screen.")
                    .small()
                    .color(MUTED),
            );
            ui.add_space(22.0);
            if ui
                .add_enabled(
                    !self.remote_id_input.trim().is_empty(),
                    Button::new(RichText::new("Connect").strong().color(Color32::WHITE))
                        .fill(ACCENT)
                        .min_size(Vec2::new(ui.available_width(), 48.0)),
                )
                .clicked()
            {
                continue_to_password = true;
            }
            ui.add_space(14.0);
            ui.label(
                RichText::new("Password will be requested next.")
                    .small()
                    .color(MUTED),
            );
        });
        if continue_to_password {
            self.open_password_dialog();
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Settings").size(27.0));
        ui.label(
            RichText::new("Manage the relay connection and local access status.").color(MUTED),
        );
        ui.add_space(22.0);

        surface(ui, ui.available_width().min(720.0), |ui| {
            ui.heading("Relay server");
            ui.label(RichText::new("RustView relay address in host:port format.").color(MUTED));
            ui.add_space(14.0);
            ui.label(RichText::new("RELAY ADDRESS").small().strong().color(MUTED));
            ui.add(
                TextEdit::singleline(&mut self.relay_draft)
                    .char_limit(MAX_RELAY_ADDRESS_LEN)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        Button::new(RichText::new("Save and reconnect").color(Color32::WHITE))
                            .fill(ACCENT),
                    )
                    .clicked()
                {
                    self.apply_relay_settings();
                }
                if ui.button("Reconnect").clicked() {
                    self.restart_host_after(HOST_RESTART_AFTER_REFRESH);
                }
            });
            ui.add_space(20.0);
            ui.separator();
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Local access status").strong());
                ui.label(RichText::new("●").color(if self.host_worker.is_some() {
                    SUCCESS
                } else {
                    ACCENT
                }));
            });
            ui.label(RichText::new(&self.host_status).color(MUTED));
            if let Some(error) = &self.host_error {
                ui.label(RichText::new(error).small().color(DANGER));
            }
        });
    }

    fn viewer_page(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").color(SUCCESS));
                    ui.label(RichText::new(&self.viewer_status).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Disconnect").clicked() {
                            self.stop_viewer(true);
                            self.notice = Some("The remote session was closed.".to_owned());
                        }
                        section_badge(
                            ui,
                            if self.control_enabled {
                                "CONTROL ENABLED"
                            } else {
                                "VIEW ONLY"
                            },
                            if self.control_enabled { ACCENT } else { MUTED },
                        );
                    });
                });
            });
        ui.add_space(12.0);

        if let Some(texture) = self.texture.clone() {
            self.remote_canvas(ui, &texture);
        } else {
            Frame::new()
                .fill(Color32::from_rgb(13, 15, 18))
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(CornerRadius::same(10))
                .show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                    ui.set_min_size(ui.available_size());
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.add_space(10.0);
                            ui.label(RichText::new(&self.viewer_status).strong());
                            ui.label(
                                RichText::new("The remote user may still need to approve access.")
                                    .color(Color32::from_rgb(184, 190, 200)),
                            );
                        });
                    });
                });
        }
    }

    fn show_password_dialog(&mut self, ctx: &egui::Context) {
        if !self.password_dialog_open {
            return;
        }
        let mut open = self.password_dialog_open;
        let mut connect = false;
        egui::Window::new("Access password")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(430.0, 250.0))
            .open(&mut open)
            .frame(
                Frame::new()
                    .fill(PANEL_RAISED)
                    .stroke(Stroke::new(1.0, ACCENT))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("Remote device").small().strong().color(MUTED));
                ui.label(
                    RichText::new(self.remote_id_input.trim())
                        .monospace()
                        .strong()
                        .size(20.0),
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new("16-CHARACTER TEMPORARY PASSWORD")
                        .small()
                        .color(MUTED),
                );
                let response = ui.add(
                    TextEdit::singleline(&mut self.remote_password_input)
                        .password(true)
                        .char_limit(32)
                        .desired_width(f32::INFINITY)
                        .hint_text("Access password"),
                );
                if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                    connect = true;
                }
                ui.label(
                    RichText::new(
                        "The password is verified while the encrypted channel is established.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.remote_password_input.clear();
                        self.password_dialog_open = false;
                    }
                    if ui
                        .add_enabled(
                            !self.remote_password_input.trim().is_empty(),
                            Button::new(RichText::new("Connect").color(Color32::WHITE))
                                .fill(ACCENT),
                        )
                        .clicked()
                    {
                        connect = true;
                    }
                });
            });
        self.password_dialog_open = open && self.password_dialog_open;
        if !self.password_dialog_open {
            self.remote_password_input.clear();
        } else if connect {
            self.start_viewer();
        }
    }

    fn show_incoming_request(&mut self, ctx: &egui::Context) {
        let Some((peer_name, requested_control)) = self.pending_peer.clone() else {
            return;
        };
        if !requested_control {
            self.grant_control = false;
        }
        let mut approve = false;
        let mut deny = false;
        egui::Window::new("Incoming connection request")
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .fixed_size(Vec2::new(470.0, 290.0))
            .frame(
                Frame::new()
                    .fill(PANEL_RAISED)
                    .stroke(Stroke::new(2.0, ACCENT))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                section_badge(ui, "LOCAL APPROVAL REQUIRED", ACCENT);
                ui.add_space(12.0);
                ui.heading("Wants to view your screen");
                ui.label(RichText::new(&peer_name).strong().size(20.0).color(TEXT));
                ui.label(
                    RichText::new(
                        "Allow access only if you recognize this person and expect the request.",
                    )
                    .color(MUTED),
                );
                ui.add_space(14.0);
                ui.add_enabled_ui(requested_control, |ui| {
                    ui.checkbox(
                        &mut self.grant_control,
                        "Also allow keyboard and mouse control",
                    );
                });
                if !requested_control {
                    ui.label(
                        RichText::new("This request asks for view-only access.")
                            .small()
                            .color(MUTED),
                    );
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            Button::new(RichText::new("Deny").color(DANGER))
                                .fill(Color32::from_rgb(251, 231, 232)),
                        )
                        .clicked()
                    {
                        deny = true;
                    }
                    if ui
                        .add(Button::new(RichText::new("Allow").color(Color32::WHITE)).fill(ACCENT))
                        .clicked()
                    {
                        approve = true;
                    }
                });
            });

        if deny {
            self.send_host(Command::Deny);
            self.pending_peer = None;
            self.grant_control = false;
            self.host_status = "Denying connection".to_owned();
        } else if approve {
            self.send_host(Command::Approve {
                remote_control: self.grant_control && requested_control,
            });
            self.pending_peer = None;
            self.grant_control = false;
            self.host_status = "Starting secure session".to_owned();
        }
    }

    fn remote_canvas(&mut self, ui: &mut egui::Ui, texture: &TextureHandle) {
        let source = Vec2::new(self.remote_size[0] as f32, self.remote_size[1] as f32);
        if source.x <= 0.0 || source.y <= 0.0 {
            return;
        }
        let available = ui.available_size();
        let scale = (available.x / source.x)
            .min(available.y / source.y)
            .min(1.0);
        let desired = source * scale;
        ui.vertical_centered(|ui| {
            let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), Color32::BLACK);
            ui.painter().image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );

            if response.clicked() {
                response.request_focus();
            }
            if self.control_enabled {
                self.capture_remote_input(ui.ctx(), &response);
            }
        });
    }

    fn capture_remote_input(&mut self, ctx: &egui::Context, response: &egui::Response) {
        if response.hovered()
            && let Some(position) = ctx.input(|input| input.pointer.hover_pos())
        {
            let relative = (position - response.rect.min) / response.rect.size();
            let x = (relative.x.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
            let y = (relative.y.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
            if self.last_pointer != Some((x, y)) {
                self.last_pointer = Some((x, y));
                self.send_remote_input(InputEvent::MouseMove { x, y });
            }
        }

        for (button, remote_button) in [
            (PointerButton::Primary, MouseButton::Left),
            (PointerButton::Secondary, MouseButton::Right),
            (PointerButton::Middle, MouseButton::Middle),
        ] {
            if (response.hovered() || response.has_focus())
                && ctx.input(|input| input.pointer.button_pressed(button))
            {
                self.send_remote_input(InputEvent::MouseButton {
                    button: remote_button,
                    state: ButtonState::Pressed,
                });
            }

            let released = ctx.input(|input| input.pointer.button_released(button));
            if released && self.remote_buttons.contains(&remote_button) {
                self.send_remote_input(InputEvent::MouseButton {
                    button: remote_button,
                    state: ButtonState::Released,
                });
            }
        }

        if response.hovered() {
            let scroll = ctx.input(|input| input.smooth_scroll_delta);
            let (delta_x, delta_y) = scroll_detents(&mut self.scroll_remainder, scroll);
            if delta_x != 0 || delta_y != 0 {
                self.send_remote_input(InputEvent::Scroll { delta_x, delta_y });
            }
        }

        let app_focused = ctx.input(|input| input.focused);
        let canvas_focused = app_focused && response.has_focus();
        if !app_focused || (self.remote_canvas_focused && !canvas_focused) {
            self.release_remote_input();
        }
        self.remote_canvas_focused = canvas_focused;

        if canvas_focused {
            let modifiers = ctx.input(|input| input.modifiers);
            self.sync_remote_modifiers(modifiers);
            let key_events = ctx.input(|input| input.events.clone());
            for event in key_events {
                if let egui::Event::Key { key, pressed, .. } = event
                    && let Some(usage) = hid_usage(key)
                {
                    self.send_remote_input(InputEvent::Key {
                        usage,
                        state: if pressed {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                    });
                }
            }
        }
    }

    fn sync_remote_modifiers(&mut self, next: egui::Modifiers) {
        for (was_pressed, is_pressed, usage) in [
            (self.remote_modifiers.ctrl, next.ctrl, 0xe0),
            (self.remote_modifiers.shift, next.shift, 0xe1),
            (self.remote_modifiers.alt, next.alt, 0xe2),
            (self.remote_modifiers.mac_cmd, next.mac_cmd, 0xe3),
        ] {
            if was_pressed != is_pressed {
                self.send_remote_input(InputEvent::Key {
                    usage,
                    state: if is_pressed {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                });
            }
        }
        self.remote_modifiers = next;
    }

    fn send_remote_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key { usage, state } => {
                track_transition(&mut self.remote_keys, usage, state);
            }
            InputEvent::MouseButton { button, state } => {
                track_transition(&mut self.remote_buttons, button, state);
            }
            InputEvent::MouseMove { .. } | InputEvent::Scroll { .. } => {}
        }
        self.send_viewer(Command::Input(event));
    }

    fn release_remote_input(&mut self) {
        for usage in std::mem::take(&mut self.remote_keys).into_iter().rev() {
            self.send_viewer(Command::Input(InputEvent::Key {
                usage,
                state: ButtonState::Released,
            }));
        }
        for button in std::mem::take(&mut self.remote_buttons) {
            self.send_viewer(Command::Input(InputEvent::MouseButton {
                button,
                state: ButtonState::Released,
            }));
        }
        self.remote_modifiers = egui::Modifiers::NONE;
        self.scroll_remainder = Vec2::ZERO;
    }
}

impl eframe::App for RustViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_workers(&ctx);
        self.header(ui);
        self.sidebar(ui);
        egui::CentralPanel::default()
            .frame(Frame::new().fill(BACKGROUND).inner_margin(Margin::same(26)))
            .show(ui, |ui| self.central_content(ui));
        self.show_password_dialog(&ctx);
        self.show_incoming_request(&ctx);
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn nav_button(ui: &mut egui::Ui, label: &str, selected: bool) -> bool {
    ui.add(
        Button::new(RichText::new(label).strong().color(if selected {
            Color32::WHITE
        } else {
            MUTED
        }))
        .fill(if selected {
            ACCENT
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::new(0.0, Color32::TRANSPARENT))
        .min_size(Vec2::new(ui.available_width(), 38.0)),
    )
    .clicked()
}

fn surface(ui: &mut egui::Ui, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            let content_width = (width - 42.0).max(120.0);
            // A card can be hosted by a horizontal two-column layout. Give the card its own
            // top-down child UI so that the parent's direction never leaks into card content.
            ui.allocate_ui_with_layout(
                Vec2::new(content_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(content_width);
                    add_contents(ui);
                },
            );
        });
}

fn section_badge(ui: &mut egui::Ui, label: &str, color: Color32) {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            30,
        ))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(label).small().strong().color(color));
        });
}

fn message_banner(ui: &mut egui::Ui, message: &str, color: Color32, dismiss: impl FnOnce()) {
    let mut dismissed = false;
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            24,
        ))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(message).color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    dismissed = ui.small_button("Dismiss").clicked();
                });
            });
        });
    if dismissed {
        dismiss();
    }
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = BACKGROUND;
    visuals.window_fill = PANEL_RAISED;
    visuals.extreme_bg_color = Color32::from_rgb(251, 250, 248);
    visuals.widgets.inactive.bg_fill = PANEL_RAISED;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(255, 240, 231);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_fill = ACCENT_DARK;
    visuals.selection.bg_fill = ACCENT;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(9.0, 9.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    ctx.set_global_style(style);
}

fn use_two_columns(width: f32) -> bool {
    width >= TWO_COLUMN_MIN_WIDTH
}

fn host_retry_delay(attempt: u32) -> Duration {
    let multiplier = 1_u64 << attempt.min(3);
    Duration::from_secs(2 * multiplier)
}

fn track_transition<T: Copy + PartialEq>(pressed: &mut Vec<T>, value: T, state: ButtonState) {
    match state {
        ButtonState::Pressed if !pressed.contains(&value) => pressed.push(value),
        ButtonState::Released => pressed.retain(|pressed_value| *pressed_value != value),
        ButtonState::Pressed => {}
    }
}

fn scroll_detents(remainder: &mut Vec2, delta: Vec2) -> (i16, i16) {
    let max_accumulated = SCROLL_POINTS_PER_DETENT * MAX_SCROLL_DETENTS_PER_FRAME;
    *remainder =
        (*remainder + delta).clamp(Vec2::splat(-max_accumulated), Vec2::splat(max_accumulated));

    let scaled = *remainder / SCROLL_POINTS_PER_DETENT;
    let detents = Vec2::new(scaled.x.trunc(), scaled.y.trunc());
    *remainder -= detents * SCROLL_POINTS_PER_DETENT;
    // egui reports content movement, while enigo expects wheel movement.
    (-(detents.x as i16), -(detents.y as i16))
}

fn hid_usage(key: Key) -> Option<u16> {
    Some(match key {
        Key::A => 0x04,
        Key::B => 0x05,
        Key::C => 0x06,
        Key::D => 0x07,
        Key::E => 0x08,
        Key::F => 0x09,
        Key::G => 0x0a,
        Key::H => 0x0b,
        Key::I => 0x0c,
        Key::J => 0x0d,
        Key::K => 0x0e,
        Key::L => 0x0f,
        Key::M => 0x10,
        Key::N => 0x11,
        Key::O => 0x12,
        Key::P => 0x13,
        Key::Q => 0x14,
        Key::R => 0x15,
        Key::S => 0x16,
        Key::T => 0x17,
        Key::U => 0x18,
        Key::V => 0x19,
        Key::W => 0x1a,
        Key::X => 0x1b,
        Key::Y => 0x1c,
        Key::Z => 0x1d,
        Key::Num1 => 0x1e,
        Key::Num2 => 0x1f,
        Key::Num3 => 0x20,
        Key::Num4 => 0x21,
        Key::Num5 => 0x22,
        Key::Num6 => 0x23,
        Key::Num7 => 0x24,
        Key::Num8 => 0x25,
        Key::Num9 => 0x26,
        Key::Num0 => 0x27,
        Key::Enter => 0x28,
        Key::Escape => 0x29,
        Key::Backspace => 0x2a,
        Key::Tab => 0x2b,
        Key::Space => 0x2c,
        Key::Minus => 0x2d,
        Key::Plus | Key::Equals => 0x2e,
        Key::OpenBracket | Key::OpenCurlyBracket => 0x2f,
        Key::CloseBracket | Key::CloseCurlyBracket => 0x30,
        Key::Backslash | Key::Pipe => 0x31,
        Key::Colon | Key::Semicolon => 0x33,
        Key::Quote => 0x34,
        Key::Backtick => 0x35,
        Key::Comma => 0x36,
        Key::Period => 0x37,
        Key::Slash | Key::Questionmark => 0x38,
        Key::Exclamationmark => 0x1e,
        Key::F1 => 0x3a,
        Key::F2 => 0x3b,
        Key::F3 => 0x3c,
        Key::F4 => 0x3d,
        Key::F5 => 0x3e,
        Key::F6 => 0x3f,
        Key::F7 => 0x40,
        Key::F8 => 0x41,
        Key::F9 => 0x42,
        Key::F10 => 0x43,
        Key::F11 => 0x44,
        Key::F12 => 0x45,
        Key::Home => 0x4a,
        Key::PageUp => 0x4b,
        Key::Delete => 0x4c,
        Key::End => 0x4d,
        Key::PageDown => 0x4e,
        Key::ArrowRight => 0x4f,
        Key::ArrowLeft => 0x50,
        Key::ArrowDown => 0x51,
        Key::ArrowUp => 0x52,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_points_are_accumulated_scaled_and_clamped() {
        let mut remainder = Vec2::ZERO;
        assert_eq!(
            scroll_detents(&mut remainder, Vec2::new(19.0, 20.0)),
            (0, 0)
        );
        assert_eq!(
            scroll_detents(&mut remainder, Vec2::new(21.0, 20.0)),
            (-1, -1)
        );
        assert_eq!(remainder, Vec2::ZERO);

        assert_eq!(
            scroll_detents(&mut remainder, Vec2::new(-10_000.0, 10_000.0)),
            (8, -8)
        );
        assert_eq!(remainder, Vec2::ZERO);
    }

    #[test]
    fn pressed_input_tracking_is_unique_and_release_removes_it() {
        let mut pressed = Vec::new();
        track_transition(&mut pressed, 0x04, ButtonState::Pressed);
        track_transition(&mut pressed, 0x04, ButtonState::Pressed);
        assert_eq!(pressed, vec![0x04]);

        track_transition(&mut pressed, 0x04, ButtonState::Released);
        assert!(pressed.is_empty());
    }

    #[test]
    fn unsupported_insert_is_not_forwarded() {
        assert_eq!(hid_usage(Key::Insert), None);
        assert_eq!(hid_usage(Key::F12), Some(0x45));
        assert_eq!(hid_usage(Key::Semicolon), Some(0x33));
    }

    #[test]
    fn surface_keeps_card_content_vertical_inside_horizontal_parent() {
        let mut first_rect = None;
        let mut second_rect = None;

        egui::__run_test_ui(|ui| {
            ui.horizontal(|ui| {
                surface(ui, 320.0, |ui| {
                    first_rect = Some(ui.label("First row").rect);
                    second_rect = Some(ui.label("Second row").rect);
                });
            });
        });

        let first_rect = first_rect.expect("first label should be rendered");
        let second_rect = second_rect.expect("second label should be rendered");
        assert!(
            second_rect.top() >= first_rect.bottom(),
            "surface content must flow downward: first={first_rect:?}, second={second_rect:?}"
        );
    }

    #[test]
    fn retry_backoff_and_responsive_breakpoint_are_bounded() {
        assert_eq!(host_retry_delay(0), Duration::from_secs(2));
        assert_eq!(host_retry_delay(3), Duration::from_secs(16));
        assert_eq!(host_retry_delay(20), Duration::from_secs(16));
        // Approximate central widths after the fixed sidebar and panel margins:
        // the 900 px minimum window stacks, while the 1120 px default stays split.
        assert!(!use_two_columns(630.0));
        assert!(use_two_columns(850.0));
        assert!(!use_two_columns(TWO_COLUMN_MIN_WIDTH - 1.0));
        assert!(use_two_columns(TWO_COLUMN_MIN_WIDTH));
    }
}
