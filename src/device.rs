//! O dispositivo simulado: a tela ao vivo (quadros do `screencap` cru, num único processo que transmite
//! sem parar) e a entrada (toque e arrastar gravados direto no dispositivo de entrada do emulador,
//! teclas e texto pelo `input`). Tudo isolado do `adb` do usuário, como o resto do app.

use crate::avd;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// Quadros.

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// RGBA, 4 bytes por pixel, linha a linha.
    pub rgba: Vec<u8>,
}

/// Bytes de cabeçalho do `screencap` cru: 12 (largura, altura, formato) até o Android 8 e 16 (mais o
/// espaço de cor) do 9 em diante. Deduz pelo tamanho total de um quadro inteiro.
pub fn header_len(total: usize, width: usize, height: usize) -> Option<usize> {
    match total.checked_sub(width.checked_mul(height)?.checked_mul(4)?)? {
        n @ (12 | 16) => Some(n),
        _ => None,
    }
}

fn u32_at(b: &[u8], i: usize) -> usize {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
}

/// Lê o próximo quadro; `Ok(None)` quando o fluxo acabou antes de um cabeçalho.
pub fn read_frame<R: Read>(r: &mut R, header: usize) -> std::io::Result<Option<Frame>> {
    let mut head = vec![0u8; header];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let (width, height, format) = (u32_at(&head, 0), u32_at(&head, 4), u32_at(&head, 8));
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("quadro com tamanho absurdo: {width}x{height}")));
    }
    let mut rgba = vec![0u8; width * height * 4];
    r.read_exact(&mut rgba)?;
    match format {
        1 => {}
        2 => rgba.chunks_exact_mut(4).for_each(|p| p[3] = 255),
        5 => rgba.chunks_exact_mut(4).for_each(|p| {
            p.swap(0, 2);
        }),
        other => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("formato de pixel {other} não suportado"))),
    }
    Ok(Some(Frame { width, height, rgba }))
}

/// A tela ao vivo: uma thread lê os quadros e guarda só o mais recente.
pub struct Screen {
    latest: Arc<Mutex<Option<(u64, Arc<Frame>)>>>,
    child: Arc<Mutex<Child>>,
    alive: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
}

impl Screen {
    pub fn start(root: &Path, port: u16) -> Result<Screen, String> {
        let serial = avd::serial(port);
        // Um quadro avulso para descobrir o tamanho do cabeçalho.
        let probe = avd::adb_command(root).args(["-s", &serial, "exec-out", "screencap"]).output().map_err(|e| e.to_string())?;
        if !probe.status.success() || probe.stdout.len() < 12 {
            return Err(format!("o screencap falhou: {}", String::from_utf8_lossy(&probe.stderr).trim()));
        }
        let (w, h) = (u32_at(&probe.stdout, 0), u32_at(&probe.stdout, 4));
        let header = header_len(probe.stdout.len(), w, h).ok_or_else(|| format!("quadro de {} bytes não bate com {w}x{h}", probe.stdout.len()))?;

        // Um único processo no dispositivo repete o screencap: sem custo de iniciar um programa por quadro.
        let mut cmd = avd::adb_command(root);
        cmd.args(["-s", &serial, "exec-out", "while true; do screencap; done"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let mut out = child.stdout.take().ok_or("sem saída do adb")?;
        let latest = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        let fps = Arc::new(AtomicU32::new(0));
        let (l, a, f) = (Arc::clone(&latest), Arc::clone(&alive), Arc::clone(&fps));
        std::thread::spawn(move || {
            let (mut seq, mut counted, mut since) = (0u64, 0u32, Instant::now());
            while let Ok(Some(frame)) = read_frame(&mut out, header) {
                seq += 1;
                counted += 1;
                if let Ok(mut slot) = l.lock() {
                    *slot = Some((seq, Arc::new(frame)));
                }
                if since.elapsed() >= Duration::from_secs(1) {
                    f.store(counted, Ordering::Relaxed);
                    counted = 0;
                    since = Instant::now();
                }
            }
            a.store(false, Ordering::Relaxed);
        });
        Ok(Screen { latest, child: Arc::new(Mutex::new(child)), alive, fps })
    }

    /// O quadro mais recente, se for mais novo que `after` (o número do último que o chamador viu).
    pub fn frame_after(&self, after: u64) -> Option<(u64, Arc<Frame>)> {
        self.latest.lock().ok().and_then(|s| s.as_ref().filter(|(seq, _)| *seq > after).cloned())
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------------------------
// Toque.

/// O dispositivo de entrada que recebe o toque e a faixa das coordenadas dele.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct TouchDevice {
    pub path: String,
    pub max_x: u32,
    pub max_y: u32,
}

/// Procura, na saída de `getevent -p`, o dispositivo com multitoque (eixos `0035` e `0036`).
pub fn parse_touch_device(getevent_p: &str) -> Option<TouchDevice> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in getevent_p.lines() {
        if let Some(rest) = line.trim().strip_prefix("add device") {
            let path = rest.split_once(": ").map(|(_, p)| p.trim().to_owned()).unwrap_or_default();
            blocks.push((path, Vec::new()));
        } else if let Some((_, lines)) = blocks.last_mut() {
            lines.push(line);
        }
    }
    let max_of = |lines: &[&str], code: &str| -> Option<u32> {
        let line = lines.iter().find(|l| l.trim_start().starts_with(code) && l.contains("max"))?;
        let after = line.split("max").nth(1)?.trim_start();
        after.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
    };
    blocks.into_iter().filter(|(p, _)| p.starts_with("/dev/input/")).find_map(|(path, lines)| {
        let (max_x, max_y) = (max_of(&lines, "0035")?, max_of(&lines, "0036")?);
        Some(TouchDevice { path, max_x, max_y })
    })
}

/// Eventos para encostar o dedo (protocolo multitoque B, contato 0).
pub fn touch_down_script(dev: &TouchDevice, tracking_id: u32, fx: f32, fy: f32) -> String {
    let (x, y) = scale(dev, fx, fy);
    let p = &dev.path;
    format!(
        "sendevent {p} 3 47 0\nsendevent {p} 3 57 {tracking_id}\nsendevent {p} 3 53 {x}\nsendevent {p} 3 54 {y}\nsendevent {p} 3 58 512\nsendevent {p} 0 0 0\n"
    )
}

pub fn touch_move_script(dev: &TouchDevice, fx: f32, fy: f32) -> String {
    let (x, y) = scale(dev, fx, fy);
    let p = &dev.path;
    format!("sendevent {p} 3 53 {x}\nsendevent {p} 3 54 {y}\nsendevent {p} 0 0 0\n")
}

pub fn touch_up_script(dev: &TouchDevice) -> String {
    let p = &dev.path;
    format!("sendevent {p} 3 57 -1\nsendevent {p} 0 0 0\n")
}

/// Posição como fração da tela (0 a 1) vira coordenada do dispositivo.
fn scale(dev: &TouchDevice, fx: f32, fy: f32) -> (u32, u32) {
    ((fx.clamp(0.0, 1.0) * dev.max_x as f32) as u32, (fy.clamp(0.0, 1.0) * dev.max_y as f32) as u32)
}

/// Uma sessão `adb shell` que fica aberta: os comandos entram por stdin, um por linha, em ordem.
struct Shell {
    child: Child,
    stdin: ChildStdin,
}

impl Shell {
    fn start(root: &Path, port: u16) -> Result<Shell, String> {
        let mut cmd = avd::adb_command(root);
        cmd.args(["-s", &avd::serial(port), "shell"]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("sem stdin")?;
        Ok(Shell { child, stdin })
    }

    fn run(&mut self, script: &str) -> Result<(), String> {
        self.stdin.write_all(script.as_bytes()).and_then(|_| self.stdin.flush()).map_err(|e| format!("o shell do dispositivo fechou: {e}"))
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------------------------
// Teclas e texto.

pub const KEY_HOME: u32 = 3;
pub const KEY_BACK: u32 = 4;
pub const KEY_VOLUME_UP: u32 = 24;
pub const KEY_VOLUME_DOWN: u32 = 25;
pub const KEY_POWER: u32 = 26;
pub const KEY_ENTER: u32 = 66;
pub const KEY_DEL: u32 = 67;
pub const KEY_TAB: u32 = 61;
pub const KEY_APP_SWITCH: u32 = 187;
pub const KEY_FORWARD_DEL: u32 = 112;
pub const KEY_DPAD_UP: u32 = 19;
pub const KEY_DPAD_DOWN: u32 = 20;
pub const KEY_DPAD_LEFT: u32 = 21;
pub const KEY_DPAD_RIGHT: u32 = 22;

/// Comando `input text` para o texto, ou `None` se não houver o que enviar. O `input text` só entende
/// ASCII imprimível; espaço vira `%s`, e o resto vai entre aspas simples para o shell não interpretar.
pub fn input_text_command(text: &str) -> Option<String> {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            ' ' => out.push_str("%s"),
            '\'' => out.push_str("'\\''"),
            c if c.is_ascii_graphic() => out.push(c),
            _ => {}
        }
    }
    (!out.is_empty()).then(|| format!("input text '{out}'\n"))
}

pub fn keyevent_command(code: u32) -> String {
    format!("input keyevent {code}\n")
}

// ---------------------------------------------------------------------------------------------
// O simulador inteiro.

/// Tela ao vivo e entrada de um emulador em execução.
pub struct Simulator {
    pub screen: Screen,
    touch_dev: TouchDevice,
    /// Sessão rápida: só `sendevent`, para o toque não esperar um `input` lento.
    touch_shell: Shell,
    /// Sessão das teclas e do texto (`input` leva centenas de ms por comando).
    keys_shell: Shell,
    next_id: u32,
    pub port: u16,
}

impl Simulator {
    pub fn start(root: &Path, port: u16) -> Result<Simulator, String> {
        let events = avd::adb(root, &["-s", &avd::serial(port), "shell", "getevent", "-p"])?;
        let touch_dev = parse_touch_device(&events).ok_or("o emulador não tem um dispositivo de toque")?;
        Ok(Simulator {
            screen: Screen::start(root, port)?,
            touch_dev,
            touch_shell: Shell::start(root, port)?,
            keys_shell: Shell::start(root, port)?,
            next_id: 0,
            port,
        })
    }

    pub fn touch_down(&mut self, fx: f32, fy: f32) -> Result<(), String> {
        self.next_id = (self.next_id + 1) % 12;
        let script = touch_down_script(&self.touch_dev, self.next_id, fx, fy);
        self.touch_shell.run(&script)
    }

    pub fn touch_move(&mut self, fx: f32, fy: f32) -> Result<(), String> {
        let script = touch_move_script(&self.touch_dev, fx, fy);
        self.touch_shell.run(&script)
    }

    pub fn touch_up(&mut self) -> Result<(), String> {
        let script = touch_up_script(&self.touch_dev);
        self.touch_shell.run(&script)
    }

    pub fn key(&mut self, code: u32) -> Result<(), String> {
        self.keys_shell.run(&keyevent_command(code))
    }

    pub fn text(&mut self, text: &str) -> Result<(), String> {
        match input_text_command(text) {
            Some(cmd) => self.keys_shell.run(&cmd),
            None => Ok(()),
        }
    }

    pub fn alive(&self) -> bool {
        self.screen.alive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn raw(w: u32, h: u32, format: u32, header: usize, px: &[u8; 4]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in [w, h, format] {
            v.extend(n.to_le_bytes());
        }
        v.resize(header, 0);
        for _ in 0..w * h {
            v.extend(px);
        }
        v
    }

    #[test]
    fn header_length_is_deduced_from_the_frame_size() {
        assert_eq!(header_len(12 + 480 * 800 * 4, 480, 800), Some(12));
        assert_eq!(header_len(16 + 480 * 800 * 4, 480, 800), Some(16));
        assert_eq!(header_len(1000, 480, 800), None, "menor que o quadro");
        assert_eq!(header_len(20 + 480 * 800 * 4, 480, 800), None, "sobra estranha");
    }

    #[test]
    fn reads_back_to_back_frames_with_either_header_size() {
        for header in [12usize, 16] {
            let mut bytes = raw(2, 3, 1, header, &[10, 20, 30, 255]);
            bytes.extend(raw(2, 3, 1, header, &[1, 2, 3, 4]));
            let mut r = Cursor::new(bytes);
            let a = read_frame(&mut r, header).unwrap().unwrap();
            assert_eq!((a.width, a.height, a.rgba.len()), (2, 3, 24));
            assert_eq!(&a.rgba[..4], &[10, 20, 30, 255]);
            let b = read_frame(&mut r, header).unwrap().unwrap();
            assert_eq!(&b.rgba[..4], &[1, 2, 3, 4]);
            assert!(read_frame(&mut r, header).unwrap().is_none(), "fim do fluxo");
        }
    }

    #[test]
    fn converts_other_pixel_formats_and_rejects_garbage() {
        let bgra = read_frame(&mut Cursor::new(raw(1, 1, 5, 12, &[30, 20, 10, 255])), 12).unwrap().unwrap();
        assert_eq!(&bgra.rgba, &[10, 20, 30, 255], "BGRA vira RGBA");
        let rgbx = read_frame(&mut Cursor::new(raw(1, 1, 2, 12, &[1, 2, 3, 0])), 12).unwrap().unwrap();
        assert_eq!(rgbx.rgba[3], 255, "RGBX tem alfa indefinido: fica opaco");
        assert!(read_frame(&mut Cursor::new(raw(1, 1, 99, 12, &[0; 4])), 12).is_err());
        assert!(read_frame(&mut Cursor::new(raw(0, 5, 1, 12, &[0; 4])), 12).is_err());
        assert!(read_frame(&mut Cursor::new(raw(100_000, 1, 1, 12, &[0; 4])), 12).is_err(), "tamanho absurdo não aloca");
        let mut truncated = raw(4, 4, 1, 12, &[0; 4]);
        truncated.truncate(30);
        assert!(read_frame(&mut Cursor::new(truncated), 12).is_err(), "quadro cortado no meio é erro, não fim limpo");
    }

    const GETEVENT: &str = "could not get driver version for /dev/input/mouse0, Not a typewriter
add device 1: /dev/input/event0
  name:     \"Power Button\"
  events:
    KEY (0001): 0074
add device 2: /dev/input/event1
  name:     \"qwerty2\"
  events:
    ABS (0003): 0000  : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
                0035  : value 0, min 0, max 32767, fuzz 0, flat 0, resolution 0
                0036  : value 0, min 0, max 24000, fuzz 0, flat 0, resolution 0
                003a  : value 0, min 0, max 1024, fuzz 0, flat 0, resolution 0
";

    #[test]
    fn finds_the_multitouch_device_and_its_range() {
        assert_eq!(parse_touch_device(GETEVENT), Some(TouchDevice { path: "/dev/input/event1".into(), max_x: 32767, max_y: 24000 }));
        assert_eq!(parse_touch_device("add device 1: /dev/input/event0\n  name: \"x\"\n"), None);
        assert_eq!(parse_touch_device(""), None);
    }

    #[test]
    fn touch_scripts_follow_the_multitouch_protocol() {
        let dev = TouchDevice { path: "/dev/input/event1".into(), max_x: 32767, max_y: 32767 };
        let down = touch_down_script(&dev, 3, 0.5, 0.25);
        let lines: Vec<&str> = down.lines().collect();
        assert_eq!(lines[0], "sendevent /dev/input/event1 3 47 0", "contato 0");
        assert_eq!(lines[1], "sendevent /dev/input/event1 3 57 3", "id do contato");
        assert_eq!(lines[2], "sendevent /dev/input/event1 3 53 16383");
        assert_eq!(lines[3], "sendevent /dev/input/event1 3 54 8191");
        assert_eq!(lines.last().unwrap(), &"sendevent /dev/input/event1 0 0 0", "termina com SYN_REPORT");
        assert_eq!(touch_move_script(&dev, 1.0, 0.0).lines().count(), 3);
        assert!(touch_move_script(&dev, 2.0, -1.0).contains("3 53 32767"), "fora da tela é limitado");
        assert!(touch_move_script(&dev, 2.0, -1.0).contains("3 54 0"));
        assert_eq!(touch_up_script(&dev), "sendevent /dev/input/event1 3 57 -1\nsendevent /dev/input/event1 0 0 0\n");
    }

    #[test]
    fn text_is_escaped_for_input_text() {
        assert_eq!(input_text_command("ola mundo").unwrap(), "input text 'ola%smundo'\n");
        assert_eq!(input_text_command("it's").unwrap(), "input text 'it'\\''s'\n");
        assert_eq!(input_text_command("ação").unwrap(), "input text 'ao'\n", "o que não é ASCII é descartado");
        assert_eq!(input_text_command("😀"), None);
        assert_eq!(input_text_command(""), None);
        assert_eq!(keyevent_command(KEY_BACK), "input keyevent 4\n");
    }
}

#[cfg(test)]
#[path = "device_e2e.rs"]
pub(crate) mod e2e;
