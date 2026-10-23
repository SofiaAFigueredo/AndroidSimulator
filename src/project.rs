//! Modelo de projeto Android (Kotlin, sem dependências do AndroidX, para o primeiro build
//! baixar o mínimo) e a compilação dele com o Gradle e o JDK do SDK do app.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Versão do plugin do Android para Gradle usada pelo modelo (compatível com o Gradle 9.6+).
pub const AGP_VERSION: &str = "9.4.1";

pub struct NewProject<'a> {
    pub name: &'a str,
    pub package: &'a str,
    pub compile_sdk: u32,
    pub build_tools: &'a str,
    pub sdk_root: &'a Path,
}

pub fn valid_package(p: &str) -> bool {
    let parts: Vec<&str> = p.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|s| {
            let mut c = s.chars();
            c.next().is_some_and(|f| f.is_ascii_lowercase()) && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_')
        })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn kt_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$")
}

fn write(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}

/// Cria o projeto em `dir`, que precisa estar vazio ou não existir.
pub fn create(dir: &Path, p: &NewProject) -> Result<(), String> {
    if !valid_package(p.package) {
        return Err(format!("pacote inválido: {} (use algo como com.exemplo.app)", p.package));
    }
    if p.name.trim().is_empty() {
        return Err("o nome do app está vazio".into());
    }
    if dir.exists() && fs::read_dir(dir).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} não está vazia", dir.display()));
    }
    let (pkg, cs) = (p.package, p.compile_sdk);
    let sdk_dir = p.sdk_root.display().to_string().replace('\\', "/");

    write(&dir.join("settings.gradle.kts"), &format!(
        "pluginManagement {{\n    repositories {{\n        google()\n        mavenCentral()\n        gradlePluginPortal()\n    }}\n}}\n\
         dependencyResolutionManagement {{\n    repositories {{\n        google()\n        mavenCentral()\n    }}\n}}\n\
         rootProject.name = \"{}\"\ninclude(\":app\")\n",
        kt_escape(p.name)
    ))?;
    write(&dir.join("build.gradle.kts"), &format!(
        "plugins {{\n    id(\"com.android.application\") version \"{AGP_VERSION}\" apply false\n}}\n"
    ))?;
    write(&dir.join("gradle.properties"), "org.gradle.jvmargs=-Xmx1536m -Dfile.encoding=UTF-8\nandroid.useAndroidX=false\n")?;
    write(&dir.join("local.properties"), &format!("sdk.dir={sdk_dir}\n"))?;
    write(&dir.join(".gitignore"), "/build\n/app/build\n/.gradle\nlocal.properties\n")?;
    write(&dir.join("app/build.gradle.kts"), &format!(
        "plugins {{\n    id(\"com.android.application\")\n}}\n\n\
         android {{\n    namespace = \"{pkg}\"\n    compileSdk = {cs}\n    buildToolsVersion = \"{}\"\n\n\
         \x20   defaultConfig {{\n        applicationId = \"{pkg}\"\n        minSdk = 24\n        targetSdk = {cs}\n        versionCode = 1\n        versionName = \"1.0\"\n    }}\n}}\n",
        p.build_tools
    ))?;
    write(&dir.join("app/src/main/AndroidManifest.xml"), &format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n\
         \x20   <application\n        android:label=\"{}\"\n        android:theme=\"@android:style/Theme.Material.Light.NoActionBar\">\n\
         \x20       <activity android:name=\".MainActivity\" android:exported=\"true\">\n\
         \x20           <intent-filter>\n\
         \x20               <action android:name=\"android.intent.action.MAIN\" />\n\
         \x20               <category android:name=\"android.intent.category.LAUNCHER\" />\n\
         \x20           </intent-filter>\n\
         \x20       </activity>\n\
         \x20   </application>\n</manifest>\n",
        xml_escape(p.name)
    ))?;
    let src: PathBuf = pkg.split('.').collect();
    write(&dir.join("app/src/main/java").join(src).join("MainActivity.kt"), &format!(
        "package {pkg}\n\nimport android.app.Activity\nimport android.os.Bundle\nimport android.widget.TextView\n\n\
         class MainActivity : Activity() {{\n    override fun onCreate(savedInstanceState: Bundle?) {{\n        super.onCreate(savedInstanceState)\n\
         \x20       val texto = TextView(this)\n        texto.text = \"Olá, {}!\"\n        texto.textSize = 24f\n        setContentView(texto)\n    }}\n}}\n",
        kt_escape(p.name)
    ))
}

pub fn jdk_home(root: &Path) -> PathBuf {
    root.join("jdk").join(crate::toolchain::JDK_MAJOR.to_string())
}

pub fn gradle_bin(root: &Path) -> PathBuf {
    root.join("gradle")
        .join(crate::toolchain::GRADLE_VERSION)
        .join("bin")
        .join(if cfg!(windows) { "gradle.bat" } else { "gradle" })
}

/// Roda `gradle <task>` com JDK, SDK e cache do Gradle do app, sem tocar no ambiente do usuário.
/// Cada linha de saída vai para `on_line`. Devolve o APK gerado, se houver.
pub fn build(root: &Path, project: &Path, task: &str, on_line: &mut dyn FnMut(&str)) -> Result<Option<PathBuf>, String> {
    let (jdk, gradle) = (jdk_home(root), gradle_bin(root));
    if !gradle.exists() || !jdk.exists() {
        return Err("JDK ou Gradle não instalados: baixe o perfil Completa".into());
    }
    let mut cmd = Command::new(&gradle);
    cmd.current_dir(project)
        .args([task, "--console=plain"])
        .env("JAVA_HOME", &jdk)
        .env("ANDROID_HOME", root)
        .env("ANDROID_SDK_ROOT", root)
        .env("ANDROID_USER_HOME", root.join("user"))
        .env("GRADLE_USER_HOME", root.join("gradle-home"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", gradle.display()))?;
    let (tx, rx) = mpsc::channel::<String>();
    let readers: Vec<_> = [
        child.stdout.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
        child.stderr.take().map(|o| Box::new(o) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        })
    })
    .collect();
    drop(tx);
    for line in rx {
        on_line(&line);
    }
    for r in readers {
        let _ = r.join();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("o Gradle terminou com {status}"));
    }
    Ok(find_apk(project))
}

pub fn find_apk(project: &Path) -> Option<PathBuf> {
    let dir = project.join("app/build/outputs/apk/debug");
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "apk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("andsim-{name}-{}", std::process::id()))
    }

    #[test]
    fn package_names() {
        assert!(valid_package("com.exemplo.app"));
        assert!(valid_package("br.com.minha_app2"));
        assert!(!valid_package("app"));
        assert!(!valid_package("Com.Exemplo"));
        assert!(!valid_package("com.1abc"));
        assert!(!valid_package("com..app"));
    }

    #[test]
    fn creates_the_expected_files_and_escapes_the_name() {
        let dir = tmp("proj");
        let sdk = tmp("sdk");
        let p = NewProject { name: "Meu \"App\" & $x", package: "com.exemplo.app", compile_sdk: 37, build_tools: "37.0.0", sdk_root: &sdk };
        create(&dir, &p).unwrap();
        for f in [
            "settings.gradle.kts",
            "build.gradle.kts",
            "local.properties",
            "app/build.gradle.kts",
            "app/src/main/AndroidManifest.xml",
            "app/src/main/java/com/exemplo/app/MainActivity.kt",
        ] {
            assert!(dir.join(f).exists(), "falta {f}");
        }
        let manifest = fs::read_to_string(dir.join("app/src/main/AndroidManifest.xml")).unwrap();
        assert!(manifest.contains("Meu &quot;App&quot; &amp; $x"));
        let kt = fs::read_to_string(dir.join("app/src/main/java/com/exemplo/app/MainActivity.kt")).unwrap();
        assert!(kt.contains("Olá, Meu \\\"App\\\" & \\$x!"));
        let gradle = fs::read_to_string(dir.join("app/build.gradle.kts")).unwrap();
        assert!(gradle.contains("compileSdk = 37") && gradle.contains("buildToolsVersion = \"37.0.0\""));
        assert!(create(&dir, &p).is_err(), "não deve sobrescrever pasta com conteúdo");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_input() {
        let sdk = tmp("sdk2");
        let bad = NewProject { name: "x", package: "Errado", compile_sdk: 37, build_tools: "37.0.0", sdk_root: &sdk };
        assert!(create(&tmp("p2"), &bad).is_err());
        let empty = NewProject { name: "  ", package: "com.a.b", compile_sdk: 37, build_tools: "37.0.0", sdk_root: &sdk };
        assert!(create(&tmp("p3"), &empty).is_err());
    }

    /// Compila um projeto novo de ponta a ponta no SDK do app: baixa plataforma, build-tools, JDK e
    /// Gradle (só o que falta), gera o modelo e roda `assembleDebug`. A licença do Android SDK precisa
    /// já ter sido aceita na interface. `cargo test builds_a_new_project -- --ignored --nocapture`
    #[test]
    #[ignore = "baixa centenas de MB e compila"]
    fn builds_a_new_project() {
        use crate::catalog::Kind;
        let root = crate::sdk::default_root();
        let (cat, warnings) = crate::catalog::fetch_all();
        assert!(warnings.is_empty(), "{warnings:?}");
        crate::sdk::ensure_metadata(&root, &cat);
        let needed: Vec<_> = [Kind::Platform, Kind::BuildTools, Kind::Jdk, Kind::Gradle]
            .iter()
            .map(|k| cat.latest(*k).unwrap_or_else(|| panic!("{k:?} fora do catálogo")))
            .collect();
        for pkg in &needed {
            if crate::sdk::installed(&root).contains_key(&pkg.path) {
                continue;
            }
            assert!(
                pkg.license.is_empty() || crate::sdk::accepted_licenses(&root).contains(&pkg.license),
                "licença {} não aceita; aceite pelo app",
                pkg.license
            );
            println!("baixando {} ({} MB)", pkg.name, pkg.size >> 20);
            crate::sdk::install(&root, pkg, &mut |_| {}).expect("install");
        }
        let api = crate::catalog::platform_api(&needed[0].path).expect("API da plataforma");
        let bt = needed[1].path.split(';').nth(1).unwrap().to_owned();
        let dir = std::env::temp_dir().join(format!("andsim-build-{}", std::process::id()));
        let p = NewProject { name: "Teste App", package: "com.exemplo.teste", compile_sdk: api, build_tools: &bt, sdk_root: &root };
        create(&dir, &p).expect("create");
        let mut lines = 0;
        let apk = build(&root, &dir, "assembleDebug", &mut |l| {
            lines += 1;
            println!("gradle> {l}");
        })
        .expect("build");
        let apk = apk.expect("sem APK");
        let size = fs::metadata(&apk).unwrap().len();
        println!("APK: {} ({} KB), {lines} linhas de saída", apk.display(), size / 1024);
        assert!(size > 1000);
        let mut zip = zip::ZipArchive::new(fs::File::open(&apk).unwrap()).unwrap();
        assert!(zip.by_name("classes.dex").is_ok() && zip.by_name("AndroidManifest.xml").is_ok());
        println!("PROJETO: {}", dir.display());
    }
}
