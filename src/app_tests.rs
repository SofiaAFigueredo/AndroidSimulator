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

/// Digitar um erro no editor faz o servidor de linguagem publicar o problema, e o editor o sublinha.
/// `cargo test editor_underlines_errors_from_the_language_server -- --ignored --nocapture`
#[test]
#[ignore = "inicia o jdtls (perfil Completa instalado)"]
fn editor_underlines_errors_from_the_language_server() {
    let code = "public class Main {\n    public static void main(String[] args) {\n        int x = 1;\n    }\n}\n";
    let mut h = open("diag-java", "src/Main.java", code, highlight::Lang::Java);
    // O servidor já analisou o arquivo e não achou erro.
    std::thread::sleep(Duration::from_secs(3));
    h.frame(vec![]);
    assert!(h.app.all_problems().iter().all(|(_, p)| p.severity != problems::Severity::Error), "{:?}", h.app.all_problems());

    // Digita uma aspa antes do 1: `int x = "1;`.
    h.put_cursor_after("int x = ");
    h.frame(vec![Event::Text("\"".into())]);
    h.frame(vec![]);
    h.wait("o erro digitado aparecer", 60, |a| a.all_problems().iter().any(|(_, p)| p.severity == problems::Severity::Error));
    let list = h.app.all_problems();
    println!("PROBLEMAS: {list:?}");
    let (path, p) = &list[0];
    assert!(path.ends_with("Main.java"));
    assert_eq!(p.line, 2, "o erro está na linha 3 do arquivo");

    // O sublinhado cobre o trecho do erro e o tooltip diz o que houve.
    let text = h.app.tabs[0].text.clone();
    let marks = problems::marks(&text, &[p.clone()]);
    assert!(!marks.is_empty(), "o problema precisa virar um trecho sublinhado");
    let under = &text[marks[0].start..marks[0].end];
    println!("SUBLINHADO: {under:?}");
    assert!(under.contains('"'), "o trecho sublinhado deveria conter a aspa solta: {under:?}");
    let msg = problems::message_at(&text, &[p.clone()], text[..marks[0].start].chars().count()).expect("tooltip");
    println!("TOOLTIP: {msg}");
    assert!(msg.starts_with("Erro"), "{msg}");
    h.app.ac.shutdown();
}

/// Um erro de compilação do Gradle vira problema: aparece na lista (mesmo com o arquivo fechado), é
/// sublinhado ao abrir o arquivo e some quando o arquivo é editado.
/// `cargo test gradle_errors_become_problems -- --ignored --nocapture`
#[test]
#[ignore = "compila com o Gradle (cerca de 1 min)"]
fn gradle_errors_become_problems() {
    use crate::catalog::Kind;
    let root = sdk::default_root();
    let (cat, _) = crate::catalog::fetch_all();
    sdk::ensure_metadata(&root, &cat);
    let (platform, tools) = (cat.latest(Kind::Platform).unwrap(), cat.latest(Kind::BuildTools).unwrap());
    let dir = std::env::temp_dir().join(format!("andsim-gp-{}", std::process::id()));
    let api = crate::catalog::platform_api(&platform.path).unwrap();
    let bt = tools.path.split(';').nth(1).unwrap().to_owned();
    let spec = project::NewProject { name: "Teste App", package: "com.exemplo.gp", compile_sdk: api, build_tools: &bt, sdk_root: &root };
    project::create(&dir, &spec).unwrap();
    let file = dir.join("app/src/main/java/com/exemplo/gp/MainActivity.kt");
    let src = std::fs::read_to_string(&file).unwrap().replace("titulo.textSize = 24f", "titulo.textSize = naoExiste");
    std::fs::write(&file, src).unwrap();

    let ctx = egui::Context::default();
    let mut app = App::build(&ctx, vec![dir.display().to_string()]);
    app.compile(&ctx);
    let deadline = Instant::now() + Duration::from_secs(300);
    while app.build_rx.is_some() {
        app.poll(&ctx);
        assert!(Instant::now() < deadline, "o build não terminou");
        std::thread::sleep(Duration::from_millis(200));
    }

    // 1. Aparece na lista de problemas, com o arquivo ainda fechado.
    let list = app.all_problems();
    println!("PROBLEMAS: {list:?}");
    let (path, p) = list.iter().find(|(_, p)| p.severity == problems::Severity::Error).expect("o Gradle reclamou de um erro");
    assert!(path.ends_with("MainActivity.kt"), "{path:?}");
    assert!(p.message.contains("naoExiste"), "{}", p.message);
    assert_eq!(p.source, "Gradle");
    assert!(app.tabs.is_empty());

    // 2. Ao abrir o arquivo, o trecho certo fica sublinhado.
    let path = path.clone();
    app.open_file(&path);
    let id = egui::Id::unique(("editor", &app.tabs[0].path));
    let mut h = Harness { ctx, app, id };
    h.frame(vec![]);
    let uri = lsp::path_to_uri(&h.app.tabs[0].path);
    let tab_problems = tab_problems(&h.app.ac, &h.app.build_problems, &h.app.tabs[0], highlight::Lang::Kotlin, &uri);
    assert_eq!(tab_problems.len(), 1);
    let text = h.app.tabs[0].text.clone();
    let marks = problems::marks(&text, &tab_problems);
    assert_eq!(&text[marks[0].start..marks[0].end], "naoExiste", "o sublinhado cobre a palavra do erro");
    println!("SUBLINHADO: naoExiste");

    // 3. Editar o arquivo apaga o problema (as posições deixam de valer).
    h.put_cursor_after("titulo.textSize = ");
    h.frame(vec![Event::Text("1".into())]);
    h.frame(vec![]);
    assert!(h.app.build_problems.is_empty(), "o problema do Gradle deveria sumir ao editar: {:?}", h.app.build_problems);
    assert!(h.app.all_problems().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
