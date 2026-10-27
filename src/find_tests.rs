//! Busca no editor, busca em arquivos e criação de arquivos, dirigidos por eventos de teclado simulados
//! do egui, sem janela e sem emulador.

use super::*;
use eframe::egui::{Event, Key, Modifiers};
use std::time::{Duration, Instant};

fn key(k: Key, modifiers: Modifiers) -> Event {
    Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers }
}

struct Env {
    ctx: egui::Context,
    app: App,
}

impl Env {
    /// Abre `dir` como projeto e, se houver, `file` no editor.
    fn new(dir: &Path, file: Option<&Path>) -> Env {
        let ctx = egui::Context::default();
        let mut args = vec![dir.display().to_string()];
        args.extend(file.map(|f| f.display().to_string()));
        let app = App::build(&ctx, args);
        let mut e = Env { ctx, app };
        e.frame(vec![]);
        e
    }

    fn frame_with(&mut self, events: Vec<Event>, modifiers: Modifiers) {
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 800.0)));
        input.events = events;
        // No egui 0.37 os modificadores chegam por um evento próprio.
        if modifiers != Modifiers::NONE {
            input.events.insert(0, Event::ModifiersChanged(modifiers));
        }
        let ctx = self.ctx.clone();
        let app = &mut self.app;
        let out = ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            app.handle_shortcuts(&ctx);
            app.poll_search();
            app.sidebar(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor(ui));
        });
        out.drop_without_applying_deltas();
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.frame_with(events, Modifiers::NONE);
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
        self.frame(vec![]);
    }

    fn press(&mut self, k: Key, modifiers: Modifiers) {
        self.frame_with(vec![key(k, modifiers)], modifiers);
        self.frame(vec![Event::ModifiersChanged(Modifiers::NONE)]);
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("andsim-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn ctrl_f_opens_the_bar_highlights_and_navigates_with_enter_and_f3() {
    let dir = tmp("find");
    let file = dir.join("Notas.txt");
    std::fs::write(&file, "alfa beta alfa gama alfa\n").unwrap();
    let mut e = Env::new(&dir, Some(&file));
    assert!(!e.app.find.open);

    e.press(Key::F, Modifiers::COMMAND);
    assert!(e.app.find.open, "Ctrl+F abre a barra");
    e.type_text("ALFA");
    assert_eq!(e.app.find.query, "ALFA", "o texto digitado foi para a caixa de busca");
    assert_eq!(e.app.find.selected, Some((0, 4)), "a primeira ocorrência (sem diferenciar maiúsculas) já fica selecionada");
    assert_eq!(e.app.find.current, Some(0));
    assert_eq!(e.app.set_cursor, None, "a seleção no editor já foi aplicada no quadro seguinte");

    // Enter vai para a próxima, Shift+Enter volta, e dá a volta no fim.
    e.press(Key::Enter, Modifiers::NONE);
    assert_eq!(e.app.find.selected, Some((10, 14)));
    e.press(Key::Enter, Modifiers::NONE);
    assert_eq!(e.app.find.selected, Some((20, 24)));
    e.press(Key::Enter, Modifiers::NONE);
    assert_eq!(e.app.find.selected, Some((0, 4)), "depois da última volta para a primeira");
    e.press(Key::Enter, Modifiers::SHIFT);
    assert_eq!(e.app.find.selected, Some((20, 24)), "Shift+Enter volta, dando a volta para trás");

    // F3 funciona com a barra fechada, desde que haja uma busca.
    e.press(Key::Escape, Modifiers::NONE);
    assert!(!e.app.find.open, "Esc fecha a barra");
    e.press(Key::F3, Modifiers::NONE);
    assert!(e.app.find.open, "F3 reabre a barra");
    assert_eq!(e.app.find.selected, Some((0, 4)));
    e.press(Key::F3, Modifiers::SHIFT);
    assert_eq!(e.app.find.selected, Some((20, 24)));

    // O que a barra marca no texto: a selecionada em destaque e todas com fundo.
    let found = search::find_all(&e.app.tabs[0].text, "ALFA", false);
    let marks = find_marks(&found, e.app.find.selected);
    assert_eq!(marks.len(), 3);
    assert!(marks.iter().all(|m| m.fill), "achados da busca pintam o fundo, não sublinham");
    assert_eq!((marks[0].start, marks[0].end), (20, 24), "a selecionada vem primeiro, para valer sobre as outras");

    // Diferenciar maiúsculas: "ALFA" não existe no texto.
    e.app.find.case_sensitive = true;
    e.app.find_refresh();
    assert_eq!(e.app.find.selected, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn replace_one_and_replace_all_edit_the_open_file() {
    let dir = tmp("replace");
    let file = dir.join("Notas.txt");
    std::fs::write(&file, "alfa beta alfa gama alfa\n").unwrap();
    let mut e = Env::new(&dir, Some(&file));
    e.press(Key::H, Modifiers::COMMAND);
    assert!(e.app.find.open && e.app.find.show_replace, "Ctrl+H abre a barra com a substituição");
    e.type_text("alfa");
    e.app.find.replace = "X".into();

    e.app.replace_current();
    assert_eq!(e.app.tabs[0].text, "X beta alfa gama alfa\n", "troca a ocorrência selecionada");
    assert!(e.app.tabs[0].dirty);
    assert_eq!(e.app.find.selected, Some((7, 11)), "e já seleciona a próxima");
    e.app.replace_current();
    assert_eq!(e.app.tabs[0].text, "X beta X gama alfa\n");

    e.app.replace_all();
    assert_eq!(e.app.tabs[0].text, "X beta X gama X\n");
    assert!(e.app.log.iter().any(|l| l.contains("1 ocorrência(s) substituída(s)")), "{:?}", e.app.log.last());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alfa beta alfa gama alfa\n", "o arquivo em disco só muda ao salvar");
    e.app.save_active();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "X beta X gama X\n");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ctrl_shift_f_searches_every_file_and_opens_the_hit() {
    let dir = tmp("search");
    std::fs::create_dir_all(dir.join("app/src")).unwrap();
    std::fs::create_dir_all(dir.join("app/build")).unwrap();
    std::fs::write(dir.join("app/src/Main.kt"), "fun main() {\n    println(\"ação\")\n}\n").unwrap();
    std::fs::write(dir.join("app/src/Outro.kt"), "val ação = 1\nval b = ação + 1\n").unwrap();
    std::fs::write(dir.join("app/build/Gerado.kt"), "val ação = 2\n").unwrap();
    let mut e = Env::new(&dir, None);

    e.press(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    assert_eq!(e.app.view, View::Search, "Ctrl+Shift+F abre a busca em arquivos");
    assert!(!e.app.find.open, "e não a barra do editor");
    e.type_text("AÇÃO");
    e.press(Key::Enter, Modifiers::NONE);
    let deadline = Instant::now() + Duration::from_secs(10);
    while e.app.search.rx.is_some() {
        e.frame(vec![]);
        assert!(Instant::now() < deadline, "a busca não terminou");
        std::thread::sleep(Duration::from_millis(20));
    }
    let results = &e.app.search.results;
    let names: Vec<String> = results.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["Main.kt", "Outro.kt"], "ignora a pasta build: {names:?}");
    assert_eq!(e.app.search.stats.as_ref().map(|s| s.hits), Some(3));
    let hit = results[1].1[1].clone();
    assert_eq!((hit.line, hit.col, hit.len), (2, 8, 4));

    e.app.open_hit(&hit);
    e.frame(vec![]);
    assert_eq!(e.app.tabs.len(), 1);
    assert!(e.app.tabs[0].path.ends_with("Outro.kt"));
    let text = &e.app.tabs[0].text;
    let (from, to) = (search::char_offset(text, 2, 8), search::char_offset(text, 2, 12));
    assert_eq!(text.chars().skip(from).take(to - from).collect::<String>(), "ação", "o trecho certo do resultado");
    assert_eq!(e.app.cursor, to, "o editor selecionou a ocorrência");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explorer_creates_a_kotlin_file_with_its_package_and_opens_it() {
    let dir = tmp("create");
    let pkg_dir = dir.join("app/src/main/java/com/exemplo/tela");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    let mut e = Env::new(&dir, None);
    assert_eq!(e.app.view, View::Explorer);

    e.app.explorer_sel = pkg_dir.clone();
    e.app.new_entry = Some(NewEntry { folder: false, name: String::new(), error: None, focus: true });
    e.frame(vec![]);
    e.type_text("Principal.kt");
    assert_eq!(e.app.new_entry.as_ref().unwrap().name, "Principal.kt", "o nome foi para a caixa do formulário");
    e.press(Key::Enter, Modifiers::NONE);

    assert!(e.app.new_entry.is_none(), "o formulário fecha ao criar: {:?}", e.app.new_entry.as_ref().and_then(|n| n.error.clone()));
    let created = pkg_dir.join("Principal.kt");
    assert_eq!(std::fs::read_to_string(&created).unwrap(), "package com.exemplo.tela\n\nclass Principal\n");
    assert_eq!(e.app.tabs.len(), 1, "o arquivo novo abre no editor");
    assert!(e.app.tabs[0].path.ends_with("Principal.kt"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explorer_refuses_bad_names_and_creates_folders() {
    let dir = tmp("create-bad");
    let mut e = Env::new(&dir, None);
    e.app.new_entry = Some(NewEntry { folder: false, name: "../fora.kt".into(), error: None, focus: false });
    e.app.create_entry();
    let error = e.app.new_entry.as_ref().and_then(|n| n.error.clone()).expect("nome que sai da pasta deveria dar erro");
    assert!(error.contains(".."), "{error}");
    assert!(!dir.parent().unwrap().join("fora.kt").exists(), "nada foi criado fora do projeto");
    e.frame(vec![]);

    e.app.new_entry = Some(NewEntry { folder: false, name: "con".into(), error: None, focus: false });
    e.app.create_entry();
    assert!(e.app.new_entry.as_ref().unwrap().error.as_ref().unwrap().contains("reservado"));

    e.app.new_entry = Some(NewEntry { folder: true, name: "docs/imagens".into(), error: None, focus: false });
    e.app.create_entry();
    assert!(dir.join("docs/imagens").is_dir());
    assert!(e.app.new_entry.is_none());
    assert_eq!(e.app.explorer_sel, dir.join("docs/imagens"), "a pasta nova fica escolhida");

    // Uma pasta fora do projeto não vale como destino.
    e.app.explorer_sel = std::env::temp_dir();
    assert_eq!(e.app.new_entry_target(), dir);
    let _ = std::fs::remove_dir_all(dir);
}
