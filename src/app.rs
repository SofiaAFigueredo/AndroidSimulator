//! Interface no estilo VS Code: barra de atividades, barra lateral, abas de editor,
//! painel inferior e barra de status.

use crate::avd;
use crate::diagnostics;
use crate::catalog::{self, Catalog, Package, Profile};
use crate::highlight;
use crate::project;
use crate::toolchain;
use crate::sdk::{self, Event};
use eframe::egui::{self, Color32, RichText};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

const BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const SIDEBAR: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
const ACTIVITY: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const ACCENT: Color32 = Color32::from_rgb(0x00, 0x7a, 0xcc);
const TEXT: Color32 = Color32::from_rgb(0xcc, 0xcc, 0xcc);
const MUTED: Color32 = Color32::from_rgb(0x85, 0x85, 0x85);
const MAX_OPEN_BYTES: u64 = 2 * 1024 * 1024;
/// Acima disso o realce é desligado, porque o texto é re-analisado a cada quadro.
const MAX_HIGHLIGHT_BYTES: usize = 300_000;

#[derive(PartialEq, Clone, Copy)]
enum View {
    Explorer,
    Devices,
}

struct Tab {
    path: PathBuf,
    text: String,
    dirty: bool,
    /// Linha para onde rolar na próxima vez que o editor for desenhado.
    goto: Option<u32>,
}

enum BuildMsg {
    Line(String),
    Done(Result<Option<PathBuf>, String>),
}

enum RunMsg {
    Line(String),
    Done(Result<(), String>),
}

struct Job {
    name: String,
    rx: Receiver<Event>,
    phase: String,
    done: u64,
    total: u64,
}

pub struct App {
    view: View,
    sdk_root: PathBuf,
    catalog: Catalog,
    catalog_rx: Option<Receiver<(Catalog, Vec<String>)>>,
    installed: BTreeMap<String, String>,
    accepted: BTreeSet<String>,
    selected_image: Option<String>,
    filter: String,
    profile: Profile,
    queue: VecDeque<Package>,
    job: Option<Job>,
    pending_license: Option<Vec<Package>>,
    folder: PathBuf,
    folder_input: String,
    tabs: Vec<Tab>,
    active: usize,
    avds: Vec<String>,
    running: Vec<(String, u16, std::process::Child)>,
    new_project: Option<(String, String)>,
    build_rx: Option<Receiver<BuildMsg>>,
    run_rx: Option<Receiver<RunMsg>>,
    last_apk: Option<PathBuf>,
    log: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = catalog::fetch_all();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        let sdk_root = sdk::default_root();
        let folder = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let avds = avd::list(&sdk_root);
        // `android-simulator [pasta] [arquivo]` abre o projeto direto no Explorador.
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (folder, view) = match args.first().map(PathBuf::from).filter(|p| p.is_dir()) {
            Some(dir) => (dir, View::Explorer),
            None => (folder, View::Devices),
        };
        let mut app = Self {
            view,
            installed: sdk::installed(&sdk_root),
            accepted: sdk::accepted_licenses(&sdk_root),
            sdk_root,
            catalog: Catalog::default(),
            catalog_rx: Some(rx),
            selected_image: None,
            filter: String::new(),
            profile: Profile::Compact,
            queue: VecDeque::new(),
            job: None,
            pending_license: None,
            folder_input: folder.display().to_string(),
            folder,
            tabs: Vec::new(),
            active: 0,
            avds,
            running: Vec::new(),
            new_project: None,
            build_rx: None,
            run_rx: None,
            last_apk: None,
            log: vec!["Lendo o catálogo oficial do Google…".into()],
        };
        if let Some(file) = args.get(1) {
            app.open_file(Path::new(file));
        }
        app
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.catalog_rx
            && let Ok((cat, warnings)) = rx.try_recv()
        {
            self.log.push(format!(
                "Catálogo carregado: {} pacotes ({} imagens de sistema).",
                cat.packages.len(),
                cat.images().len()
            ));
            self.log.extend(warnings.into_iter().map(|w| format!("Aviso: {w}")));
            sdk::ensure_metadata(&self.sdk_root, &cat);
            self.selected_image = cat.recommended_image().or(cat.images().first().copied()).map(|p| p.path.clone());
            self.catalog = cat;
            self.catalog_rx = None;
        }

        let mut build_done = None;
        if let Some(rx) = &self.build_rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    BuildMsg::Line(l) => self.log.push(l),
                    BuildMsg::Done(r) => build_done = Some(r),
                }
            }
        }
        if let Some(r) = build_done {
            self.build_rx = None;
            match r {
                Ok(Some(apk)) => {
                    self.log.push(format!("APK pronto: {}", apk.display()));
                    self.last_apk = Some(apk);
                }
                Ok(None) => self.log.push("Build concluído, mas não achei o APK.".into()),
                Err(e) => self.log.push(format!("Erro no build: {e}")),
            }
        }
        let mut run_done = None;
        if let Some(rx) = &self.run_rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    RunMsg::Line(l) => self.log.push(l),
                    RunMsg::Done(r) => run_done = Some(r),
                }
            }
        }
        if let Some(r) = run_done {
            self.run_rx = None;
            if let Err(e) = r {
                self.log.push(format!("Não consegui executar: {e}"));
            }
        }
        if self.log.len() > 5000 {
            self.log.drain(..self.log.len() - 5000);
        }
        self.running.retain_mut(|(_, _, c)| c.try_wait().map(|s| s.is_none()).unwrap_or(false));
        let mut finished = None;
        if let Some(job) = &mut self.job {
            while let Ok(ev) = job.rx.try_recv() {
                match ev {
                    Event::Phase(p) => {
                        self.log.push(p.clone());
                        job.phase = p;
                    }
                    Event::Downloaded(n) => job.done = n,
                    Event::Finished(r) => finished = Some(r),
                }
            }
        }
        if let Some(result) = finished {
            let name = self.job.take().map(|j| j.name).unwrap_or_default();
            match result {
                Ok(()) => self.log.push(format!("OK: {name} instalado.")),
                Err(e) => {
                    self.log.push(format!("Erro em {name}: {e}"));
                    self.queue.clear();
                }
            }
            self.installed = sdk::installed(&self.sdk_root);
        }
        if self.job.is_none()
            && let Some(pkg) = self.queue.pop_front()
        {
            self.start_job(pkg, ctx);
        }
    }

    fn start_job(&mut self, pkg: Package, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let root = self.sdk_root.clone();
        let ctx = ctx.clone();
        self.job = Some(Job {
            name: pkg.name.clone(),
            rx,
            phase: "Iniciando".into(),
            done: 0,
            total: pkg.size,
        });
        std::thread::spawn(move || {
            let mut emit = |e: Event| {
                let _ = tx.send(e);
                ctx.request_repaint();
            };
            let r = sdk::install(&root, &pkg, &mut emit);
            emit(Event::Finished(r));
        });
    }

    /// Pacotes que faltam para o perfil escolhido, mais a imagem selecionada.
    fn plan(&self) -> Vec<Package> {
        self.catalog.plan(self.profile, self.selected_image.as_deref(), &|p| self.installed.contains_key(p))
    }

    fn request_install(&mut self, pkgs: Vec<Package>) {
        if pkgs.iter().any(|p| !p.license.is_empty() && !self.accepted.contains(&p.license)) {
            self.pending_license = Some(pkgs);
        } else {
            self.queue.extend(pkgs);
        }
    }

    fn open_file_at(&mut self, path: &Path, line: u32) {
        self.open_file(path);
        if let Some(t) = self.tabs.iter_mut().find(|t| t.path == path) {
            t.goto = Some(line);
        }
    }

    fn open_file(&mut self, path: &Path) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = i;
            return;
        }
        let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_OPEN_BYTES).unwrap_or(false);
        if too_big {
            self.log.push(format!("{} é grande demais para o editor (limite 2 MB).", path.display()));
            return;
        }
        match std::fs::read_to_string(path) {
            Ok(text) => {
                self.tabs.push(Tab { path: path.to_owned(), text, dirty: false, goto: None });
                self.active = self.tabs.len() - 1;
            }
            Err(e) => self.log.push(format!("Não abri {}: {e}", path.display())),
        }
    }

    fn save_active(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            match std::fs::write(&t.path, &t.text) {
                Ok(()) => t.dirty = false,
                Err(e) => self.log.push(format!("Não salvei {}: {e}", t.path.display())),
            }
        }
    }

    fn activity_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("activity")
            .exact_size(48.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(ACTIVITY))
            .show(ui, |ui| {
                ui.add_space(6.0);
                for (view, tip) in [(View::Explorer, "Explorador"), (View::Devices, "Dispositivos")] {
                    let on = self.view == view;
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::click());
                    let color = if on || resp.hovered() { Color32::WHITE } else { MUTED };
                    if on {
                        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(2.0, 48.0)), 0.0, Color32::WHITE);
                    }
                    draw_icon(ui.painter(), rect.center(), view, color);
                    if resp.on_hover_text(tip).clicked() {
                        self.view = view;
                    }
                }
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("sidebar")
            .default_size(340.0)
            .size_range(220.0..=640.0)
            .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(8.0))
            .show(ui, |ui| match self.view {
                View::Explorer => self.explorer(ui),
                View::Devices => self.devices(ui),
            });
    }

    fn explorer(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("EXPLORADOR").small().color(MUTED));
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.folder_input).desired_width(ui.available_width() - 40.0));
            if (ui.button("Ir").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
                && Path::new(&self.folder_input).is_dir()
            {
                self.folder = PathBuf::from(&self.folder_input);
            }
        });
        let versions = self.build_versions();
        let is_project = self.folder.join("settings.gradle.kts").exists();
        ui.horizontal(|ui| {
            if ui.button("Novo projeto").clicked() {
                self.new_project = Some(("Meu App".into(), "com.exemplo.meuapp".into()));
            }
            let can_build = is_project && versions.is_some() && self.build_rx.is_none();
            let hint = if !is_project {
                "Abra uma pasta com um projeto Android"
            } else if versions.is_none() {
                "Baixe o perfil Completa (JDK, Gradle, plataforma e build-tools)"
            } else {
                "Compilando…"
            };
            if ui.add_enabled(can_build, egui::Button::new("Compilar")).on_disabled_hover_text(hint).clicked() {
                self.compile(ui.ctx());
            }
            let run_hint = if self.last_apk.is_none() {
                "Compile o projeto antes"
            } else if self.running.is_empty() {
                "Inicie um emulador na aba Dispositivos"
            } else {
                "Instalando…"
            };
            let can_run = self.last_apk.is_some() && !self.running.is_empty() && self.run_rx.is_none();
            if ui.add_enabled(can_run, egui::Button::new("Executar")).on_disabled_hover_text(run_hint).clicked() {
                self.run_app(ui.ctx());
            }
            if ui
                .add_enabled(!self.running.is_empty() && self.run_rx.is_none(), egui::Button::new("Captura"))
                .on_hover_text("Salva a tela do emulador como captura-AAAAMMDD-HHMMSS.png na pasta do projeto")
                .clicked()
            {
                self.screenshot(ui.ctx());
            }
        });
        if let Some(apk) = &self.last_apk {
            ui.label(RichText::new(format!("APK: {}", apk.display())).small().color(MUTED));
        }
        ui.separator();
        let mut to_open = None;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let folder = self.folder.clone();
            tree(ui, &folder, &mut to_open);
        });
        if let Some(p) = to_open {
            self.open_file(&p);
        }
    }

    fn devices(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("DISPOSITIVOS").small().color(MUTED));
        ui.label("Escolha a imagem do Android a baixar. O app usa o próprio SDK, em uma pasta separada.");
        ui.label(RichText::new(self.sdk_root.display().to_string()).small().color(MUTED));
        ui.separator();

        if self.catalog_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Carregando o catálogo do Google…");
            });
            return;
        }
        if self.catalog.packages.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, "Sem catálogo. Verifique a conexão e reabra o app.");
            return;
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.profile, Profile::Compact, "Compacta");
            ui.selectable_value(&mut self.profile, Profile::Complete, "Completa");
        });
        ui.label(
            RichText::new(match self.profile {
                Profile::Compact => "Só o necessário para rodar apps: emulador, adb e a imagem escolhida.",
                Profile::Complete => {
                    "Rodar e compilar: soma plataforma, build-tools, cmdline-tools, código-fonte do Android, NDK e CMake (versões mais recentes)."
                }
            })
            .small()
            .color(MUTED),
        );
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filtrar imagens (API, tipo, ABI)"));
        let filter = self.filter.to_lowercase();
        let recommended = self.catalog.recommended_image().map(|p| p.path.clone());
        let mut clicked = None;
        let mut remove = None;
        let list_height = (ui.available_height() - 340.0).max(120.0);
        egui::ScrollArea::vertical().max_height(list_height).auto_shrink(false).show(ui, |ui| {
            for p in self.catalog.images() {
                let label = format!(
                    "API {} · {} · {} · {}",
                    p.api.as_deref().unwrap_or("?"),
                    flavor(p),
                    p.abi.as_deref().unwrap_or("?"),
                    catalog::human_size(p.size)
                );
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                ui.horizontal(|ui| {
                    let on = self.selected_image.as_deref() == Some(p.path.as_str());
                    if ui.selectable_label(on, &label).clicked() {
                        clicked = Some(p.path.clone());
                    }
                    if recommended.as_deref() == Some(p.path.as_str()) {
                        ui.label(RichText::new("recomendada").small().color(ACCENT)).on_hover_text(
                            "A menor imagem em que o app do modelo roda e aparece na tela",
                        );
                    }
                    if self.installed.contains_key(&p.path) {
                        ui.label(RichText::new("instalado").small().color(Color32::LIGHT_GREEN));
                        if ui.small_button("remover").clicked() {
                            remove = Some(p.clone());
                        }
                    }
                });
            }
        });
        if let Some(c) = clicked {
            self.selected_image = Some(c);
        }
        if let Some(p) = remove {
            match sdk::uninstall(&self.sdk_root, &p) {
                Ok(()) => self.log.push(format!("{} removido.", p.name)),
                Err(e) => self.log.push(format!("Não removi {}: {e}", p.name)),
            }
            self.installed = sdk::installed(&self.sdk_root);
        }

        ui.separator();
        self.avd_section(ui);
        ui.separator();
        if let Some(job) = &self.job {
            ui.label(format!("{}: {}", job.name, job.phase));
            let frac = if job.total > 0 { job.done as f32 / job.total as f32 } else { 0.0 };
            ui.add(egui::ProgressBar::new(frac.min(1.0)).show_percentage());
            return;
        }
        let plan = self.plan();
        let total: u64 = plan.iter().map(|p| p.size).sum();
        if plan.is_empty() {
            ui.label("Nada a baixar para a imagem escolhida.");
        } else {
            let names: Vec<&str> = plan.iter().map(|p| p.name.as_str()).collect();
            ui.label(RichText::new(names.join(" + ")).small().color(MUTED));
            if ui.button(format!("Baixar ({})", catalog::human_size(total))).clicked() {
                self.request_install(plan);
            }
        }
    }

    fn avd_section(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("DISPOSITIVOS VIRTUAIS").small().color(MUTED));
        let ready = self.installed.contains_key("emulator") && self.installed.contains_key("platform-tools");
        let image = self
            .selected_image
            .as_ref()
            .filter(|p| self.installed.contains_key(*p))
            .and_then(|path| self.catalog.packages.iter().find(|p| &p.path == path))
            .cloned();
        if let Some(img) = &image {
            let name = avd::default_name(img);
            if !self.avds.contains(&name) && ui.button(format!("Criar {name}")).clicked() {
                match avd::create(&self.sdk_root, &name, img) {
                    Ok(()) => self.log.push(format!("AVD {name} criado.")),
                    Err(e) => self.log.push(format!("Não criei {name}: {e}")),
                }
                self.avds = avd::list(&self.sdk_root);
            }
        }
        let mut start = None;
        let mut delete = None;
        let mut stop = None;
        for name in &self.avds {
            let running = self.running.iter().any(|(n, _, _)| n == name);
            ui.horizontal(|ui| {
                ui.label(name);
                if running {
                    ui.label(RichText::new("em execução").small().color(Color32::LIGHT_GREEN));
                    if ui.small_button("parar").clicked() {
                        stop = Some(name.clone());
                    }
                } else {
                    if ui.add_enabled(ready, egui::Button::new("Iniciar")).clicked() {
                        start = Some(name.clone());
                    }
                    if ui.small_button("excluir").clicked() {
                        delete = Some(name.clone());
                    }
                }
            });
        }
        if self.avds.is_empty() {
            ui.label(RichText::new("Nenhum AVD. Instale uma imagem e crie um.").small().color(MUTED));
        }
        if let Some(name) = start {
            let port = 5554 + 2 * self.running.len() as u16;
            match avd::launch(&self.sdk_root, &name, &avd::Launch::new(false, port)) {
                Ok(child) => {
                    self.log.push(format!("Iniciando {name} (console {port})."));
                    self.running.push((name, port, child));
                }
                Err(e) => self.log.push(format!("Não iniciei {name}: {e}")),
            }
        }
        let idle = self.running.is_empty() && self.run_rx.is_none();
        if ui
            .add_enabled(idle, egui::Button::new("Compactar imagens"))
            .on_hover_text("Regrava as imagens instaladas como arquivos esparsos, sem os blocos de zeros (feche os emuladores antes)")
            .on_disabled_hover_text("Feche os emuladores e espere a tarefa atual terminar")
            .clicked()
        {
            self.compact(ui.ctx());
        }
        if let Some(name) = stop
            && let Some(i) = self.running.iter().position(|(n, _, _)| *n == name)
        {
            let (_, port, mut child) = self.running.remove(i);
            avd::stop(&self.sdk_root, &mut child, port);
            self.log.push(format!("{name} parado."));
        }
        if let Some(name) = delete {
            match avd::delete(&self.sdk_root, &name) {
                Ok(()) => self.log.push(format!("AVD {name} excluído.")),
                Err(e) => self.log.push(format!("Não excluí {name}: {e}")),
            }
            self.avds = avd::list(&self.sdk_root);
        }
    }

    /// API da plataforma e build-tools mais novos instalados; `None` se faltar o que compilar exige.
    fn build_versions(&self) -> Option<(u32, String)> {
        let has = |p: &str| self.installed.contains_key(p);
        if !has(&format!("jdk;{}", toolchain::JDK_MAJOR)) || !has(&format!("gradle;{}", toolchain::GRADLE_VERSION)) {
            return None;
        }
        let api = self
            .installed
            .keys()
            .filter_map(|k| catalog::platform_api(k))
            .max()?;
        let version = |v: &str| v.split('.').filter_map(|n| n.parse::<u32>().ok()).collect::<Vec<_>>();
        let bt = self
            .installed
            .keys()
            .filter_map(|k| k.strip_prefix("build-tools;"))
            .filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
            .max_by_key(|v| version(v))?;
        Some((api, bt.to_owned()))
    }

    fn compile(&mut self, ctx: &egui::Context) {
        if self.build_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (root, project, ctx) = (self.sdk_root.clone(), self.folder.clone(), ctx.clone());
        self.log.push(format!("Compilando {}…", project.display()));
        self.build_rx = Some(rx);
        std::thread::spawn(move || {
            let r = project::build(&root, &project, "assembleDebug", &mut |l| {
                let _ = tx.send(BuildMsg::Line(l.to_owned()));
                ctx.request_repaint();
            });
            let _ = tx.send(BuildMsg::Done(r));
            ctx.request_repaint();
        });
    }

    /// Instala o APK compilado no primeiro emulador em execução e abre o app.
    fn run_app(&mut self, ctx: &egui::Context) {
        let (Some(apk), Some(&(_, port, _))) = (self.last_apk.clone(), self.running.first()) else { return };
        let Some(package) = project::application_id(&self.folder) else {
            self.log.push("Não achei o applicationId em app/build.gradle.kts.".into());
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (root, ctx) = (self.sdk_root.clone(), ctx.clone());
        self.run_rx = Some(rx);
        std::thread::spawn(move || {
            let r = avd::run_apk(&root, port, &apk, &package, &mut |l| {
                let _ = tx.send(RunMsg::Line(l.to_owned()));
                ctx.request_repaint();
            });
            let _ = tx.send(RunMsg::Done(r));
            ctx.request_repaint();
        });
    }

    fn compact(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (root, ctx) = (self.sdk_root.clone(), ctx.clone());
        self.log.push("Compactando as imagens instaladas…".into());
        self.run_rx = Some(rx);
        std::thread::spawn(move || {
            let saved = sdk::compact_installed(&root, &mut |l| {
                let _ = tx.send(RunMsg::Line(l.to_owned()));
                ctx.request_repaint();
            });
            let _ = tx.send(RunMsg::Line(format!("Compactação concluída: {} MB liberados.", saved >> 20)));
            let _ = tx.send(RunMsg::Done(Ok(())));
            ctx.request_repaint();
        });
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some(&(_, port, _)) = self.running.first() else { return };
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let out = self.folder.join(format!("captura-{secs}.png"));
        let (tx, rx) = mpsc::channel();
        let (root, ctx) = (self.sdk_root.clone(), ctx.clone());
        self.run_rx = Some(rx);
        std::thread::spawn(move || {
            let r = avd::screenshot(&root, port, &out);
            if r.is_ok() {
                let _ = tx.send(RunMsg::Line(format!("Captura salva em {}", out.display())));
            }
            let _ = tx.send(RunMsg::Done(r));
            ctx.request_repaint();
        });
    }

    fn new_project_window(&mut self, ctx: &egui::Context) {
        let folder = self.folder.clone();
        let versions = self.build_versions();
        let Some((name, package)) = &mut self.new_project else { return };
        let slug: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_owned();
        let target = folder.join(&slug);
        let mut decision = None;
        egui::Window::new("Novo projeto Android")
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Nome do app");
                ui.text_edit_singleline(name);
                ui.label("Pacote");
                ui.text_edit_singleline(package);
                ui.label(RichText::new(format!("Será criado em {}", target.display())).small().color(MUTED));
                if versions.is_none() {
                    ui.colored_label(Color32::LIGHT_YELLOW, "Para compilar, baixe antes o perfil Completa.");
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!slug.is_empty(), egui::Button::new("Criar")).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancelar").clicked() {
                        decision = Some(false);
                    }
                });
            });
        let Some(create) = decision else { return };
        if create && let Some((name, package)) = self.new_project.clone() {
            let (api, bt) = versions.unwrap_or((catalog::MAX_PLATFORM_API as u32, "36.0.0".into()));
            let spec = project::NewProject { name: &name, package: &package, compile_sdk: api, build_tools: &bt, sdk_root: &self.sdk_root };
            match project::create(&target, &spec) {
                Ok(()) => {
                    self.log.push(format!("Projeto criado em {}.", target.display()));
                    self.folder_input = target.display().to_string();
                    self.folder = target.clone();
                    let main: PathBuf = package.split('.').collect();
                    self.open_file(&target.join("app/src/main/java").join(main).join("MainActivity.kt"));
                    self.view = View::Explorer;
                }
                Err(e) => {
                    self.log.push(format!("Não criei o projeto: {e}"));
                    return;
                }
            }
        }
        self.new_project = None;
    }

    fn license_window(&mut self, ctx: &egui::Context) {
        let Some(pkgs) = &self.pending_license else { return };
        let mut ids: Vec<String> = pkgs
            .iter()
            .map(|p| p.license.clone())
            .filter(|l| !l.is_empty() && !self.accepted.contains(l))
            .collect();
        ids.sort();
        ids.dedup();
        let mut decision = None;
        egui::Window::new("Termos de licença do Android SDK")
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Os componentes são do Google e exigem que você leia e aceite estes termos.");
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    for id in &ids {
                        let text = self.catalog.licenses.get(id).map(String::as_str).unwrap_or("(texto indisponível)");
                        ui.label(RichText::new(text).small());
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Aceitar e baixar").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancelar").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match decision {
            Some(true) => {
                for id in &ids {
                    if let Err(e) = sdk::accept_license(&self.sdk_root, id) {
                        self.log.push(format!("Não gravei a licença {id}: {e}"));
                    }
                }
                self.accepted = sdk::accepted_licenses(&self.sdk_root);
                if let Some(p) = self.pending_license.take() {
                    self.queue.extend(p);
                }
            }
            Some(false) => self.pending_license = None,
            None => {}
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Abra um arquivo no Explorador").color(MUTED));
            });
            return;
        }
        let mut close = None;
        ui.horizontal(|ui| {
            for (i, t) in self.tabs.iter().enumerate() {
                let name = t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let title = if t.dirty { format!("• {name}") } else { name };
                if ui.selectable_label(i == self.active, title).clicked() {
                    self.active = i;
                }
                if ui.small_button("×").clicked() {
                    close = Some(i);
                }
                ui.add_space(6.0);
            }
        });
        ui.separator();
        if let Some(i) = close {
            self.tabs.remove(i);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
            return;
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            let lang = if t.text.len() > MAX_HIGHLIGHT_BYTES { highlight::Lang::Plain } else { highlight::lang_for(&t.path) };
            let font = egui::FontId::monospace(14.0);
            let lines = t.text.split('\n').count();
            let width = lines.to_string().len();
            let gutter = (1..=lines).map(|n| format!("{n:>width$}")).collect::<Vec<_>>().join("\n");
            let rows = (ui.available_height() / 18.0) as usize;
            let row = ui.painter().layout_no_wrap("A".into(), font.clone(), Color32::WHITE).size().y;
            let mut area = egui::ScrollArea::both().auto_shrink(false);
            if let Some(line) = t.goto.take() {
                area = area.vertical_scroll_offset((line.saturating_sub(1) as f32 * row - 120.0).max(0.0));
            }
            area.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.add(egui::Label::new(RichText::new(gutter).font(font.clone()).color(MUTED)).selectable(false));
                    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                        ui.painter().layout_job(highlight::layout_job(text.as_str(), lang, font.clone()))
                    };
                    let r = ui.add(
                        egui::TextEdit::multiline(&mut t.text)
                            .font(font.clone())
                            .lock_focus(true)
                            .desired_width(f32::INFINITY)
                            .desired_rows(rows)
                            .layouter(&mut layouter)
                            .frame(egui::Frame::NONE),
                    );
                    if r.changed() {
                        t.dirty = true;
                    }
                });
            });
        }
    }
}

fn flavor(p: &Package) -> &str {
    match p.path.split(';').nth(2) {
        Some("default") => "AOSP",
        Some("google_apis") => "Google APIs",
        Some("google_apis_playstore") => "Google Play",
        Some("google_atd") => "ATD (sem tela)",
        Some(other) => other,
        None => "?",
    }
}

fn draw_icon(p: &egui::Painter, c: egui::Pos2, view: View, color: Color32) {
    let stroke = egui::Stroke::new(1.8, color);
    match view {
        View::Explorer => {
            let back = egui::Rect::from_center_size(c + egui::vec2(3.0, -3.0), egui::vec2(14.0, 18.0));
            let front = egui::Rect::from_center_size(c + egui::vec2(-3.0, 3.0), egui::vec2(14.0, 18.0));
            p.rect_stroke(back, 1.0, stroke, egui::StrokeKind::Middle);
            p.rect_filled(front, 1.0, ACTIVITY);
            p.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Middle);
        }
        View::Devices => {
            let phone = egui::Rect::from_center_size(c, egui::vec2(14.0, 24.0));
            p.rect_stroke(phone, 3.0, stroke, egui::StrokeKind::Middle);
            p.line_segment([c + egui::vec2(-3.0, 8.0), c + egui::vec2(3.0, 8.0)], stroke);
        }
    }
}

fn tree(ui: &mut egui::Ui, dir: &Path, to_open: &mut Option<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| (!e.path().is_dir(), e.file_name().to_string_lossy().to_lowercase()));
    for e in entries.into_iter().take(500) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            egui::CollapsingHeader::new(name).id_salt(&path).show(ui, |ui| tree(ui, &path, to_open));
        } else if ui.selectable_label(false, format!("   {name}")).clicked() {
            *to_open = Some(path);
        }
    }
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SIDEBAR;
    v.extreme_bg_color = BG;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_rgb(0x09, 0x4a, 0x7a);
    ctx.set_visuals(v);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.poll(ctx);
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save_active();
        }

        egui::Panel::bottom("status")
            .exact_size(22.0)
            .frame(egui::Frame::new().fill(ACCENT).inner_margin(egui::Margin::symmetric(8, 2)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let installed = self.installed.len();
                    ui.label(RichText::new(format!("SDK: {installed} pacote(s) instalado(s)")).color(Color32::WHITE).small());
                    if let Some(job) = &self.job {
                        ui.label(RichText::new(job.phase.clone()).color(Color32::WHITE).small());
                    }
                });
            });

        egui::Panel::bottom("output")
            .resizable(true)
            .default_size(150.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(8.0))
            .show(ui, |ui| {
                ui.label(RichText::new("SAÍDA").small().color(MUTED));
                let mut jump = None;
                let row = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show_rows(
                    ui,
                    row,
                    self.log.len(),
                    |ui, range| {
                        for l in &self.log[range] {
                            match diagnostics::parse_line(l).filter(|d| d.path.is_file()) {
                                Some(d) => {
                                    let color = if d.error { Color32::from_rgb(0xf4, 0x87, 0x71) } else { Color32::from_rgb(0xcc, 0xa7, 0x00) };
                                    let r = ui.add(egui::Label::new(RichText::new(l).monospace().small().color(color).underline()).sense(egui::Sense::click()));
                                    if r.on_hover_text("Abrir no editor").clicked() {
                                        jump = Some(d);
                                    }
                                }
                                None => {
                                    ui.label(RichText::new(l).monospace().small());
                                }
                            }
                        }
                    },
                );
                if let Some(d) = jump {
                    self.open_file_at(&d.path, d.line);
                }
            });

        self.activity_bar(ui);
        self.sidebar(ui);
        egui::CentralPanel::default().show(ui, |ui| self.editor(ui));
        self.license_window(ctx);
        self.new_project_window(ctx);

        if self.job.is_some() || self.build_rx.is_some() || self.run_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}
