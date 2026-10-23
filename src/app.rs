//! Interface no estilo VS Code: barra de atividades, barra lateral, abas de editor,
//! painel inferior e barra de status.

use crate::avd;
use crate::catalog::{self, Catalog, Package, Profile};
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

#[derive(PartialEq, Clone, Copy)]
enum View {
    Explorer,
    Devices,
}

struct Tab {
    path: PathBuf,
    text: String,
    dirty: bool,
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
        Self {
            view: View::Devices,
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
            log: vec!["Lendo o catálogo oficial do Google…".into()],
        }
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
            self.selected_image = cat.images().first().map(|p| p.path.clone());
            self.catalog = cat;
            self.catalog_rx = None;
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
        if pkgs.iter().any(|p| !self.accepted.contains(&p.license)) {
            self.pending_license = Some(pkgs);
        } else {
            self.queue.extend(pkgs);
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
                self.tabs.push(Tab { path: path.to_owned(), text, dirty: false });
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
        let smallest = self.catalog.images().first().map(|p| p.path.clone());
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
                    if smallest.as_deref() == Some(p.path.as_str()) {
                        ui.label(RichText::new("menor").small().color(ACCENT));
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
            match avd::launch(&self.sdk_root, &name, &avd::Launch { headless: false, port }) {
                Ok(child) => {
                    self.log.push(format!("Iniciando {name} (console {port})."));
                    self.running.push((name, port, child));
                }
                Err(e) => self.log.push(format!("Não iniciei {name}: {e}")),
            }
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

    fn license_window(&mut self, ctx: &egui::Context) {
        let Some(pkgs) = &self.pending_license else { return };
        let mut ids: Vec<String> = pkgs
            .iter()
            .map(|p| p.license.clone())
            .filter(|l| !self.accepted.contains(l))
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
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let r = ui.add_sized(
                    ui.available_size(),
                    egui::TextEdit::multiline(&mut t.text).code_editor().lock_focus(true).frame(egui::Frame::NONE),
                );
                if r.changed() {
                    t.dirty = true;
                }
            });
        }
    }
}

fn flavor(p: &Package) -> &str {
    match p.path.split(';').nth(2) {
        Some("default") => "AOSP",
        Some("google_apis") => "Google APIs",
        Some("google_apis_playstore") => "Google Play",
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
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                    for l in &self.log {
                        ui.label(RichText::new(l).monospace().small());
                    }
                });
            });

        self.activity_bar(ui);
        self.sidebar(ui);
        egui::CentralPanel::default().show(ui, |ui| self.editor(ui));
        self.license_window(ctx);

        if self.job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}
