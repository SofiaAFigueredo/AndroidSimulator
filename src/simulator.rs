//! O painel do simulador: a tela do dispositivo ao vivo dentro do app, com o mouse como dedo (tocar,
//! arrastar e a roda para rolar), o teclado indo para o app e os botões de hardware (Voltar, Início…).

use crate::avd;
use crate::device::{self, Simulator};
use eframe::egui::{self, Color32, RichText};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

const MUTED: Color32 = Color32::from_rgb(0x85, 0x85, 0x85);
/// Intervalo mínimo entre dois eventos de movimento enviados ao dispositivo.
const MOVE_EVERY: Duration = Duration::from_millis(20);
/// Quanto a roda precisa girar (em pixels do egui) para virar um gesto de rolagem.
const WHEEL_STEP: f32 = 30.0;

enum State {
    Off,
    Starting { port: u16, rx: Receiver<Result<Simulator, String>> },
    Running(Simulator),
    Failed { port: u16, message: String },
}

pub struct SimulatorView {
    state: State,
    texture: Option<egui::TextureHandle>,
    /// Número do último quadro que virou textura.
    seq: u64,
    touching: bool,
    last_pos: Option<egui::Pos2>,
    last_move: Instant,
    /// Movimento da roda ainda não transformado em gesto.
    wheel: f32,
    /// Onde a imagem do dispositivo foi desenhada na última vez (para os testes mirarem o mouse).
    pub image_rect: Option<egui::Rect>,
    /// Tamanho do último quadro, em pixels do dispositivo.
    pub frame_size: Option<(usize, usize)>,
}

impl Default for SimulatorView {
    fn default() -> Self {
        Self { state: State::Off, texture: None, seq: 0, touching: false, last_pos: None, last_move: Instant::now(), wheel: 0.0, image_rect: None, frame_size: None }
    }
}

/// Posição do mouse como fração da imagem (0 a 1 nos dois eixos, limitada às bordas).
pub fn to_fraction(pos: egui::Pos2, rect: egui::Rect) -> (f32, f32) {
    (((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0), ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0))
}

/// Cabe a imagem `(w, h)` no espaço disponível, mantendo a proporção.
pub fn fit(avail: egui::Vec2, w: usize, h: usize) -> egui::Vec2 {
    let scale = (avail.x / w as f32).min(avail.y / h as f32).max(0.01);
    egui::vec2(w as f32 * scale, h as f32 * scale)
}

/// Quanto o dedo anda, em fração da altura da tela, para um giro da roda do mouse. Roda para baixo
/// (valor negativo no egui) mostra o que vem depois, então o dedo sobe.
pub fn wheel_to_fraction(wheel_y: f32, image_height: f32) -> f32 {
    (wheel_y / image_height.max(1.0)).clamp(-0.45, 0.45)
}

/// Tecla do teclado que vira tecla do Android.
pub fn android_key(key: egui::Key) -> Option<u32> {
    use egui::Key::*;
    Some(match key {
        Enter => device::KEY_ENTER,
        Backspace => device::KEY_DEL,
        Delete => device::KEY_FORWARD_DEL,
        Tab => device::KEY_TAB,
        Escape => device::KEY_BACK,
        ArrowUp => device::KEY_DPAD_UP,
        ArrowDown => device::KEY_DPAD_DOWN,
        ArrowLeft => device::KEY_DPAD_LEFT,
        ArrowRight => device::KEY_DPAD_RIGHT,
        _ => return None,
    })
}

impl SimulatorView {
    #[cfg(test)]
    pub fn running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    /// Para o simulador (derruba os processos do `adb`).
    pub fn stop(&mut self) {
        self.state = State::Off;
        self.texture = None;
        self.seq = 0;
        self.touching = false;
        self.image_rect = None;
        self.frame_size = None;
    }

    /// Espera o emulador terminar o boot, numa thread, e liga a tela e a entrada.
    fn start(&mut self, root: &Path, port: u16, ctx: &egui::Context) {
        self.stop();
        let (tx, rx) = mpsc::channel();
        let (root, ctx) = (root.to_owned(), ctx.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(300);
            while !avd::is_booted(&root, port) {
                if Instant::now() > deadline {
                    let _ = tx.send(Err("o emulador não terminou de iniciar em 5 minutos".into()));
                    ctx.request_repaint();
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            let _ = tx.send(Simulator::start(&root, port));
            ctx.request_repaint();
        });
        self.state = State::Starting { port, rx };
    }

    fn poll_start(&mut self) {
        let State::Starting { port, rx } = &self.state else { return };
        let port = *port;
        match rx.try_recv() {
            Ok(Ok(sim)) => self.state = State::Running(sim),
            Ok(Err(message)) => self.state = State::Failed { port, message },
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.state = State::Failed { port, message: "o simulador não iniciou".into() },
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, root: &Path, port: Option<u16>) {
        let ctx = ui.ctx().clone();
        let Some(port) = port else {
            self.stop();
            ui.label(RichText::new("SIMULADOR").small().color(MUTED));
            ui.label("Inicie um emulador na aba Dispositivos para ver e usar o app aqui.");
            return;
        };
        // Reinicia se o emulador mudou ou se a conexão caiu; falhas só tentam de novo pelo botão.
        let restart = match &self.state {
            State::Off => true,
            State::Starting { port: p, .. } | State::Failed { port: p, .. } => *p != port,
            State::Running(s) => s.port != port || !s.alive(),
        };
        if restart {
            self.start(root, port, &ctx);
        }
        self.poll_start();

        ui.label(RichText::new("SIMULADOR").small().color(MUTED));
        match &self.state {
            State::Starting { .. } | State::Off => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Iniciando o emulador…");
                });
                ctx.request_repaint_after(Duration::from_millis(300));
            }
            State::Failed { message, .. } => {
                ui.colored_label(Color32::LIGHT_RED, message.clone());
                if ui.button("Tentar de novo").clicked() {
                    self.start(root, port, &ctx);
                }
            }
            State::Running(_) => self.running_ui(ui, &ctx),
        }
    }

    fn running_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Self { state, texture, seq, touching, last_pos, last_move, wheel, image_rect, frame_size } = self;
        let State::Running(sim) = state else { return };

        // Botões de hardware.
        ui.horizontal_wrapped(|ui| {
            for (label, code) in [
                ("Voltar", device::KEY_BACK),
                ("Início", device::KEY_HOME),
                ("Recentes", device::KEY_APP_SWITCH),
                ("Energia", device::KEY_POWER),
                ("Vol +", device::KEY_VOLUME_UP),
                ("Vol -", device::KEY_VOLUME_DOWN),
            ] {
                if ui.small_button(label).clicked() {
                    let _ = sim.key(code);
                }
            }
        });

        // O quadro mais recente vira a textura.
        if let Some((n, frame)) = sim.screen.frame_after(*seq) {
            let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
            match texture {
                Some(t) => t.set(image, egui::TextureOptions::LINEAR),
                None => *texture = Some(ctx.load_texture("device-screen", image, egui::TextureOptions::LINEAR)),
            }
            *seq = n;
            *frame_size = Some((frame.width, frame.height));
        }
        ui.label(RichText::new(match frame_size {
            Some((w, h)) => format!("{w}x{h} · {} quadros/s", sim.screen.fps()),
            None => "Esperando o primeiro quadro…".to_owned(),
        }).small().color(MUTED));
        ctx.request_repaint_after(Duration::from_millis(33));

        let (Some(tex), Some((w, h))) = (texture.as_ref(), *frame_size) else { return };
        let size = fit(ui.available_size(), w, h);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        *image_rect = Some(rect);
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);

        // Mouse: o botão esquerdo é um dedo.
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let (pressed, down, released) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released()));
        if pressed && let Some(p) = pointer.filter(|p| rect.contains(*p)) {
            let (fx, fy) = to_fraction(p, rect);
            let _ = sim.touch_down(fx, fy);
            *touching = true;
            *last_pos = Some(p);
            resp.request_focus();
        } else if *touching {
            if down
                && let Some(p) = pointer
                && last_pos.is_none_or(|l| l.distance(p) >= 1.0)
                && last_move.elapsed() >= MOVE_EVERY
            {
                let (fx, fy) = to_fraction(p, rect);
                let _ = sim.touch_move(fx, fy);
                *last_pos = Some(p);
                *last_move = Instant::now();
            }
            if released || !down {
                let _ = sim.touch_up();
                *touching = false;
            }
        }

        // Roda do mouse: um arrastar curto, como um dedo rolando a tela.
        // O egui espalha um giro da roda por vários quadros: junta até dar um gesto.
        if pointer.is_some_and(|p| rect.contains(p)) {
            *wheel += ui.input(|i| i.smooth_scroll_delta().y);
        }
        if wheel.abs() >= WHEEL_STEP && !*touching {
            let (fx, fy) = to_fraction(pointer.unwrap_or(rect.center()), rect);
            let end = (fy + wheel_to_fraction(*wheel, rect.height())).clamp(0.02, 0.98);
            *wheel = 0.0;
            let _ = sim.touch_down(fx, fy);
            for step in 1..=4 {
                let _ = sim.touch_move(fx, fy + (end - fy) * step as f32 / 4.0);
            }
            let _ = sim.touch_up();
        }

        // Teclado: com o painel em foco, o texto e as teclas vão para o app.
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true }));
        if resp.has_focus() {
            for event in ui.input(|i| i.events.clone()) {
                match event {
                    egui::Event::Text(t) => {
                        let _ = sim.text(&t);
                    }
                    egui::Event::Key { key, pressed: true, .. } => {
                        if let Some(code) = android_key(key) {
                            let _ = sim.key(code);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_position_becomes_a_clamped_fraction_of_the_image() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(200.0, 400.0));
        assert_eq!(to_fraction(egui::pos2(200.0, 250.0), rect), (0.5, 0.5));
        assert_eq!(to_fraction(egui::pos2(100.0, 50.0), rect), (0.0, 0.0));
        assert_eq!(to_fraction(egui::pos2(0.0, 999.0), rect), (0.0, 1.0), "fora da imagem vai para a borda");
    }

    #[test]
    fn fit_keeps_the_aspect_ratio() {
        let s = fit(egui::vec2(300.0, 300.0), 480, 800);
        assert!((s.y - 300.0).abs() < 0.01 && (s.x - 180.0).abs() < 0.01, "{s:?}");
        let wide = fit(egui::vec2(100.0, 1000.0), 480, 800);
        assert!((wide.x - 100.0).abs() < 0.01 && (wide.y - 166.66).abs() < 0.1, "{wide:?}");
        assert!(fit(egui::vec2(0.0, 0.0), 480, 800).x > 0.0, "sem espaço não gera tamanho zero");
    }

    #[test]
    fn wheel_moves_the_finger_in_the_scroll_direction_and_is_limited() {
        assert!(wheel_to_fraction(-50.0, 800.0) < 0.0);
        assert!(wheel_to_fraction(50.0, 800.0) > 0.0);
        assert_eq!(wheel_to_fraction(10_000.0, 800.0), 0.45);
        assert_eq!(wheel_to_fraction(5.0, 0.0), 0.45, "altura zero não divide por zero");
    }

    #[test]
    fn keyboard_keys_map_to_android_keycodes() {
        assert_eq!(android_key(egui::Key::Enter), Some(66));
        assert_eq!(android_key(egui::Key::Backspace), Some(67));
        assert_eq!(android_key(egui::Key::Escape), Some(4));
        assert_eq!(android_key(egui::Key::ArrowDown), Some(20));
        assert_eq!(android_key(egui::Key::A), None, "letras vão como texto, não como tecla");
    }

    #[test]
    fn without_an_emulator_the_panel_just_says_so() {
        let ctx = egui::Context::default();
        let mut view = SimulatorView::default();
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 600.0)));
        let out = ctx.run_ui(input, |ui| view.ui(ui, Path::new("."), None));
        out.drop_without_applying_deltas();
        assert!(!view.running() && view.image_rect.is_none());
    }
}

#[cfg(test)]
#[path = "simulator_e2e.rs"]
mod e2e;
