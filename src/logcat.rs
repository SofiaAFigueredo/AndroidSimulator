//! Logcat: lê `adb logcat -v threadtime` do emulador, interpreta as linhas e filtra por nível e texto.

use crate::avd;
use eframe::egui::{self, Color32, RichText};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Quantas linhas ficam na memória; as mais antigas saem primeiro.
const CAPACITY: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Level; 6] = [Level::Verbose, Level::Debug, Level::Info, Level::Warn, Level::Error, Level::Fatal];

    fn from_letter(c: &str) -> Option<Level> {
        Some(match c {
            "V" => Level::Verbose,
            "D" => Level::Debug,
            "I" => Level::Info,
            "W" => Level::Warn,
            "E" => Level::Error,
            "F" | "A" => Level::Fatal,
            _ => return None,
        })
    }

    pub fn letter(self) -> char {
        match self {
            Level::Verbose => 'V',
            Level::Debug => 'D',
            Level::Info => 'I',
            Level::Warn => 'W',
            Level::Error => 'E',
            Level::Fatal => 'F',
        }
    }

    fn color(self) -> Color32 {
        match self {
            Level::Verbose => Color32::from_rgb(0x85, 0x85, 0x85),
            Level::Debug => Color32::from_rgb(0x9c, 0xdc, 0xfe),
            Level::Info => Color32::from_rgb(0xcc, 0xcc, 0xcc),
            Level::Warn => Color32::from_rgb(0xcc, 0xa7, 0x00),
            Level::Error | Level::Fatal => Color32::from_rgb(0xf4, 0x87, 0x71),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub time: String,
    pub pid: u32,
    pub level: Level,
    pub tag: String,
    pub msg: String,
    /// Minúsculas, para o filtro de texto não refazer isso a cada quadro.
    lower: String,
}

fn token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches(' ');
    let end = s.find(' ')?;
    Some((&s[..end], &s[end..]))
}

/// Interpreta uma linha no formato `threadtime`:
/// `10-23 14:02:11.123  1234  1250 I ActivityManager: mensagem`.
pub fn parse(line: &str) -> Option<Entry> {
    let (_date, rest) = token(line)?;
    let (time, rest) = token(rest)?;
    let (pid, rest) = token(rest)?;
    let (_tid, rest) = token(rest)?;
    let (letter, rest) = token(rest)?;
    let level = Level::from_letter(letter)?;
    let pid = pid.parse().ok()?;
    let rest = rest.trim_start_matches(' ');
    let (tag, msg) = rest.split_once(": ").or_else(|| rest.strip_suffix(':').map(|t| (t, "")))?;
    let tag = tag.trim().to_owned();
    let msg = msg.trim_end().to_owned();
    Some(Entry { time: time.to_owned(), pid, level, lower: format!("{tag} {msg}").to_lowercase(), tag, msg })
}

/// Linhas que não seguem o formato (`--------- beginning of main`) viram entradas informativas.
fn plain(line: &str) -> Entry {
    Entry { time: String::new(), pid: 0, level: Level::Info, tag: String::new(), msg: line.to_owned(), lower: line.to_lowercase() }
}

#[derive(Clone)]
pub struct Filter {
    pub text: String,
    pub min_level: Level,
}

impl Filter {
    pub fn matches(&self, e: &Entry) -> bool {
        e.level >= self.min_level && (self.text.is_empty() || e.lower.contains(&self.text.to_lowercase()))
    }
}

/// O processo `adb logcat` e o canal de linhas dele.
struct Stream {
    child: Child,
    rx: Receiver<String>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Stream {
    fn start(root: &Path, port: u16) -> Result<Self, String> {
        let mut cmd = avd::adb_command(root);
        cmd.args(["-s", &avd::serial(port), "logcat", "-v", "threadtime"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", avd::adb_path(root).display()))?;
        let out = child.stdout.take().ok_or("sem saída do adb")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                let line = String::from_utf8_lossy(&line).trim_end_matches('\r').to_owned();
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, rx })
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct LogcatView {
    stream: Option<Stream>,
    entries: VecDeque<Entry>,
    filter: Filter,
    /// Índices (em `entries`) das linhas que passam no filtro; refeito só quando algo muda.
    view: Vec<usize>,
    dirty: bool,
    follow: bool,
    status: String,
}

impl Default for LogcatView {
    fn default() -> Self {
        Self {
            stream: None,
            entries: VecDeque::new(),
            filter: Filter { text: String::new(), min_level: Level::Verbose },
            view: Vec::new(),
            dirty: true,
            follow: true,
            status: "Desconectado".into(),
        }
    }
}

impl LogcatView {
    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn connect(&mut self, root: &Path, port: u16) {
        self.disconnect();
        match Stream::start(root, port) {
            Ok(s) => {
                self.stream = Some(s);
                self.status = format!("Conectado a {}", avd::serial(port));
            }
            Err(e) => self.status = format!("Erro: {e}"),
        }
    }

    pub fn disconnect(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.stop();
            self.status = "Desconectado".into();
        }
    }

    /// Passa as linhas novas do processo para a lista. Chame uma vez por quadro.
    pub fn poll(&mut self) {
        let Some(stream) = &self.stream else { return };
        let mut ended = false;
        loop {
            match stream.rx.try_recv() {
                Ok(line) => {
                    if line.is_empty() {
                        continue;
                    }
                    if self.entries.len() >= CAPACITY {
                        self.entries.pop_front();
                    }
                    self.entries.push_back(parse(&line).unwrap_or_else(|| plain(&line)));
                    self.dirty = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
            }
        }
        if ended {
            self.disconnect();
            self.status = "O adb encerrou (o emulador ainda está iniciando ou foi fechado?)".into();
        }
    }

    pub fn clear(&mut self, root: &Path, port: Option<u16>) {
        self.entries.clear();
        self.dirty = true;
        if let Some(p) = port {
            let _ = avd::adb(root, &["-s", &avd::serial(p), "logcat", "-c"]);
        }
    }

    fn refresh_view(&mut self) {
        self.view = self.entries.iter().enumerate().filter(|(_, e)| self.filter.matches(e)).map(|(i, _)| i).collect();
        self.dirty = false;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, root: &Path, port: Option<u16>) {
        ui.horizontal(|ui| {
            if self.connected() {
                if ui.button("Desconectar").clicked() {
                    self.disconnect();
                }
            } else if ui.add_enabled(port.is_some(), egui::Button::new("Conectar")).on_disabled_hover_text("Inicie um emulador na aba Dispositivos").clicked()
                && let Some(p) = port
            {
                self.connect(root, p);
            }
            if ui.button("Limpar").clicked() {
                self.clear(root, port);
            }
            let before = (self.filter.text.clone(), self.filter.min_level);
            egui::ComboBox::from_id_salt("logcat-level")
                .selected_text(format!("{}+", self.filter.min_level.letter()))
                .width(48.0)
                .show_ui(ui, |ui| {
                    for l in Level::ALL {
                        ui.selectable_value(&mut self.filter.min_level, l, format!("{} ({:?})", l.letter(), l));
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut self.filter.text).hint_text("Filtrar (tag, pacote, texto)").desired_width(220.0));
            if before != (self.filter.text.clone(), self.filter.min_level) {
                self.dirty = true;
            }
            ui.checkbox(&mut self.follow, "Seguir");
            ui.label(RichText::new(format!("{} · {} linhas", self.status, self.view.len())).small().color(Color32::from_rgb(0x85, 0x85, 0x85)));
        });
        if self.dirty {
            self.refresh_view();
        }
        let row = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        egui::ScrollArea::vertical().stick_to_bottom(self.follow).auto_shrink(false).show_rows(ui, row, self.view.len(), |ui, range| {
            for &i in &self.view[range] {
                let e = &self.entries[i];
                let text = if e.time.is_empty() {
                    e.msg.clone()
                } else {
                    format!("{} {:>5} {} {}: {}", e.time, e.pid, e.level.letter(), e.tag, e.msg)
                };
                ui.label(RichText::new(text).monospace().small().color(e.level.color()));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_threadtime_line() {
        let e = parse("10-23 14:02:11.123  1234  1250 I ActivityManager: Start proc com.exemplo.teste for activity").unwrap();
        assert_eq!((e.time.as_str(), e.pid, e.level), ("14:02:11.123", 1234, Level::Info));
        assert_eq!(e.tag, "ActivityManager");
        assert_eq!(e.msg, "Start proc com.exemplo.teste for activity");
    }

    #[test]
    fn tag_padding_and_colons_in_the_message() {
        let e = parse("10-23 14:02:11.123   999   999 E AndroidRuntime : FATAL EXCEPTION: main").unwrap();
        assert_eq!((e.level, e.tag.as_str(), e.msg.as_str()), (Level::Error, "AndroidRuntime", "FATAL EXCEPTION: main"));
        let empty = parse("10-23 14:02:11.123  1  2 D Tag:").unwrap();
        assert_eq!((empty.tag.as_str(), empty.msg.as_str()), ("Tag", ""));
    }

    #[test]
    fn trailing_spaces_are_dropped_from_the_message() {
        let e = parse("10-23 14:02:11.123  1  2 W Tag: mensagem com espaço no fim   ").unwrap();
        assert_eq!(e.msg, "mensagem com espaço no fim");
    }

    #[test]
    fn rejects_lines_that_are_not_log_entries() {
        for l in ["--------- beginning of main", "", "garbage line here", "10-23 14:02:11.123  x  2 D Tag: m", "10-23 14:02:11.123  1  2 Z Tag: m"] {
            assert_eq!(parse(l), None, "{l:?}");
        }
    }

    #[test]
    fn filter_by_level_and_text_ignoring_case() {
        let warn = parse("10-23 14:02:11.123  1  2 W Network: Timeout em com.Exemplo.app").unwrap();
        let info = parse("10-23 14:02:11.123  1  2 I Other: nada").unwrap();
        let f = |text: &str, lvl| Filter { text: text.into(), min_level: lvl };
        assert!(f("", Level::Verbose).matches(&warn) && f("", Level::Verbose).matches(&info));
        assert!(f("", Level::Warn).matches(&warn) && !f("", Level::Warn).matches(&info));
        assert!(f("EXEMPLO", Level::Verbose).matches(&warn) && !f("EXEMPLO", Level::Verbose).matches(&info));
        assert!(f("network", Level::Verbose).matches(&warn), "o filtro também olha a tag");
    }

    #[test]
    fn unparsable_lines_are_kept_as_plain_info() {
        let e = plain("--------- beginning of main");
        assert_eq!((e.level, e.msg.as_str()), (Level::Info, "--------- beginning of main"));
    }

    #[test]
    fn capacity_drops_the_oldest_lines() {
        let (tx, rx) = mpsc::channel();
        let child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" })
            .args(if cfg!(windows) { vec!["/C", "ping -n 30 127.0.0.1 >nul"] } else { vec!["30"] })
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut v = LogcatView { stream: Some(Stream { child, rx }), ..Default::default() };
        for i in 0..CAPACITY + 5 {
            tx.send(format!("10-23 14:02:11.123  1  2 I T: linha {i}")).unwrap();
        }
        v.poll();
        assert_eq!(v.entries.len(), CAPACITY);
        assert_eq!(v.entries.front().unwrap().msg, "linha 5");
        v.disconnect();
    }

    /// Sobe o emulador e confere que o logcat recebe, interpreta e filtra uma linha que o teste mesmo
    /// escreve no log do Android. Exige a imagem `ANDSIM_IMAGE` instalada (padrão: API 24 default x86).
    /// `cargo test streams_logcat_from_the_emulator -- --ignored --nocapture`
    #[test]
    #[ignore = "sobe o emulador"]
    fn streams_logcat_from_the_emulator() {
        let root = crate::sdk::default_root();
        let (cat, _) = crate::catalog::fetch_all();
        let image = std::env::var("ANDSIM_IMAGE").unwrap_or_else(|_| "system-images;android-24;default;x86".into());
        let img = cat.packages.iter().find(|p| p.path == image).expect("imagem fora do catálogo");
        assert!(crate::sdk::installed(&root).contains_key(&image), "{image} não instalada");
        let name = format!("probe_log_{}", avd::default_name(img));
        avd::create(&root, &name, img).unwrap();
        let port = 5586;
        let mut child = avd::launch(&root, &name, &avd::Launch::new(true, port)).unwrap();
        let result = (|| -> Result<(usize, Entry), String> {
            avd::wait_boot(&root, &mut child, port, std::time::Duration::from_secs(300))?;
            let mut v = LogcatView::default();
            v.connect(&root, port);
            assert!(v.connected(), "{}", v.status);
            std::thread::sleep(std::time::Duration::from_secs(2));
            avd::adb(&root, &["-s", &avd::serial(port), "shell", "log", "-p", "w", "-t", "AndsimProbe", "olá do teste"])?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let found = loop {
                v.poll();
                if let Some(e) = v.entries.iter().find(|e| e.tag == "AndsimProbe") {
                    break e.clone();
                }
                if std::time::Instant::now() > deadline {
                    return Err(format!("não recebi a linha; {} linhas lidas ({})", v.entries.len(), v.status));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            };
            let parsed = v.entries.iter().filter(|e| !e.time.is_empty()).count();
            let f = Filter { text: "andsimprobe".into(), min_level: Level::Warn };
            assert!(f.matches(&found));
            assert_eq!(v.entries.iter().filter(|e| f.matches(e)).count(), 1);
            v.disconnect();
            Ok((parsed, found))
        })();
        avd::stop(&root, &mut child, port);
        let _ = avd::delete(&root, &name);
        let (parsed, found) = result.expect("logcat");
        println!("RESULTADO: {parsed} linhas interpretadas; achei {found:?}");
        assert_eq!((found.level, found.msg.as_str()), (Level::Warn, "olá do teste"));
    }
}
