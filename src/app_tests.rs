//! Testes de interface sem janela: o egui roda quadros com eventos de teclado simulados, e o editor fala
//! com os servidores de linguagem reais (precisam estar instalados: perfil Completa).

use super::*;
use eframe::egui::{Event, Key, Modifiers};
use std::time::{Duration, Instant};

fn key(k: Key, modifiers: Modifiers) -> Event {
    Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers }
}

struct Harness {
    ctx: egui::Context,
    app: App,
    id: egui::Id,
}

impl Harness {
    fn frame(&mut self, events: Vec<Event>) {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 800.0)));
        input.events = events;
        let ctx = self.ctx.clone();
        let app = &mut self.app;
        let output = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            app.sync_language_servers(&ctx);
            egui::CentralPanel::default().show(ui, |ui| app.editor(ui));
        });
        // Sem janela não há quem aplique as texturas; o egui exige descartá-las de forma explícita.
        output.drop_without_applying_deltas();
    }

    fn wait(&mut self, what: &str, secs: u64, mut done: impl FnMut(&mut App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            self.frame(vec![]);
            if done(&mut self.app) {
                return;
            }
            assert!(Instant::now() < deadline, "tempo esgotado esperando: {what}");
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Cursor logo depois de `after` (índice em caracteres), com o editor em foco.
    fn put_cursor_after(&mut self, after: &str) {
        let text = self.app.tabs[0].text.clone();
        let byte = text.find(after).expect("trecho não encontrado") + after.len();
        let idx = text[..byte].chars().count();
        let mut state = egui::TextEdit::load_state(&self.ctx, self.id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(idx))));
        state.store(&self.ctx, self.id);
        self.ctx.memory_mut(|m| m.request_focus(self.id));
        self.frame(vec![]);
        self.frame(vec![]);
        assert_eq!(self.app.cursor, idx, "o editor deveria ter o cursor onde o teste pôs");
    }
}

/// Abre `file` (com `code`) num projeto temporário e espera o servidor da linguagem ficar pronto.
fn open(name: &str, file: &str, code: &str, lang: highlight::Lang) -> Harness {
    let root = sdk::default_root();
    assert!(servers::installed(&root, lang), "instale o servidor de {lang:?} pelo app (perfil Completa)");
    let project = std::env::temp_dir().join(format!("andsim-ui-{name}-{}", std::process::id()));
    let path = project.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, code).unwrap();
    let ctx = egui::Context::default();
    let app = App::build(&ctx, vec![project.display().to_string(), path.display().to_string()]);
    assert_eq!(app.tabs.len(), 1, "o arquivo deveria ter aberto numa aba");
    let id = egui::Id::unique(("editor", &app.tabs[0].path));
    let mut h = Harness { ctx, app, id };
    h.wait("servidor de linguagem pronto e documento aberto", 180, |a| a.ac.client(lang).is_some() && a.tabs[0].lsp_version > 0);
    h
}

fn drive(h: &mut Harness, after: &str, typed: &str, pick: &str) -> String {
    h.put_cursor_after(after);
    // O servidor pode responder vazio enquanto ainda analisa o arquivo; o usuário repete o atalho.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        h.frame(vec![key(Key::Space, Modifiers::CTRL)]);
        h.wait("resposta do pedido", 60, |a| a.pending.is_none());
        if h.app.popup.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "tempo esgotado esperando: popup de sugestões aberto");
        std::thread::sleep(Duration::from_secs(2));
    }
    for ch in typed.chars() {
        h.frame(vec![Event::Text(ch.to_string())]);
        // Lista cortada pelo servidor: o app pede de novo; espera chegar.
        h.wait("resposta depois de digitar", 60, |a| a.pending.is_none() && a.popup.is_some());
    }
    // Move a seleção até a sugestão desejada com a seta para baixo.
    let prefix = {
        let t = &h.app.tabs[0].text;
        let start = autocomplete::prefix_start(t, h.app.cursor);
        autocomplete::prefix_text(t, start, h.app.cursor)
    };
    let labels: Vec<String> = autocomplete::matching(&h.app.popup.as_ref().unwrap().items, &prefix).iter().map(|i| i.label.clone()).collect();
    println!("sugestões para {prefix:?}: {:?}", &labels[..labels.len().min(8)]);
    let target = labels.iter().position(|l| l.starts_with(pick)).unwrap_or_else(|| panic!("sem {pick:?} em {labels:?}"));
    for _ in 0..target {
        h.frame(vec![key(Key::ArrowDown, Modifiers::NONE)]);
    }
    h.frame(vec![key(Key::Enter, Modifiers::NONE)]);
    h.frame(vec![]);
    assert!(h.app.popup.is_none(), "o popup deveria ter fechado ao aceitar");
    h.app.tabs[0].text.clone()
}

/// `cargo test editor_autocomplete_java -- --ignored --nocapture`
#[test]
#[ignore = "inicia o jdtls (perfil Completa instalado)"]
fn editor_autocomplete_java() {
    let code = "public class Main {\n    public static void main(String[] args) {\n        System.\n    }\n}\n";
    let mut h = open("java", "src/Main.java", code, highlight::Lang::Java);
    let text = drive(&mut h, "System.", "ou", "out");
    println!("TEXTO FINAL:\n{text}");
    assert!(text.contains("        System.out\n"), "o editor deveria ter completado para System.out:\n{text}");
    assert_eq!(h.app.cursor, text.find("System.out").map(|i| text[..i].chars().count() + "System.out".chars().count()).unwrap(), "cursor logo depois da sugestão");
    assert!(h.app.tabs[0].dirty);
    assert!(h.app.tabs[0].lsp_version > 1, "as alterações precisam ter ido para o servidor");
    h.app.ac.shutdown();
}

/// `cargo test editor_autocomplete_kotlin -- --ignored --nocapture`
#[test]
#[ignore = "inicia o kotlin-language-server (perfil Completa instalado)"]
fn editor_autocomplete_kotlin() {
    let code = "fun main() {\n    val lista = listOf(1, 2, 3)\n    lista.\n}\n";
    let mut h = open("kotlin", "Main.kt", code, highlight::Lang::Kotlin);
    let text = drive(&mut h, "lista.", "ma", "map");
    println!("TEXTO FINAL:\n{text}");
    assert!(text.contains("    lista.map"), "o editor deveria ter completado para lista.map:\n{text}");
    // O espaço reservado do snippet (`map { transform }`) precisa ficar selecionado para a próxima digitação.
    let range = egui::TextEdit::load_state(&h.ctx, h.id).and_then(|s| s.cursor.char_range()).expect("seleção do editor");
    let (a, b) = (range.primary.index.0.min(range.secondary.index.0), range.primary.index.0.max(range.secondary.index.0));
    let selected: String = text.chars().skip(a).take(b - a).collect();
    println!("SELECIONADO: {selected:?}");
    assert_eq!(selected, "transform", "o placeholder deveria estar selecionado");
    h.app.ac.shutdown();
}
