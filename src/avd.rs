//! Dispositivos virtuais: cria o AVD escrevendo os `.ini` direto (sem Java nem
//! `avdmanager`), inicia o emulador do SDK do app e espera o boot terminar.
//!
//! O emulador e o adb rodam isolados da instalação do Android Studio: o app usa o
//! próprio servidor adb (porta 5038), a própria pasta de AVDs e o próprio ANDROID_USER_HOME.

use crate::catalog::Package;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const ADB_PORT: u16 = 5038;

pub fn avd_home(root: &Path) -> PathBuf {
    root.join("avd")
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

pub fn adb_path(root: &Path) -> PathBuf {
    root.join("platform-tools").join(exe("adb"))
}

pub fn emulator_path(root: &Path) -> PathBuf {
    root.join("emulator").join(exe("emulator"))
}

/// Nome do AVD a partir do pacote: `API10_default_x86`.
pub fn default_name(img: &Package) -> String {
    let parts: Vec<&str> = img.path.split(';').collect();
    let api = parts.get(1).map(|s| s.trim_start_matches("android-")).unwrap_or("x");
    format!("API{}_{}_{}", api, parts.get(2).unwrap_or(&"img"), parts.get(3).unwrap_or(&"abi"))
}

fn cpu_arch(abi: &str) -> &str {
    match abi {
        "arm64-v8a" => "arm64",
        "armeabi-v7a" => "arm",
        other => other,
    }
}

pub fn config_ini(name: &str, img: &Package) -> String {
    let parts: Vec<&str> = img.path.split(';').collect();
    let (api, tag, abi) = (parts.get(1).copied().unwrap_or(""), parts.get(2).copied().unwrap_or(""), parts.get(3).copied().unwrap_or(""));
    let ram = if img.api.as_deref().and_then(|a| a.parse::<u32>().ok()).is_some_and(|a| a < 16) { 512 } else { 1536 };
    format!(
        "AvdId={name}\n\
         avd.ini.displayname={name}\n\
         PlayStore.enabled={play}\n\
         abi.type={abi}\n\
         hw.cpu.arch={arch}\n\
         hw.ramSize={ram}\n\
         image.sysdir.1=system-images/{api}/{tag}/{abi}/\n\
         tag.id={tag}\n\
         hw.lcd.width=480\n\
         hw.lcd.height=800\n\
         hw.lcd.density=240\n\
         hw.keyboard=yes\n\
         hw.gpu.enabled=yes\n\
         hw.gpu.mode=auto\n\
         disk.dataPartition.size=2048M\n\
         showDeviceFrame=no\n",
        play = tag == "google_apis_playstore",
        arch = cpu_arch(abi),
    )
}

pub fn create(root: &Path, name: &str, img: &Package) -> Result<(), String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("nome de AVD inválido: {name}"));
    }
    let home = avd_home(root);
    let dir = home.join(format!("{name}.avd"));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("config.ini"), config_ini(name, img)).map_err(|e| e.to_string())?;
    let api = img.path.split(';').nth(1).unwrap_or("");
    fs::write(
        home.join(format!("{name}.ini")),
        format!("avd.ini.encoding=UTF-8\npath={}\npath.rel=avd/{name}.avd\ntarget={api}\n", dir.display()),
    )
    .map_err(|e| e.to_string())
}

pub fn list(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(avd_home(root))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".ini")).map(str::to_owned))
        .collect();
    v.sort();
    v
}

pub fn delete(root: &Path, name: &str) -> Result<(), String> {
    let home = avd_home(root);
    let _ = fs::remove_file(home.join(format!("{name}.ini")));
    let dir = home.join(format!("{name}.avd"));
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn isolated(cmd: &mut Command, root: &Path) {
    cmd.env("ANDROID_SDK_ROOT", root)
        .env("ANDROID_HOME", root)
        .env("ANDROID_AVD_HOME", avd_home(root))
        .env("ANDROID_USER_HOME", root.join("user"))
        .env("ANDROID_EMULATOR_HOME", root.join("user"))
        .env("ANDROID_ADB_SERVER_PORT", ADB_PORT.to_string());
}

/// Porta de console do emulador (o adb vê `emulator-<porta>`).
pub fn serial(port: u16) -> String {
    format!("emulator-{port}")
}

pub struct Launch {
    pub headless: bool,
    pub port: u16,
    /// Modo de renderização usado sem janela (`swiftshader_indirect`, `guest`, `angle_indirect`…).
    pub gpu: String,
}

impl Launch {
    pub fn new(headless: bool, port: u16) -> Self {
        Self { headless, port, gpu: "swiftshader_indirect".into() }
    }
}

pub fn launch(root: &Path, name: &str, opts: &Launch) -> Result<Child, String> {
    let emu = emulator_path(root);
    if !emu.exists() {
        return Err(format!("emulador não instalado ({})", emu.display()));
    }
    fs::create_dir_all(root.join("user")).map_err(|e| e.to_string())?;
    let log = fs::File::create(avd_home(root).join(format!("{name}.log"))).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(emu);
    cmd.args(["-avd", name, "-port", &opts.port.to_string(), "-no-snapshot", "-no-audio", "-no-boot-anim"]);
    if opts.headless {
        cmd.args(["-no-window", "-gpu", &opts.gpu]);
    }
    isolated(&mut cmd, root);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: sem console preto atrás
    }
    cmd.spawn().map_err(|e| e.to_string())
}

/// `adb` isolado (servidor, pasta de usuário e SDK do app) e sem janela de console no Windows.
pub fn adb_command(root: &Path) -> Command {
    let mut cmd = Command::new(adb_path(root));
    isolated(&mut cmd, root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

pub fn adb(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = adb_command(root);
    cmd.args(args);
    let out = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{stdout} {}", String::from_utf8_lossy(&out.stderr).trim()).trim().to_owned())
    }
}

/// Captura a tela do emulador como PNG (`adb exec-out screencap -p`).
pub fn screenshot(root: &Path, port: u16, out: &Path) -> Result<(), String> {
    let mut cmd = adb_command(root);
    cmd.args(["-s", &serial(port), "exec-out", "screencap", "-p"]);
    let o = cmd.output().map_err(|e| e.to_string())?;
    if !o.status.success() || !o.stdout.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Err(format!("screencap falhou: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    fs::write(out, o.stdout).map_err(|e| e.to_string())
}

pub fn is_booted(root: &Path, port: u16) -> bool {
    adb(root, &["-s", &serial(port), "shell", "getprop", "sys.boot_completed"]).is_ok_and(|o| o == "1")
}

/// Instala o APK e abre o app. Falha com uma mensagem clara se o emulador ainda não terminou o boot.
pub fn run_apk(root: &Path, port: u16, apk: &Path, package: &str, on_line: &mut dyn FnMut(&str)) -> Result<(), String> {
    let serial = serial(port);
    if !is_booted(root, port) {
        return Err(format!("o emulador {serial} ainda não terminou de iniciar (ou não está rodando)"));
    }
    // Em emulador sem janela a tela costuma estar apagada, e o app abriria "no escuro".
    let _ = adb(root, &["-s", &serial, "shell", "input", "keyevent", "KEYCODE_WAKEUP"]);
    on_line(&format!("Instalando {} em {serial}…", apk.display()));
    let out = adb(root, &["-s", &serial, "install", "-r", &apk.to_string_lossy()])?;
    if !out.contains("Success") {
        return Err(format!("a instalação falhou: {out}"));
    }
    on_line(&format!("Abrindo {package}…"));
    let out = adb(root, &["-s", &serial, "shell", "monkey", "-p", package, "-c", "android.intent.category.LAUNCHER", "1"])?;
    if out.contains("No activities found") || out.contains("monkey aborted") {
        return Err(format!("não consegui abrir {package}: {out}"));
    }
    on_line(&format!("{package} aberto."));
    Ok(())
}

/// Janela em foco (`mCurrentFocus`), para diagnosticar se o app chegou à tela.
#[cfg(test)]
pub fn focused_window(root: &Path, port: u16) -> String {
    adb(root, &["-s", &serial(port), "shell", "dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'"]).unwrap_or_default()
}

/// Confere se o app está em execução: `pidof` devolve o PID.
#[cfg(test)]
pub fn app_pid(root: &Path, port: u16, package: &str) -> Option<String> {
    adb(root, &["-s", &serial(port), "shell", "pidof", package]).ok().filter(|p| !p.is_empty())
}

#[allow(dead_code)] // usada pelo teste de boot; a UI ainda não mostra o progresso do boot
/// Espera `sys.boot_completed=1`. Falha cedo se o processo do emulador morrer.
pub fn wait_boot(root: &Path, child: &mut Child, port: u16, timeout: Duration) -> Result<Duration, String> {
    let start = Instant::now();
    let serial = serial(port);
    while start.elapsed() < timeout {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("o emulador encerrou antes do boot ({status})"));
        }
        if adb(root, &["-s", &serial, "shell", "getprop", "sys.boot_completed"]).is_ok_and(|o| o == "1") {
            return Ok(start.elapsed());
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    Err(format!("sem boot em {}s", timeout.as_secs()))
}

pub fn stop(root: &Path, child: &mut Child, port: u16) {
    let _ = adb(root, &["-s", &serial(port), "emu", "kill"]);
    std::thread::sleep(Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();
    let _ = adb(root, &["kill-server"]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Kind;

    fn img(path: &str, api: &str) -> Package {
        Package {
            path: path.into(),
            name: "x".into(),
            kind: Kind::SystemImage,
            api: Some(api.into()),
            abi: None,
            revision: (1, 0, 0),
            size: 1,
            url: String::new(),
            checksum: String::new(),
            license: String::new(),
            package_xml: String::new(),
        }
    }

    #[test]
    fn config_points_to_the_image_dir() {
        let p = img("system-images;android-10;default;x86", "10");
        assert_eq!(default_name(&p), "API10_default_x86");
        let c = config_ini("API10_default_x86", &p);
        assert!(c.contains("image.sysdir.1=system-images/android-10/default/x86/\n"));
        assert!(c.contains("hw.cpu.arch=x86\n"));
        assert!(c.contains("hw.ramSize=512\n"));
        assert!(c.contains("PlayStore.enabled=false\n"));
    }

    #[test]
    fn create_list_delete_roundtrip_and_name_validation() {
        let root = std::env::temp_dir().join(format!("andsim-avd-{}", std::process::id()));
        let p = img("system-images;android-30;google_apis_playstore;x86_64", "30");
        assert!(create(&root, "../fora", &p).is_err());
        create(&root, "Teste", &p).unwrap();
        assert_eq!(list(&root), vec!["Teste".to_string()]);
        assert!(fs::read_to_string(avd_home(&root).join("Teste.avd/config.ini")).unwrap().contains("PlayStore.enabled=true"));
        delete(&root, "Teste").unwrap();
        assert!(list(&root).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// Sobe o emulador de verdade, sem janela. Precisa de emulador, platform-tools e das
    /// imagens já instaladas no SDK do app (baixe pela tela Dispositivos, que mostra a licença).
    /// `ANDSIM_IMAGES="system-images;android-10;default;x86,..." cargo test boots -- --ignored --nocapture`
    #[test]
    #[ignore = "inicia o emulador"]
    fn boots_images() {
        let root = crate::sdk::default_root();
        let wanted = std::env::var("ANDSIM_IMAGES").expect("defina ANDSIM_IMAGES");
        let (cat, _) = crate::catalog::fetch_all();
        for path in wanted.split(',') {
            let img = cat.packages.iter().find(|p| p.path == path).unwrap_or_else(|| panic!("{path} fora do catálogo"));
            assert!(crate::sdk::installed(&root).contains_key(path), "{path} não instalada");
            let name = format!("probe_{}", default_name(img));
            create(&root, &name, img).unwrap();
            let port = 5580;
            let mut child = launch(&root, &name, &Launch::new(true, port)).unwrap();
            let r = wait_boot(&root, &mut child, port, Duration::from_secs(240));
            let sdk = adb(&root, &["-s", &serial(port), "shell", "getprop", "ro.build.version.sdk"]).unwrap_or_default();
            println!("RESULTADO {path}: {r:?} (ro.build.version.sdk={sdk})");
            stop(&root, &mut child, port);
            let _ = delete(&root, &name);
        }
    }
}
