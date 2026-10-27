//! Interface no estilo VS Code: barra de atividades, barra lateral, abas de editor,
//! painel inferior e barra de status.

use crate::autocomplete::{self, Manager as AutoComplete, Popup};
use crate::avd;
use crate::diagnostics;
use crate::catalog::{self, Catalog, Package, Profile};
use crate::highlight;
use crate::logcat::LogcatView;
use crate::terminal::Terminal;
use crate::lsp;
use crate::problems::{self, Problem};
use crate::project;
use crate::files;
use crate::search::{self, Hit};
use crate::servers;
use crate::simulator::SimulatorView;
use crate::toolchain;
use crate::sdk::{self, Event};
use eframe::egui::{self, Color32, RichText};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

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
enum Bottom {
    Output,
    Problems,
    Logcat,
    Terminal,
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum View {
    Explorer,
    Search,
    Devices,
}

struct Tab {
    path: PathBuf,
    text: String,
    dirty: bool,
    /// Linha para onde rolar na próxima vez que o editor for desenhado.
    goto: Option<u32>,
    /// Versão do documento no servidor de linguagem; 0 enquanto não foi aberto lá.
    lsp_version: i32,
}

/// A barra de busca do editor (Ctrl+F) e a de substituição (Ctrl+H).
#[derive(Default)]
struct Find {
    open: bool,
    show_replace: bool,
    query: String,
    replace: String,
    case_sensitive: bool,
    /// Pedir o foco para a caixa de busca no próximo quadro.
    focus: bool,
    /// Índice da ocorrência selecionada e o trecho dela, em bytes.
    current: Option<usize>,
    selected: Option<(usize, usize)>,
}

enum SearchMsg {
    File(Vec<Hit>),
    Done(search::Stats),
}

/// A busca em todos os arquivos do projeto (Ctrl+Shift+F).
#[derive(Default)]
struct SearchState {
    query: String,
    case_sensitive: bool,
    /// Resultados agrupados por arquivo, na ordem em que chegam.
    results: Vec<(PathBuf, Vec<Hit>)>,
    rx: Option<Receiver<SearchMsg>>,
    cancel: Option<Arc<AtomicBool>>,
    stats: Option<search::Stats>,
    focus: bool,
}

/// O formulário de arquivo ou pasta novos, no Explorador.
struct NewEntry {
    folder: bool,
    name: String,
    error: Option<String>,
    focus: bool,
}

/// Pedido de completion em andamento.
struct Pending {
    rx: Receiver<Result<serde_json::Value, String>>,
    tab: usize,
    origin: usize,
    /// Pedido feito com Ctrl+Espaço (e não ao digitar): vale avisar se vier vazio.
    explicit: bool,
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
    ac: AutoComplete,
    /// A tela do dispositivo ao vivo, no painel da direita.
    sim: SimulatorView,
    find: Find,
    search: SearchState,
    new_entry: Option<NewEntry>,
    /// A pasta escolhida no Explorador (onde o arquivo novo é criado).
    explorer_sel: PathBuf,
    /// O que o Gradle reclamou na última compilação, por arquivo (chave: `lsp::uri_key`); é apagado do
    /// arquivo que o usuário edita, porque as posições deixam de valer.
    build_problems: HashMap<String, (PathBuf, Vec<Problem>)>,
    show_sim: bool,
    /// Abrir o emulador na janela dele, em vez de dentro do app.
    separate_window: bool,
    popup: Option<Popup>,
    pending: Option<Pending>,
    /// Cursor (índice de caractere) do editor no último quadro.
    cursor: usize,
    /// Posição em que o cursor deve ficar no próximo quadro (depois de aceitar uma sugestão).
    set_cursor: Option<(usize, usize)>,
    bottom: Bottom,
    logcat: LogcatView,
    terminal: Terminal,
    /// Pasta que o terminal seguiu da última vez; o `cd` do usuário não é desfeito a cada quadro.
    term_folder: PathBuf,
    new_project: Option<(String, String)>,
    build_rx: Option<Receiver<BuildMsg>>,
    run_rx: Option<Receiver<RunMsg>>,
    last_apk: Option<PathBuf>,
    log: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // `android-simulator [pasta] [arquivo]` abre o projeto direto no Explorador.
        Self::build(&cc.egui_ctx, std::env::args().skip(1).collect())
    }

    /// Monta o app a partir de um contexto do egui (sem janela), para os testes poderem criá-lo.
    fn build(ctx: &egui::Context, args: Vec<String>) -> Self {
        apply_theme(ctx);
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = catalog::fetch_all();
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        let sdk_root = sdk::default_root();
        let folder = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let avds = avd::list(&sdk_root);
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
            ac: AutoComplete::default(),
            sim: SimulatorView::default(),
            find: Find::default(),
            search: SearchState::default(),
            new_entry: None,
            explorer_sel: folder.clone(),
            build_problems: HashMap::new(),
            show_sim: false,
            separate_window: false,
            popup: None,
            pending: None,
            cursor: 0,
            set_cursor: None,
            terminal: Terminal::new(folder.clone()),
            term_folder: folder.clone(),
            bottom: Bottom::Output,
            logcat: LogcatView::default(),
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
                    BuildMsg::Line(l) => {
                        if let Some(d) = diagnostics::parse_line(&l) {
                            let key = lsp::uri_key(&lsp::path_to_uri(&d.path));
                            self.build_problems.entry(key).or_insert_with(|| (d.path.clone(), Vec::new())).1.push(problems::from_gradle(&d));
                        }
                        self.log.push(l);
                    }
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
        self.catalog.plan(self.profile, self.selected_image.as_deref(), &|p| {
            self.installed.get(&p.path).is_some_and(|rev| *rev == p.revision_text())
        })
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
                self.tabs.push(Tab { path: path.to_owned(), text, dirty: false, goto: None, lsp_version: 0 });
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
                for (view, tip) in [(View::Explorer, "Explorador"), (View::Search, "Busca nos arquivos (Ctrl+Shift+F)"), (View::Devices, "Dispositivos")] {
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
                // O simulador não troca a barra lateral: abre e fecha o painel da direita.
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::click());
                let color = if self.show_sim || resp.hovered() { Color32::WHITE } else { MUTED };
                if self.show_sim {
                    ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(2.0, 48.0)), 0.0, Color32::WHITE);
                }
                draw_simulator_icon(ui.painter(), rect.center(), color);
                if resp.on_hover_text("Simulador (tela do dispositivo)").clicked() {
                    self.show_sim = !self.show_sim;
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
                View::Search => self.search_view(ui),
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
        ui.horizontal(|ui| {
            if ui.button("Novo arquivo").on_hover_text("Cria na pasta escolhida na árvore (clique numa pasta para escolher)").clicked() {
                self.new_entry = Some(NewEntry { folder: false, name: String::new(), error: None, focus: true });
            }
            if ui.button("Nova pasta").clicked() {
                self.new_entry = Some(NewEntry { folder: true, name: String::new(), error: None, focus: true });
            }
        });
        if self.new_entry.is_some() {
            self.new_entry_form(ui);
        }
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
        let (mut select, mut new_in) = (None, None);
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let folder = self.folder.clone();
            let root_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.display().to_string());
            if ui.selectable_label(self.explorer_sel == folder, format!("{root_name}  (pasta raiz)")).clicked() {
                select = Some(folder.clone());
            }
            tree(ui, &folder, &mut to_open, &self.explorer_sel, &mut select, &mut new_in);
        });
        if let Some(p) = select {
            self.explorer_sel = p;
        }
        if let Some((p, is_folder)) = new_in {
            self.explorer_sel = p;
            self.new_entry = Some(NewEntry { folder: is_folder, name: String::new(), error: None, focus: true });
        }
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
        ui.checkbox(&mut self.separate_window, "Emulador em janela separada").on_hover_text("Por padrão o emulador roda sem janela e a tela aparece no painel do simulador");
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
            match avd::launch(&self.sdk_root, &name, &avd::Launch::new(!self.separate_window, port)) {
                Ok(child) => {
                    self.log.push(format!("Iniciando {name} (console {port})."));
                    self.running.push((name, port, child));
                    // Sem janela própria, a tela do emulador aparece no painel do simulador.
                    self.show_sim = !self.separate_window;
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
        self.build_problems.clear();
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
        self.show_sim = !self.separate_window;
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

    /// Saída do app (downloads, Gradle, emulador). Linhas de erro com arquivo e linha abrem o editor.
    fn output_view(&mut self, ui: &mut egui::Ui) {
        let mut jump = None;
        let row = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show_rows(ui, row, self.log.len(), |ui, range| {
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
        });
        if let Some(d) = jump {
            self.open_file_at(&d.path, d.line);
        }
    }

    /// Todos os problemas: dos arquivos abertos (servidor de linguagem + Gradle) e os da última compilação
    /// em arquivos que não estão abertos. Erros primeiro.
    fn all_problems(&self) -> Vec<(PathBuf, Problem)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for t in &self.tabs {
            let uri = lsp::path_to_uri(&t.path);
            for p in tab_problems(&self.ac, &self.build_problems, t, highlight::lang_for(&t.path), &uri) {
                out.push((t.path.clone(), p));
            }
            seen.insert(lsp::uri_key(&uri));
        }
        for (key, (path, list)) in &self.build_problems {
            if !seen.contains(key) {
                out.extend(list.iter().map(|p| (path.clone(), p.clone())));
            }
        }
        out.sort_by(|a, b| a.1.severity.cmp(&b.1.severity).then_with(|| a.0.cmp(&b.0)).then_with(|| a.1.line.cmp(&b.1.line)));
        out
    }

    /// A aba Problemas: clicar numa linha abre o arquivo no lugar.
    fn problems_view(&mut self, ui: &mut egui::Ui, list: &[(PathBuf, Problem)]) {
        if list.is_empty() {
            ui.label(RichText::new("Nenhum problema nos arquivos abertos nem na última compilação.").small().color(MUTED));
            return;
        }
        let row = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        let mut jump = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row, list.len(), |ui, range| {
            for (path, p) in &list[range] {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let text = format!("{:<5} {name}:{}:{}  {}  [{}]", p.severity.label(), p.line + 1, p.col + 1, p.message, p.source);
                let label = egui::Label::new(RichText::new(text).monospace().small().color(p.severity.color())).sense(egui::Sense::click());
                if ui.add(label).on_hover_text("Abrir no editor").clicked() {
                    jump = Some((path.clone(), p.line + 1));
                }
            }
        });
        if let Some((path, line)) = jump {
            self.open_file_at(&path, line);
        }
    }

    /// Atalhos de busca: Ctrl+F (no arquivo), Ctrl+H (substituir), Ctrl+Shift+F (nos arquivos) e F3.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        // As variantes com Shift primeiro: o egui deixa um Ctrl+Shift+F também casar com Ctrl+F.
        let (search_all, find, replace, prev, next) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F),
                i.consume_key(Modifiers::COMMAND, Key::F),
                i.consume_key(Modifiers::COMMAND, Key::H),
                i.consume_key(Modifiers::SHIFT, Key::F3),
                i.consume_key(Modifiers::NONE, Key::F3),
            )
        });
        if search_all {
            self.view = View::Search;
            self.search.focus = true;
        }
        if (find || replace) && !self.tabs.is_empty() {
            self.find.open = true;
            self.find.focus = true;
            self.find.show_replace |= replace;
        }
        if (prev || next) && !self.find.query.is_empty() && !self.tabs.is_empty() {
            self.find.open = true;
            self.find_jump(!prev);
        }
    }

    /// Seleciona a ocorrência `idx` de `matches` no arquivo ativo e rola até ela.
    fn select_match(&mut self, idx: usize, matches: &[(usize, usize)]) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        let Some(&(a, b)) = matches.get(idx) else { return };
        self.find.current = Some(idx);
        self.find.selected = Some((a, b));
        let (from, to) = (t.text[..a].chars().count(), t.text[..b].chars().count());
        self.set_cursor = Some((from, to));
        self.cursor = to;
        t.goto = Some(t.text[..a].matches('\n').count() as u32 + 1);
    }

    /// Vai para a próxima (ou anterior) ocorrência da busca, dando a volta no arquivo.
    fn find_jump(&mut self, forward: bool) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let matches = search::find_all(&t.text, &self.find.query, self.find.case_sensitive);
        if matches.is_empty() {
            self.find.current = None;
            self.find.selected = None;
            return;
        }
        let cursor_byte = char_to_byte(&t.text, self.cursor);
        // Se a ocorrência selecionada ainda é a que a busca marcou, anda a partir dela.
        let idx = match self.find.current.filter(|c| self.find.selected == matches.get(*c).copied()) {
            Some(c) => search::step(c, matches.len(), forward),
            None if forward => search::pick_from(&matches, cursor_byte).unwrap_or(0),
            None => search::pick_before(&matches, cursor_byte),
        };
        self.select_match(idx, &matches);
    }

    /// A consulta mudou: seleciona a primeira ocorrência a partir de onde a anterior estava.
    fn find_refresh(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let matches = search::find_all(&t.text, &self.find.query, self.find.case_sensitive);
        let from = self.find.selected.map_or_else(|| char_to_byte(&t.text, self.cursor), |s| s.0);
        self.find.current = None;
        self.find.selected = None;
        if let Some(i) = search::pick_from(&matches, from) {
            self.select_match(i, &matches);
        }
    }

    /// Troca o texto do arquivo ativo (substituição) e avisa o servidor de linguagem.
    fn edit_active(&mut self, text: String) {
        let Some(t) = self.tabs.get_mut(self.active) else { return };
        t.text = text;
        t.dirty = true;
        let uri = lsp::path_to_uri(&t.path);
        self.build_problems.remove(&lsp::uri_key(&uri));
        if t.lsp_version > 0 {
            t.lsp_version += 1;
            if let Some(c) = self.ac.client(highlight::lang_for(&t.path)) {
                let _ = c.did_change(&uri, t.lsp_version, &t.text);
            }
        }
        self.popup = None;
    }

    fn replace_current(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let matches = search::find_all(&t.text, &self.find.query, self.find.case_sensitive);
        match self.find.selected.filter(|s| matches.contains(s)) {
            None => self.find_jump(true),
            Some(sel) => {
                let after = t.text[..sel.0].chars().count() + self.find.replace.chars().count();
                let new = search::replace_range(&t.text, sel, &self.find.replace);
                self.edit_active(new);
                self.cursor = after;
                self.find.current = None;
                self.find.selected = None;
                self.find_jump(true);
            }
        }
    }

    fn replace_all(&mut self) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let (new, n) = search::replace_all(&t.text, &self.find.query, &self.find.replace, self.find.case_sensitive);
        if n > 0 {
            self.edit_active(new);
            self.find.current = None;
            self.find.selected = None;
            self.log.push(format!("{n} ocorrência(s) substituída(s)."));
        }
    }

    /// A barra de busca (e de substituição) acima do editor.
    fn find_bar(&mut self, ui: &mut egui::Ui) {
        let total = self.tabs.get(self.active).map_or(0, |t| search::find_all(&t.text, &self.find.query, self.find.case_sensitive).len());
        let (mut jump, mut close, mut refresh, mut replace_one, mut replace_every) = (None, false, false, false, false);
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.find.query).id(egui::Id::unique("find-query")).hint_text("Buscar").desired_width(220.0));
            if self.find.focus {
                r.request_focus();
                self.find.focus = false;
            }
            if r.changed() {
                refresh = true;
            }
            let (enter, shift, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
            if r.lost_focus() && enter {
                jump = Some(!shift);
                r.request_focus();
            }
            if (r.has_focus() || r.lost_focus()) && esc {
                close = true;
            }
            if ui.selectable_label(self.find.case_sensitive, "Aa").on_hover_text("Diferenciar maiúsculas e minúsculas").clicked() {
                self.find.case_sensitive = !self.find.case_sensitive;
                refresh = true;
            }
            let count = if self.find.query.is_empty() {
                String::new()
            } else if total == 0 {
                "sem resultados".to_owned()
            } else {
                match self.find.current {
                    Some(c) => format!("{} de {total}", c + 1),
                    None => format!("{total} resultados"),
                }
            };
            ui.label(RichText::new(count).small().color(MUTED));
            if ui.small_button("<").on_hover_text("Anterior (Shift+F3)").clicked() {
                jump = Some(false);
            }
            if ui.small_button(">").on_hover_text("Próxima (F3)").clicked() {
                jump = Some(true);
            }
            if ui.small_button("Substituir…").on_hover_text("Mostrar ou esconder a substituição (Ctrl+H)").clicked() {
                self.find.show_replace = !self.find.show_replace;
            }
            if ui.small_button("×").on_hover_text("Fechar (Esc)").clicked() {
                close = true;
            }
        });
        if self.find.show_replace {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.find.replace).hint_text("Substituir por").desired_width(220.0));
                if ui.small_button("Substituir").clicked() {
                    replace_one = true;
                }
                if ui.small_button("Todas").clicked() {
                    replace_every = true;
                }
            });
        }
        if refresh {
            self.find_refresh();
        }
        if let Some(forward) = jump {
            self.find_jump(forward);
        }
        if replace_one {
            self.replace_current();
        }
        if replace_every {
            self.replace_all();
        }
        if close {
            self.find.open = false;
        }
    }

    /// Começa a busca nos arquivos do projeto, numa thread; os resultados chegam aos poucos.
    fn start_search(&mut self, ctx: &egui::Context) {
        if let Some(c) = self.search.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.search.results.clear();
        self.search.stats = None;
        self.search.rx = None;
        if self.search.query.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.search.cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = mpsc::channel();
        self.search.rx = Some(rx);
        let (root, query, case, ctx) = (self.folder.clone(), self.search.query.clone(), self.search.case_sensitive, ctx.clone());
        std::thread::spawn(move || {
            let (files_tx, files_ctx) = (tx.clone(), ctx.clone());
            let stats = search::search_dir(&root, &query, case, &cancel, |hits| {
                let _ = files_tx.send(SearchMsg::File(hits));
                files_ctx.request_repaint();
            });
            let _ = tx.send(SearchMsg::Done(stats));
            ctx.request_repaint();
        });
    }

    fn poll_search(&mut self) {
        let Some(rx) = &self.search.rx else { return };
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                SearchMsg::File(hits) => {
                    if let Some(first) = hits.first() {
                        let path = first.path.clone();
                        self.search.results.push((path, hits));
                    }
                }
                SearchMsg::Done(stats) => {
                    self.search.stats = Some(stats);
                    done = true;
                }
            }
        }
        if done {
            self.search.rx = None;
        }
    }

    /// Abre o arquivo do resultado e seleciona a ocorrência.
    fn open_hit(&mut self, hit: &Hit) {
        self.open_file_at(&hit.path, hit.line as u32);
        if let Some(t) = self.tabs.iter().find(|t| t.path == hit.path) {
            let from = search::char_offset(&t.text, hit.line, hit.col);
            self.set_cursor = Some((from, from + hit.len));
            self.cursor = from + hit.len;
        }
    }

    /// A aba de busca nos arquivos.
    fn search_view(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("BUSCA").small().color(MUTED));
        let mut run = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.search.query).hint_text("Buscar nos arquivos").desired_width(ui.available_width() - 90.0));
            if self.search.focus {
                r.request_focus();
                self.search.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                run = true;
                r.request_focus();
            }
            if ui.selectable_label(self.search.case_sensitive, "Aa").on_hover_text("Diferenciar maiúsculas e minúsculas").clicked() {
                self.search.case_sensitive = !self.search.case_sensitive;
                run = !self.search.query.is_empty();
            }
            if ui.button("Ir").clicked() {
                run = true;
            }
        });
        if run {
            self.start_search(&ui.ctx().clone());
        }
        let folder = self.folder.clone();
        ui.label(RichText::new(format!("em {} (arquivos salvos)", folder.display())).small().color(MUTED));
        if self.search.rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Buscando…");
            });
        } else if let Some(stats) = &self.search.stats {
            let more = if stats.truncated { " (limite de resultados atingido: refine a busca)" } else { "" };
            ui.label(RichText::new(format!("{} resultado(s) em {} arquivo(s){more}", stats.hits, self.search.results.len())).small());
        }
        ui.separator();
        let mut open = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (path, hits) in &self.search.results {
                let rel = path.strip_prefix(&folder).unwrap_or(path).display().to_string();
                egui::CollapsingHeader::new(format!("{rel} ({})", hits.len())).id_salt(path).default_open(true).show(ui, |ui| {
                    for h in hits {
                        let line = RichText::new(format!("{:>4}: {}", h.line, h.preview)).monospace().small();
                        if ui.selectable_label(false, line).clicked() {
                            open = Some(h.clone());
                        }
                    }
                });
            }
        });
        if let Some(hit) = open {
            self.open_hit(&hit);
        }
    }

    /// A pasta onde o arquivo ou pasta novos serão criados: a escolhida na árvore, se for do projeto.
    fn new_entry_target(&self) -> PathBuf {
        if self.explorer_sel.is_dir() && self.explorer_sel.starts_with(&self.folder) { self.explorer_sel.clone() } else { self.folder.clone() }
    }

    fn create_entry(&mut self) {
        let Some(entry) = &self.new_entry else { return };
        let (is_folder, name) = (entry.folder, entry.name.clone());
        let base = self.new_entry_target();
        let result = if is_folder { files::create_dir(&base, &name) } else { files::create_file(&base, &name) };
        match result {
            Ok(path) => {
                self.log.push(format!("Criado {}.", path.display()));
                if is_folder {
                    self.explorer_sel = path;
                } else {
                    self.explorer_sel = path.parent().map(Path::to_path_buf).unwrap_or(base);
                    self.open_file(&path);
                }
                self.new_entry = None;
            }
            Err(e) => {
                if let Some(entry) = &mut self.new_entry {
                    entry.error = Some(e);
                }
            }
        }
    }

    fn new_entry_form(&mut self, ui: &mut egui::Ui) {
        let target = self.new_entry_target();
        let rel = target.strip_prefix(&self.folder).map(|p| p.display().to_string()).unwrap_or_default();
        let rel = if rel.is_empty() { ".".to_owned() } else { rel };
        let (mut create, mut cancel) = (false, false);
        let Some(entry) = &mut self.new_entry else { return };
        ui.label(RichText::new(format!("{} em {rel}", if entry.folder { "Nova pasta" } else { "Novo arquivo" })).small().color(MUTED));
        ui.horizontal(|ui| {
            let hint = if entry.folder { "nome ou caminho/da/pasta" } else { "Nome.kt ou pasta/Nome.kt" };
            let r = ui.add(egui::TextEdit::singleline(&mut entry.name).hint_text(hint).desired_width(ui.available_width() - 100.0));
            if entry.focus {
                r.request_focus();
                entry.focus = false;
            }
            if r.changed() {
                entry.error = None;
            }
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if r.lost_focus() && enter {
                create = true;
            }
            if (r.has_focus() || r.lost_focus()) && esc {
                cancel = true;
            }
            if ui.button("Criar").clicked() {
                create = true;
            }
            if ui.button("×").clicked() {
                cancel = true;
            }
        });
        if let Some(e) = &entry.error {
            ui.colored_label(Color32::LIGHT_RED, e.clone());
        }
        if cancel {
            self.new_entry = None;
        } else if create {
            self.create_entry();
        }
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
            let t = self.tabs.remove(i);
            if t.lsp_version > 0
                && let Some(c) = self.ac.client(highlight::lang_for(&t.path))
            {
                let _ = c.did_close(&lsp::path_to_uri(&t.path));
            }
            self.popup = None;
            self.pending = None;
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
            return;
        }
        if self.find.open {
            self.find_bar(ui);
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
            let (path_lang, uri) = (highlight::lang_for(&t.path), lsp::path_to_uri(&t.path));
            let ctx = ui.ctx().clone();
            let id = egui::Id::unique(("editor", &t.path));
            let supported = servers::language_id(path_lang).is_some() && t.text.len() <= MAX_HIGHLIGHT_BYTES;
            let mut changed_text = false;

            // Problemas deste arquivo (LSP + Gradle): viram o sublinhado e o tooltip.
            let tab_problems = tab_problems(&self.ac, &self.build_problems, t, path_lang, &uri);
            let mut marks = problems::marks(&t.text, &tab_problems);
            if self.find.open && !self.find.query.is_empty() {
                let found = search::find_all(&t.text, &self.find.query, self.find.case_sensitive);
                marks.extend(find_marks(&found, self.find.selected));
            }

            // Resposta do servidor a um pedido de completion.
            if let Some(p) = &self.pending {
                match p.rx.try_recv() {
                    Ok(result) => {
                        let (tab, origin, explicit) = (p.tab, p.origin, p.explicit);
                        self.pending = None;
                        match result {
                            Ok(v) if tab == self.active => {
                                let items = lsp::parse_completion(&v);
                                if items.is_empty() && explicit {
                                    self.log.push("Autocomplete: nenhuma sugestão (o servidor pode ainda estar analisando o projeto; tente de novo).".into());
                                }
                                self.popup = (!items.is_empty()).then(|| Popup { items, incomplete: lsp::is_incomplete(&v), selected: 0, origin });
                            }
                            Ok(_) => {}
                            Err(e) => self.log.push(format!("Autocomplete: {e}")),
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
                }
            }

            // O cursor pedido no quadro anterior (depois de aceitar uma sugestão).
            if let Some((from, to)) = self.set_cursor.take() {
                let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(from), egui::text::CCursor::new(to))));
                state.store(&ctx, id);
            }

            // Teclas do popup, tomadas antes de o editor vê-las (senão Enter viraria quebra de linha).
            let start = autocomplete::prefix_start(&t.text, self.cursor);
            let prefix = autocomplete::prefix_text(&t.text, start, self.cursor);
            let visible_len = self.popup.as_ref().map_or(0, |p| autocomplete::matching(&p.items, &prefix).len());
            let mut accept = None;
            let mut trigger = false;
            let mut explicit = false;
            if visible_len > 0
                && let Some(p) = &mut self.popup
            {
                ui.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        p.selected = (p.selected + 1) % visible_len;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        p.selected = (p.selected + visible_len - 1) % visible_len;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
                        accept = Some(p.selected.min(visible_len - 1));
                    }
                });
                if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                    self.popup = None;
                }
            }
            if supported {
                explicit = ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Space));
                trigger = explicit;
            }
            if let Some(n) = accept.take()
                && let Some(p) = self.popup.take()
                && let Some(item) = autocomplete::matching(&p.items, &prefix).get(n).copied().cloned()
            {
                let (text, (from, to)) = autocomplete::apply(&t.text, self.cursor, start, &item);
                t.text = text;
                self.cursor = to;
                let mut state = egui::TextEdit::load_state(&ctx, id).unwrap_or_default();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(from), egui::text::CCursor::new(to))));
                state.store(&ctx, id);
                changed_text = true;
            }

            area.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.add(egui::Label::new(RichText::new(gutter).font(font.clone()).color(MUTED)).selectable(false));
                    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap: f32| {
                        ui.painter().layout_job(highlight::layout_job(text.as_str(), lang, font.clone(), &marks))
                    };
                    let out = egui::TextEdit::multiline(&mut t.text)
                        .id(id)
                        .font(font.clone())
                        .lock_focus(true)
                        .desired_width(f32::INFINITY)
                        .desired_rows(rows)
                        .layouter(&mut layouter)
                        .frame(egui::Frame::NONE)
                        .show(ui);
                    if let Some(range) = out.cursor_range {
                        self.cursor = range.primary.index.0;
                    }
                    changed_text |= out.response.changed();

                    // Passar o mouse sobre um trecho sublinhado mostra a mensagem do problema.
                    if out.response.hovered()
                        && !tab_problems.is_empty()
                        && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                    {
                        let idx = out.galley.cursor_from_pos(pos - out.galley_pos).index.0;
                        if let Some(message) = problems::message_at(&t.text, &tab_problems, idx) {
                            out.response.show_tooltip_text(message);
                        }
                    }

                    // Onde desenhar o popup: logo abaixo do cursor.
                    let caret = out.galley.pos_from_cursor(egui::text::CCursor::new(self.cursor));
                    let anchor = out.galley_pos + caret.left_bottom().to_vec2();

                    if changed_text {
                        t.dirty = true;
                        self.build_problems.remove(&lsp::uri_key(&uri));
                        if t.lsp_version > 0 {
                            t.lsp_version += 1;
                            if let Some(c) = self.ac.client(path_lang) {
                                let _ = c.did_change(&uri, t.lsp_version, &t.text);
                            }
                        }
                        // `.` abre as sugestões; digitar dentro de uma lista cortada pelo servidor pede de novo.
                        let before = t.text.chars().nth(self.cursor.saturating_sub(1));
                        if before == Some('.') {
                            trigger = true;
                        } else if let Some(p) = &self.popup {
                            if before.is_some_and(autocomplete::is_ident) {
                                trigger = p.incomplete;
                            } else {
                                self.popup = None;
                            }
                        }
                    }
                    // O cursor saiu da palavra (setas, clique): fecha.
                    if let Some(p) = &self.popup
                        && (self.cursor < p.origin || autocomplete::prefix_start(&t.text, self.cursor) < p.origin)
                    {
                        self.popup = None;
                    }

                    if trigger
                        && t.lsp_version > 0
                        && let Some(c) = self.ac.client(path_lang)
                    {
                        let (line, col) = lsp::position_of(&t.text, self.cursor);
                        self.pending = Some(Pending {
                            rx: c.completion(&uri, line, col),
                            tab: self.active,
                            origin: autocomplete::prefix_start(&t.text, self.cursor),
                            explicit,
                        });
                    } else if trigger {
                        self.log.push("Autocomplete indisponível: o servidor desta linguagem ainda não está pronto.".into());
                    }

                    // Lista de sugestões.
                    if let Some(p) = &mut self.popup {
                        let prefix = autocomplete::prefix_text(&t.text, autocomplete::prefix_start(&t.text, self.cursor), self.cursor);
                        let visible = autocomplete::matching(&p.items, &prefix);
                        if visible.is_empty() {
                            self.popup = None;
                        } else {
                            let selected = p.selected.min(visible.len() - 1);
                            let mut clicked = None;
                            egui::Area::new(egui::Id::unique("completion-popup"))
                                .order(egui::Order::Foreground)
                                .fixed_pos(anchor)
                                .show(&ctx, |ui| {
                                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                                        ui.set_max_width(560.0);
                                        for (i, it) in visible.iter().enumerate().skip(selected.saturating_sub(11)).take(12) {
                                            let text = format!("{} {}", it.kind_letter(), it.label);
                                            if ui.selectable_label(i == selected, RichText::new(text).monospace().small()).clicked() {
                                                clicked = Some(i);
                                            }
                                        }
                                    });
                                });
                            if let Some(n) = clicked {
                                let item = visible[n].clone();
                                let start = autocomplete::prefix_start(&t.text, self.cursor);
                                let (text, (from, to)) = autocomplete::apply(&t.text, self.cursor, start, &item);
                                t.text = text;
                                self.cursor = to;
                                self.set_cursor = Some((from, to));
                                self.popup = None;
                                t.dirty = true;
                                if t.lsp_version > 0 {
                                    t.lsp_version += 1;
                                    if let Some(c) = self.ac.client(path_lang) {
                                        let _ = c.did_change(&uri, t.lsp_version, &t.text);
                                    }
                                }
                            }
                        }
                    }
                });
            });
        }
    }

    /// Mantém os servidores de linguagem em dia com os documentos abertos: inicia o da linguagem do
    /// arquivo ativo e abre nele os documentos que ainda não foram.
    fn sync_language_servers(&mut self, ctx: &egui::Context) {
        if self.ac.set_project(&self.folder) {
            for t in &mut self.tabs {
                t.lsp_version = 0;
            }
            self.popup = None;
            self.pending = None;
        }
        if let Some(t) = self.tabs.get(self.active) {
            let (root, ctx) = (self.sdk_root.clone(), ctx.clone());
            self.ac.ensure(&root, highlight::lang_for(&t.path), move || ctx.request_repaint());
        }
        self.ac.poll();
        for t in &mut self.tabs {
            if t.lsp_version > 0 || t.text.len() > MAX_HIGHLIGHT_BYTES {
                continue;
            }
            let lang = highlight::lang_for(&t.path);
            let (Some(id), Some(c)) = (servers::language_id(lang), self.ac.client(lang)) else { continue };
            if c.did_open(&lsp::path_to_uri(&t.path), id, 1, &t.text).is_ok() {
                t.lsp_version = 1;
            }
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

fn source_name(lang: highlight::Lang) -> &'static str {
    match lang {
        highlight::Lang::Java => "Java",
        highlight::Lang::Kotlin => "Kotlin",
        _ => "Editor",
    }
}

/// Problemas de um arquivo aberto: o que o servidor de linguagem publicou mais o que o Gradle reclamou.
fn tab_problems(ac: &AutoComplete, build: &HashMap<String, (PathBuf, Vec<Problem>)>, t: &Tab, lang: highlight::Lang, uri: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    if t.lsp_version > 0
        && let Some(c) = ac.client(lang)
    {
        out.extend(c.diagnostics(uri).iter().map(|d| problems::from_lsp(d, source_name(lang))));
    }
    if let Some((_, list)) = build.get(&lsp::uri_key(uri)) {
        out.extend(list.iter().cloned());
    }
    out
}

/// As ocorrências da busca como fundo colorido: a selecionada mais forte, e primeiro, para valer sobre as outras.
fn find_marks(matches: &[(usize, usize)], selected: Option<(usize, usize)>) -> Vec<highlight::Mark> {
    let all = Color32::from_rgba_unmultiplied(0xea, 0xc5, 0x4f, 90);
    let current = Color32::from_rgba_unmultiplied(0xf2, 0x8b, 0x30, 190);
    let mut out: Vec<highlight::Mark> = matches.iter().filter(|m| Some(**m) == selected).map(|(a, b)| highlight::Mark::fill(*a, *b, current)).collect();
    out.extend(matches.iter().filter(|m| Some(**m) != selected).take(5000).map(|(a, b)| highlight::Mark::fill(*a, *b, all)));
    out
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices().nth(char_idx).map_or(text.len(), |(b, _)| b)
}

/// Um celular com o triângulo de "executar" dentro.
fn draw_simulator_icon(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let stroke = egui::Stroke::new(1.8, color);
    p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(16.0, 26.0)), 3.0, stroke, egui::StrokeKind::Middle);
    let tri = vec![c + egui::vec2(-3.0, -5.0), c + egui::vec2(-3.0, 5.0), c + egui::vec2(4.0, 0.0)];
    p.add(egui::Shape::convex_polygon(tri, color, egui::Stroke::NONE));
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
        View::Search => {
            p.circle_stroke(c + egui::vec2(-2.0, -2.0), 7.0, stroke);
            p.line_segment([c + egui::vec2(3.0, 3.0), c + egui::vec2(10.0, 10.0)], stroke);
        }
        View::Devices => {
            let phone = egui::Rect::from_center_size(c, egui::vec2(14.0, 24.0));
            p.rect_stroke(phone, 3.0, stroke, egui::StrokeKind::Middle);
            p.line_segment([c + egui::vec2(-3.0, 8.0), c + egui::vec2(3.0, 8.0)], stroke);
        }
    }
}

fn tree(ui: &mut egui::Ui, dir: &Path, to_open: &mut Option<PathBuf>, selected: &Path, on_select: &mut Option<PathBuf>, on_new: &mut Option<(PathBuf, bool)>) {
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
            let label = if path == selected { RichText::new(name).color(ACCENT) } else { RichText::new(name) };
            let r = egui::CollapsingHeader::new(label).id_salt(&path).show(ui, |ui| tree(ui, &path, to_open, selected, on_select, on_new));
            if r.header_response.clicked() {
                *on_select = Some(path.clone());
            }
            r.header_response.context_menu(|ui| {
                if ui.button("Novo arquivo aqui").clicked() {
                    *on_new = Some((path.clone(), false));
                    ui.close();
                }
                if ui.button("Nova pasta aqui").clicked() {
                    *on_new = Some((path.clone(), true));
                    ui.close();
                }
            });
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
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Desliga os servidores de linguagem e o logcat com educação; o `Drop` mata o que sobrar.
        self.ac.shutdown();
        self.logcat.disconnect();
        self.sim.stop();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.poll(ctx);
        self.sync_language_servers(ctx);
        self.handle_shortcuts(ctx);
        self.poll_search();
        let problems = self.all_problems();
        self.logcat.poll();
        self.terminal.poll();
        if self.folder != self.term_folder {
            self.term_folder = self.folder.clone();
            self.terminal.set_cwd(self.folder.clone());
        }
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
                    if let Some(t) = self.tabs.get(self.active)
                        && let Some(status) = self.ac.status(&self.sdk_root, highlight::lang_for(&t.path))
                    {
                        ui.label(RichText::new(status).color(Color32::WHITE).small());
                    }
                    let (errors, warnings) = problems::counts(problems.iter().map(|(_, p)| p));
                    if errors + warnings > 0 {
                        ui.label(RichText::new(format!("{errors} erro(s), {warnings} aviso(s)")).color(Color32::WHITE).small());
                    }
                });
            });

        egui::Panel::bottom("output")
            .resizable(true)
            .default_size(150.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(8.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (errors, warnings) = problems::counts(problems.iter().map(|(_, p)| p));
                    let problems_label = if errors + warnings > 0 { format!("PROBLEMAS ({errors}/{warnings})") } else { "PROBLEMAS".to_owned() };
                    for (tab, label) in [
                        (Bottom::Output, "SAÍDA".to_owned()),
                        (Bottom::Problems, problems_label),
                        (Bottom::Logcat, "LOGCAT".to_owned()),
                        (Bottom::Terminal, "TERMINAL".to_owned()),
                    ] {
                        if ui.selectable_label(self.bottom == tab, RichText::new(label).small()).clicked() {
                            self.bottom = tab;
                        }
                    }
                });
                match self.bottom {
                    Bottom::Output => self.output_view(ui),
                    Bottom::Logcat => {
                        let port = self.running.first().map(|(_, p, _)| *p);
                        self.logcat.ui(ui, &self.sdk_root, port);
                    }
                    Bottom::Problems => self.problems_view(ui, &problems),
                    Bottom::Terminal => {
                        let ctx = ui.ctx().clone();
                        self.terminal.ui(ui, &self.sdk_root, &ctx);
                    }
                }
            });

        self.activity_bar(ui);
        self.sidebar(ui);
        if self.show_sim {
            egui::Panel::right("simulator")
                .default_size(360.0)
                .size_range(260.0..=720.0)
                .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(8.0))
                .show(ui, |ui| {
                    let port = self.running.first().map(|(_, p, _)| *p);
                    self.sim.ui(ui, &self.sdk_root, port);
                });
        } else {
            self.sim.stop();
        }
        egui::CentralPanel::default().show(ui, |ui| self.editor(ui));
        self.license_window(ctx);
        self.new_project_window(ctx);

        if self.logcat.connected() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if self.job.is_some() || self.build_rx.is_some() || self.run_rx.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "find_tests.rs"]
mod find_tests;
