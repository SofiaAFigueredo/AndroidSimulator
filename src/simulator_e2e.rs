//! O painel do simulador, de ponta a ponta: eventos de mouse e teclado do egui entram no painel, e o app
//! de exemplo, rodando num emulador real, reage.

use super::*;
use crate::device::e2e::{node_bounds, ui_dump, wait_for};
use eframe::egui::{Event, Modifiers, PointerButton};

struct Panel {
    ctx: egui::Context,
    view: SimulatorView,
    root: std::path::PathBuf,
    port: u16,
}

impl Panel {
    fn frame(&mut self, events: Vec<Event>) {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 900.0)));
        input.events = events;
        let (view, root, port) = (&mut self.view, &self.root, self.port);
        let out = self.ctx.run_ui(input, |ui| view.ui(ui, root, Some(port)));
        out.drop_without_applying_deltas();
    }

    /// Posição na tela do egui que corresponde ao ponto `(x, y)` do dispositivo (em pixels dele).
    fn screen_pos(&self, x: f32, y: f32) -> egui::Pos2 {
        let rect = self.view.image_rect.expect("a imagem do dispositivo ainda não foi desenhada");
        let (w, h) = self.view.frame_size.unwrap();
        rect.min + egui::vec2(x / w as f32 * rect.width(), y / h as f32 * rect.height())
    }

    /// Um clique de mouse (dedo) em `pos`, com uma pausa para virar um toque de verdade.
    fn click(&mut self, pos: egui::Pos2) {
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        std::thread::sleep(Duration::from_millis(150));
        self.frame(vec![]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![]);
    }
}

/// `cargo test panel_taps_and_types_into_the_real_app -- --ignored --nocapture`
#[test]
#[ignore = "compila, sobe o emulador e interage com ele (alguns minutos)"]
fn panel_taps_and_types_into_the_real_app() {
    use crate::catalog::Kind;
    let root = crate::sdk::default_root();
    let (cat, _) = crate::catalog::fetch_all();
    crate::sdk::ensure_metadata(&root, &cat);
    let image = std::env::var("ANDSIM_IMAGE").unwrap_or_else(|_| "system-images;android-24;default;x86".into());
    assert!(crate::sdk::installed(&root).contains_key(&image), "{image} não instalada");
    let img = cat.packages.iter().find(|p| p.path == image).expect("imagem fora do catálogo");

    let (platform, tools) = (cat.latest(Kind::Platform).unwrap(), cat.latest(Kind::BuildTools).unwrap());
    let dir = std::env::temp_dir().join(format!("andsim-panel-{}", std::process::id()));
    let api = crate::catalog::platform_api(&platform.path).unwrap();
    let bt = tools.path.split(';').nth(1).unwrap().to_owned();
    let spec = crate::project::NewProject { name: "Teste App", package: "com.exemplo.painel", compile_sdk: api, build_tools: &bt, sdk_root: &root };
    crate::project::create(&dir, &spec).unwrap();
    let apk = crate::project::build(&root, &dir, "assembleDebug", &mut |_| {}).expect("build").expect("sem APK");

    let name = format!("probe_panel_{}", avd::default_name(img));
    avd::create(&root, &name, img).unwrap();
    let port = 5590;
    let mut emu = avd::launch(&root, &name, &avd::Launch::new(true, port)).unwrap();
    let result = (|| -> Result<(), String> {
        avd::wait_boot(&root, &mut emu, port, Duration::from_secs(300))?;
        avd::run_apk(&root, port, &apk, "com.exemplo.painel", &mut |_| {})?;
        wait_for(&root, port, "Cliques: 0", 30)?;

        let mut p = Panel { ctx: egui::Context::default(), view: SimulatorView::default(), root: root.clone(), port };
        let deadline = Instant::now() + Duration::from_secs(120);
        while p.view.image_rect.is_none() {
            p.frame(vec![]);
            if Instant::now() > deadline {
                return Err("o painel nunca mostrou a tela do dispositivo".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        println!("painel mostrando {:?} em {:?}", p.view.frame_size, p.view.image_rect);

        // Clique de mouse no botão, pela imagem do painel.
        let xml = ui_dump(&root, port);
        let (x1, y1, x2, y2) = node_bounds(&xml, "class=\"android.widget.Button\"").ok_or("sem o botão na árvore")?;
        let button = p.screen_pos((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        p.click(button);
        wait_for(&root, port, "Cliques: 1", 15).map_err(|e| format!("o clique do mouse não chegou ao botão: {e}"))?;
        println!("RESULTADO mouse: Cliques: 1");

        // Clique no campo de texto (dá o foco do teclado ao painel) e digitação pelo teclado do egui.
        let (ex1, ey1, ex2, ey2) = node_bounds(&xml, "class=\"android.widget.EditText\"").ok_or("sem o campo de texto")?;
        let field = p.screen_pos((ex1 + ex2) / 2.0, (ey1 + ey2) / 2.0);
        p.click(field);
        std::thread::sleep(Duration::from_secs(1));
        p.frame(vec![Event::Text("ola".into())]);
        p.frame(vec![]);
        wait_for(&root, port, "text=\"ola\"", 20).map_err(|e| format!("o teclado não chegou ao campo: {e}"))?;
        println!("RESULTADO teclado: ola");
        p.frame(vec![Event::Key { key: egui::Key::Backspace, physical_key: Some(egui::Key::Backspace), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
        p.frame(vec![]);
        wait_for(&root, port, "text=\"ol\"", 20).map_err(|e| format!("o Backspace não chegou ao campo: {e}"))?;
        println!("RESULTADO Backspace: ol");

        if let Ok(path) = std::env::var("ANDSIM_SHOT") {
            avd::screenshot(&root, port, Path::new(&path))?;
        }
        p.view.stop();
        Ok(())
    })();
    avd::stop(&root, &mut emu, port);
    let _ = avd::delete(&root, &name);
    let _ = std::fs::remove_dir_all(&dir);
    result.expect("usar o app pelo painel");
}
