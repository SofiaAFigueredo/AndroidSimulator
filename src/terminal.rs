//! Terminal integrado, sem emulação de PTY: cada linha digitada roda no shell do sistema, na pasta
//! atual, com o ambiente do app já pronto (adb, Gradle e JDK no PATH, JAVA_HOME e ANDROID_HOME).
//! Cobre comandos que imprimem e terminam; programas interativos (vim, REPLs) não funcionam.

use eframe::egui::{self, Color32, RichText};
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_LINES: usize = 10_000;

/// Remove sequências de escape ANSI (cores e movimento de cursor), que o terminal não interpreta.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: ESC [ parâmetros... letra final
            Some('[') => {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... BEL ou ESC \
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Resolve `.` e `..` sem tocar no disco (`canonicalize` no Windows devolve caminhos `\\?\`).
pub fn clean_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Destino de um `cd`; `None` se a pasta não existe.
pub fn resolve_cd(cwd: &Path, arg: &str) -> Option<PathBuf> {
    let arg = arg.trim().trim_matches('"');
    let target = if arg.is_empty() { dirs::home_dir()? } else { cwd.join(arg) };
    let target = clean_path(&target);
    target.is_dir().then_some(target)
}

/// Variáveis de ambiente que fazem `adb`, `gradle` e `java` funcionarem direto, sem tocar no sistema.
pub fn tool_env(root: &Path) -> Vec<(String, std::ffi::OsString)> {
    let jdk = crate::project::jdk_home(root);
    let mut dirs_in_path: Vec<PathBuf> = [jdk.join("bin"), root.join("platform-tools"), root.join("emulator")]
        .into_iter()
        .chain(crate::project::gradle_bin(root).parent().map(Path::to_path_buf))
        .filter(|d| d.is_dir())
        .collect();
    dirs_in_path.extend(std::env::var_os("PATH").iter().flat_map(std::env::split_paths));
    let mut env = vec![
        ("ANDROID_HOME".to_owned(), root.as_os_str().to_owned()),
        ("ANDROID_SDK_ROOT".to_owned(), root.as_os_str().to_owned()),
        ("ANDROID_ADB_SERVER_PORT".to_owned(), crate::avd::ADB_PORT.to_string().into()),
        ("GRADLE_USER_HOME".to_owned(), root.join("gradle-home").into_os_string()),
    ];
    if jdk.is_dir() {
        env.push(("JAVA_HOME".to_owned(), jdk.into_os_string()));
    }
    if let Ok(path) = std::env::join_paths(dirs_in_path) {
        env.push(("PATH".to_owned(), path));
    }
    env
}

enum Msg {
    Line(String),
    Exit(Option<i32>),
}

pub struct Terminal {
    lines: Vec<String>,
    input: String,
    cwd: PathBuf,
    rx: Option<Receiver<Msg>>,
    child: Option<Arc<Mutex<Child>>>,
    history: Vec<String>,
    /// Posição na navegação com ↑/↓; `None` quando se está digitando uma linha nova.
    hist_pos: Option<usize>,
    focus_input: bool,
}

impl Terminal {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            lines: vec!["Terminal do app: adb, gradle e java já estão no PATH. Sem programas interativos.".into()],
            input: String::new(),
            cwd,
            rx: None,
            child: None,
            history: Vec::new(),
            hist_pos: None,
            focus_input: true,
        }
    }

    pub fn set_cwd(&mut self, cwd: PathBuf) {
        if cwd != self.cwd && self.rx.is_none() {
            self.cwd = cwd;
        }
    }

    pub fn running(&self) -> bool {
        self.rx.is_some()
    }

    fn push(&mut self, line: String) {
        if self.lines.len() >= MAX_LINES {
            self.lines.drain(..MAX_LINES / 10);
        }
        self.lines.push(line);
    }

    /// Executa a linha digitada. `repaint` é chamado quando chega saída nova.
    pub fn submit(&mut self, root: &Path, line: &str, repaint: impl Fn() + Send + Sync + 'static) {
        let line = line.trim();
        if line.is_empty() || self.running() {
            return;
        }
        self.history.push(line.to_owned());
        self.hist_pos = None;
        self.push(format!("{}> {line}", self.cwd.display()));
        let (cmd, arg) = line.split_once(char::is_whitespace).map_or((line, ""), |(c, a)| (c, a.trim()));
        match cmd.to_ascii_lowercase().as_str() {
            "clear" | "cls" => {
                self.lines.clear();
                return;
            }
            "cd" | "chdir" => {
                match resolve_cd(&self.cwd, arg) {
                    Some(dir) => self.cwd = dir,
                    None => self.push(format!("cd: pasta não encontrada: {arg}")),
                }
                return;
            }
            _ => {}
        }
        match spawn_shell(root, &self.cwd, line) {
            Ok(mut child) => {
                let (tx, rx) = mpsc::channel();
                let repaint = Arc::new(repaint);
                let readers: Vec<_> = [
                    child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
                    child.stderr.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
                ]
                .into_iter()
                .flatten()
                .map(|stream| {
                    let (tx, repaint) = (tx.clone(), Arc::clone(&repaint));
                    std::thread::spawn(move || {
                        for l in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                            let l = strip_ansi(String::from_utf8_lossy(&l).trim_end_matches('\r'));
                            if tx.send(Msg::Line(l)).is_err() {
                                break;
                            }
                            repaint();
                        }
                    })
                })
                .collect();
                let child = Arc::new(Mutex::new(child));
                self.child = Some(Arc::clone(&child));
                let tx_exit = tx;
                std::thread::spawn(move || {
                    for r in readers {
                        let _ = r.join();
                    }
                    // Não segura o lock enquanto espera, para o botão Parar conseguir matar o processo.
                    let code = loop {
                        match child.lock().map(|mut c| c.try_wait()) {
                            Ok(Ok(Some(status))) => break status.code(),
                            Ok(Ok(None)) => {}
                            _ => break None,
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    };
                    let _ = tx_exit.send(Msg::Exit(code));
                    repaint();
                });
                self.rx = Some(rx);
            }
            Err(e) => self.push(format!("não consegui executar: {e}")),
        }
    }

    pub fn stop(&mut self) {
        if let Some(child) = &self.child
            && let Ok(mut c) = child.lock()
        {
            // No Windows o `cmd` é só o invólucro: matar só ele deixa o programa filho rodando e
            // segurando a saída. `taskkill /T` encerra a árvore inteira.
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &c.id().to_string()])
                    .creation_flags(0x0800_0000)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = c.kill();
        }
    }

    /// Recolhe a saída que chegou. Chame uma vez por quadro.
    pub fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        let mut new_lines = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(Msg::Line(l)) => new_lines.push(l),
                Ok(Msg::Exit(code)) => {
                    done = Some(code);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = Some(None);
                    break;
                }
            }
        }
        for l in new_lines {
            self.push(l);
        }
        if let Some(code) = done {
            // As últimas linhas podem ter chegado junto com o aviso de saída.
            if let Some(rx) = &self.rx {
                let rest: Vec<String> = rx.try_iter().filter_map(|m| if let Msg::Line(l) = m { Some(l) } else { None }).collect();
                for l in rest {
                    self.push(l);
                }
            }
            if code != Some(0) {
                self.push(format!("(saiu com código {})", code.map_or("desconhecido".to_owned(), |c| c.to_string())));
            }
            self.rx = None;
            self.child = None;
            self.focus_input = true;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, root: &Path, ctx: &egui::Context) {
        self.poll();
        let input_height = 28.0;
        let row = ui.text_style_height(&egui::TextStyle::Small) + 2.0;
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink(false)
            .max_height((ui.available_height() - input_height).max(40.0))
            .show_rows(ui, row, self.lines.len(), |ui, range| {
                for l in &self.lines[range] {
                    ui.label(RichText::new(l).monospace().small());
                }
            });
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} >", self.cwd.display())).monospace().small().color(Color32::from_rgb(0x4e, 0xc9, 0xb0)));
            let r = ui.add_enabled(
                !self.running(),
                egui::TextEdit::singleline(&mut self.input).font(egui::TextStyle::Monospace).desired_width(ui.available_width() - 60.0),
            );
            if self.focus_input && !self.running() {
                r.request_focus();
                self.focus_input = false;
            }
            if r.has_focus() {
                let (up, down) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown)));
                if up && !self.history.is_empty() {
                    let pos = self.hist_pos.map_or(self.history.len() - 1, |p| p.saturating_sub(1));
                    self.hist_pos = Some(pos);
                    self.input = self.history[pos].clone();
                } else if down && let Some(p) = self.hist_pos {
                    if p + 1 < self.history.len() {
                        self.hist_pos = Some(p + 1);
                        self.input = self.history[p + 1].clone();
                    } else {
                        self.hist_pos = None;
                        self.input.clear();
                    }
                }
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let line = std::mem::take(&mut self.input);
                let ctx = ctx.clone();
                self.submit(root, &line, move || ctx.request_repaint());
                self.focus_input = true;
            }
            if self.running() && ui.button("Parar").clicked() {
                self.stop();
            }
        });
    }
}

fn spawn_shell(root: &Path, cwd: &Path, line: &str) -> std::io::Result<Child> {
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        // `raw_arg` evita que o Rust reescreva as aspas; `chcp 65001` faz a saída sair em UTF-8.
        c.arg("/C").raw_arg(format!("chcp 65001>nul & {line}"));
        c.creation_flags(0x0800_0000);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", line]);
        c
    };
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in tool_env(root) {
        cmd.env(k, v);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn run_to_end(t: &mut Terminal, root: &Path, line: &str) {
        t.submit(root, line, || {});
        let deadline = Instant::now() + Duration::from_secs(30);
        while t.running() {
            assert!(Instant::now() < deadline, "o comando não terminou");
            t.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("andsim-term-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn strips_colors_cursor_moves_and_titles() {
        assert_eq!(strip_ansi("\u{1b}[31mErro\u{1b}[0m: falhou"), "Erro: falhou");
        assert_eq!(strip_ansi("a\u{1b}[2K\u{1b}[1Gb"), "ab");
        assert_eq!(strip_ansi("\u{1b}]0;título\u{7}texto"), "texto");
        assert_eq!(strip_ansi("sem escapes ção"), "sem escapes ção");
    }

    #[test]
    fn cleans_dot_segments_without_touching_the_disk() {
        assert_eq!(clean_path(Path::new("a/b/../c/./d")), PathBuf::from("a/c/d"));
        assert_eq!(clean_path(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn cd_resolves_relative_parent_and_rejects_missing_dirs() {
        let base = tmp("cd");
        std::fs::create_dir_all(base.join("sub/dentro")).unwrap();
        assert_eq!(resolve_cd(&base, "sub"), Some(base.join("sub")));
        assert_eq!(resolve_cd(&base.join("sub"), ".."), Some(base.clone()));
        assert_eq!(resolve_cd(&base, "\"sub/dentro\""), Some(base.join("sub/dentro")));
        assert_eq!(resolve_cd(&base, "nao-existe"), None);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn tool_env_puts_the_sdk_tools_first() {
        let root = tmp("env");
        std::fs::create_dir_all(root.join("platform-tools")).unwrap();
        let env = tool_env(&root);
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let path = get("PATH").unwrap();
        assert_eq!(std::env::split_paths(&path).next().unwrap(), root.join("platform-tools"));
        assert_eq!(get("ANDROID_HOME").unwrap(), root.as_os_str());
        assert!(get("JAVA_HOME").is_none(), "sem JDK instalado, JAVA_HOME não deve apontar para o vazio");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runs_a_command_in_the_current_dir_and_captures_output() {
        let root = tmp("run-root");
        let dir = tmp("run-cwd");
        let mut t = Terminal::new(dir.clone());
        run_to_end(&mut t, &root, if cfg!(windows) { "cd" } else { "pwd" });
        let shown = t.lines.last().unwrap().clone();
        assert!(shown.contains(dir.file_name().unwrap().to_str().unwrap()), "{shown}");
        run_to_end(&mut t, &root, "echo ola mundo");
        assert!(t.lines.iter().any(|l| l.trim() == "ola mundo"), "{:?}", t.lines);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_a_failing_exit_code_and_stderr() {
        let root = tmp("fail-root");
        let mut t = Terminal::new(tmp("fail-cwd"));
        run_to_end(&mut t, &root, if cfg!(windows) { "echo falha 1>&2 & exit /b 3" } else { "echo falha 1>&2; exit 3" });
        assert!(t.lines.iter().any(|l| l.trim() == "falha"), "{:?}", t.lines);
        assert!(t.lines.last().unwrap().contains("código 3"), "{:?}", t.lines);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn builtins_cd_and_clear_do_not_spawn_a_process() {
        let root = tmp("bi-root");
        let base = tmp("bi");
        std::fs::create_dir_all(base.join("filho")).unwrap();
        let mut t = Terminal::new(base.clone());
        t.submit(&root, "cd filho", || {});
        assert!(!t.running());
        assert_eq!(t.cwd, base.join("filho"));
        t.submit(&root, "cd nao-existe", || {});
        assert!(t.lines.last().unwrap().contains("pasta não encontrada"));
        t.submit(&root, "clear", || {});
        assert!(t.lines.is_empty());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn stop_kills_a_long_running_command() {
        let root = tmp("stop-root");
        let mut t = Terminal::new(tmp("stop-cwd"));
        t.submit(&root, if cfg!(windows) { "ping -n 60 127.0.0.1" } else { "sleep 60" }, || {});
        assert!(t.running());
        std::thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        t.stop();
        while t.running() {
            assert!(start.elapsed() < Duration::from_secs(10), "o processo não morreu");
            t.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Confere que `adb`, `java` e `gradle` respondem no terminal, com o SDK realmente instalado.
    /// `cargo test terminal_finds_the_real_sdk_tools -- --ignored --nocapture`
    #[test]
    #[ignore = "usa o SDK instalado (JDK, Gradle e platform-tools)"]
    fn terminal_finds_the_real_sdk_tools() {
        let root = crate::sdk::default_root();
        let mut t = Terminal::new(tmp("real-cwd"));
        for (cmd, expected) in [("adb version", "Android Debug Bridge"), ("java -version", "openjdk"), ("gradle --version", "Gradle 9.8.2")] {
            run_to_end(&mut t, &root, cmd);
            let out = t.lines.join("
");
            assert!(out.to_lowercase().contains(&expected.to_lowercase()), "{cmd}: esperava {expected:?} em
{out}");
            println!("OK {cmd} -> {expected}");
        }
        run_to_end(&mut t, &root, "echo %JAVA_HOME%");
        println!("{}", t.lines.last().unwrap());
    }
}
