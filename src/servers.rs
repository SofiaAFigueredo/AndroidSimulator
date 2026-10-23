//! Como iniciar cada servidor de linguagem instalado no SDK do app (jdtls para Java, kotlin-language-server
//! para Kotlin) e que configuração mandar para ele.

use crate::highlight::Lang;
use crate::lsp::Client;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn language_id(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Java => Some("java"),
        Lang::Kotlin => Some("kotlin"),
        _ => None,
    }
}

fn server_dir(root: &Path, lang: Lang) -> Option<PathBuf> {
    match lang {
        Lang::Java => Some(root.join("lsp").join("jdtls")),
        Lang::Kotlin => Some(root.join("lsp").join("kotlin")),
        _ => None,
    }
}

/// Há servidor instalado para esta linguagem (e o JDK que ele precisa)?
pub fn installed(root: &Path, lang: Lang) -> bool {
    let has_jdk = crate::project::jdk_home(root).join("bin").is_dir();
    has_jdk
        && match lang {
            Lang::Java => launcher_jar(root).is_some(),
            Lang::Kotlin => kotlin_script(root).is_some_and(|p| p.is_file()),
            _ => false,
        }
}

fn launcher_jar(root: &Path) -> Option<PathBuf> {
    let plugins = server_dir(root, Lang::Java)?.join("plugins");
    std::fs::read_dir(plugins)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("org.eclipse.equinox.launcher_") && n.ends_with(".jar")))
}

fn kotlin_script(root: &Path) -> Option<PathBuf> {
    let bin = server_dir(root, Lang::Kotlin)?.join("bin");
    Some(bin.join(if cfg!(windows) { "kotlin-language-server.bat" } else { "kotlin-language-server" }))
}

fn jdtls_config_dir() -> &'static str {
    match (cfg!(windows), cfg!(target_os = "macos"), cfg!(target_arch = "aarch64")) {
        (true, _, _) => "config_win",
        (_, true, true) => "config_mac_arm",
        (_, true, false) => "config_mac",
        (_, _, true) => "config_linux_arm",
        _ => "config_linux",
    }
}

/// Hash curto e estável do caminho, para dar a cada projeto a própria pasta de dados do jdtls.
fn project_key(project: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in project.to_string_lossy().bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// A `android.jar` da plataforma mais nova instalada, para o Java enxergar `Activity`, `Bundle`…
pub fn android_jar(root: &Path) -> Option<PathBuf> {
    let platforms = root.join("platforms");
    std::fs::read_dir(platforms)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let api = crate::catalog::platform_api(&format!("platforms;{name}"))?;
            let jar = e.path().join("android.jar");
            jar.is_file().then_some((api, jar))
        })
        .max_by_key(|(api, _)| *api)
        .map(|(_, jar)| jar)
}

/// Comando que inicia o servidor, já com o JDK do app e uma pasta de dados própria.
pub fn command(root: &Path, lang: Lang, project: &Path) -> Result<Command, String> {
    let java = crate::project::jdk_home(root).join("bin").join(if cfg!(windows) { "java.exe" } else { "java" });
    match lang {
        Lang::Java => {
            let launcher = launcher_jar(root).ok_or("jdtls não instalado")?;
            let dir = server_dir(root, lang).ok_or("sem servidor")?;
            let data = root.join("lsp").join("workspaces").join(project_key(project));
            std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
            let mut c = Command::new(java);
            c.args([
                "-Declipse.application=org.eclipse.jdt.ls.core.id1",
                "-Dosgi.bundles.defaultStartLevel=4",
                "-Declipse.product=org.eclipse.jdt.ls.core.product",
                "-Dlog.level=ERROR",
                "-Xmx1G",
                "-XX:+UseParallelGC",
                "--add-modules=ALL-SYSTEM",
                "--add-opens",
                "java.base/java.util=ALL-UNNAMED",
                "--add-opens",
                "java.base/java.lang=ALL-UNNAMED",
                "-jar",
            ])
            .arg(launcher)
            .arg("-configuration")
            .arg(dir.join(jdtls_config_dir()))
            .arg("-data")
            .arg(data);
            Ok(hide_console(c))
        }
        Lang::Kotlin => {
            let script = kotlin_script(root).filter(|p| p.is_file()).ok_or("kotlin-language-server não instalado")?;
            let mut c = Command::new(script);
            // O servidor procura o kotlin-stdlib no cache do Gradle; sem isso, funções de extensão como
            // `map` e `filter` não aparecem. O cache do app não fica em `~/.gradle`.
            c.env("JAVA_HOME", crate::project::jdk_home(root)).env("GRADLE_USER_HOME", root.join("gradle-home"));
            // Sem um projeto Gradle que o servidor consiga importar, o classpath vem de um script global
            // que lemos de uma pasta de configuração só do app (`XDG_CONFIG_HOME`).
            let config = write_kotlin_classpath(root).map_err(|e| format!("não consegui preparar o classpath do Kotlin: {e}"))?;
            c.env("XDG_CONFIG_HOME", config);
            Ok(hide_console(c))
        }
        _ => Err("sem servidor para esta linguagem".into()),
    }
}

/// O `kotlin-stdlib` que o próprio servidor traz (não os `-jdk7`/`-jdk8`).
fn kotlin_stdlib(root: &Path) -> Option<PathBuf> {
    let lib = server_dir(root, Lang::Kotlin)?.join("lib");
    std::fs::read_dir(lib).ok()?.filter_map(Result::ok).map(|e| e.path()).find(|p| {
        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("kotlin-stdlib-") && !n.contains("jdk") && n.ends_with(".jar"))
    })
}

/// Entradas do classpath do Kotlin: a biblioteca padrão (funções de extensão como `map` e `filter`) e a
/// `android.jar` (Activity, Bundle…).
pub fn kotlin_classpath(root: &Path) -> Vec<PathBuf> {
    kotlin_stdlib(root).into_iter().chain(android_jar(root)).collect()
}

/// Grava o script `kotlin-language-server/classpath` que o servidor executa para saber o classpath e
/// devolve a pasta a usar como `XDG_CONFIG_HOME`.
fn write_kotlin_classpath(root: &Path) -> std::io::Result<PathBuf> {
    let config = root.join("lsp").join("kls-config");
    let dir = config.join("kotlin-language-server");
    std::fs::create_dir_all(&dir)?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    let joined = kotlin_classpath(root).iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(sep);
    if cfg!(windows) {
        std::fs::write(dir.join("classpath.bat"), format!("@echo off\r\necho {joined}\r\n"))?;
    } else {
        let script = dir.join("classpath");
        std::fs::write(&script, format!("#!/bin/sh\necho \"{joined}\"\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(config)
}

fn hide_console(mut c: Command) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Configuração enviada no `initialize`. Para o Java: sem importar Gradle/Maven (o import do AGP é lento
/// e frágil), tratando `app/src/main/java` como fonte e a `android.jar` como biblioteca.
pub fn init_options(root: &Path, lang: Lang) -> Value {
    match lang {
        Lang::Java => {
            let libs: Vec<String> = android_jar(root).into_iter().map(|p| p.to_string_lossy().into_owned()).collect();
            json!({"settings": {"java": {
                "import": {"gradle": {"enabled": false}, "maven": {"enabled": false}},
                "configuration": {"updateBuildConfiguration": "disabled"},
                "project": {"referencedLibraries": libs, "sourcePaths": ["app/src/main/java", "src"]},
            }}})
        }
        _ => json!({}),
    }
}

/// Inicia e inicializa o servidor. Bloqueante (jdtls leva dezenas de segundos): chame de uma thread.
pub fn start(root: &Path, lang: Lang, project: &Path) -> Result<Client, String> {
    let mut cmd = command(root, lang, project)?;
    Client::start(&mut cmd, project, init_options(root, lang), Duration::from_secs(180))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{self, parse_completion};
    use std::time::Instant;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("andsim-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn language_ids() {
        assert_eq!(language_id(Lang::Java), Some("java"));
        assert_eq!(language_id(Lang::Kotlin), Some("kotlin"));
        assert_eq!(language_id(Lang::Xml), None);
    }

    #[test]
    fn nothing_is_installed_in_an_empty_sdk() {
        let root = tmp("lsp-empty");
        assert!(!installed(&root, Lang::Java) && !installed(&root, Lang::Kotlin));
        assert!(command(&root, Lang::Java, &root).is_err());
        assert!(command(&root, Lang::Kotlin, &root).is_err());
        assert!(command(&root, Lang::Plain, &root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn each_project_gets_its_own_data_dir_key() {
        assert_eq!(project_key(Path::new("/a/b")), project_key(Path::new("/a/b")));
        assert_ne!(project_key(Path::new("/a/b")), project_key(Path::new("/a/c")));
        assert_eq!(project_key(Path::new("/a/b")).len(), 16);
    }

    #[test]
    fn finds_the_newest_whole_api_android_jar() {
        let root = tmp("lsp-jar");
        for n in ["android-34", "android-37.0", "android-37.2", "android-38"] {
            std::fs::create_dir_all(root.join("platforms").join(n)).unwrap();
            std::fs::write(root.join("platforms").join(n).join("android.jar"), b"x").unwrap();
        }
        let jar = android_jar(&root).unwrap();
        assert!(jar.to_string_lossy().contains("android-37.0"), "{jar:?}");
        assert!(android_jar(&tmp("lsp-nojar")).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn kotlin_classpath_script_lists_stdlib_and_android_jar() {
        let root = tmp("lsp-kcp");
        std::fs::create_dir_all(root.join("lsp/kotlin/lib")).unwrap();
        for n in ["kotlin-stdlib-2.1.0.jar", "kotlin-stdlib-jdk8-2.1.0.jar", "kotlin-reflect-2.1.0.jar"] {
            std::fs::write(root.join("lsp/kotlin/lib").join(n), b"x").unwrap();
        }
        std::fs::create_dir_all(root.join("platforms/android-37.0")).unwrap();
        std::fs::write(root.join("platforms/android-37.0/android.jar"), b"x").unwrap();
        let cp = kotlin_classpath(&root);
        assert_eq!(cp.len(), 2);
        assert!(cp[0].ends_with("kotlin-stdlib-2.1.0.jar"), "{cp:?}");
        assert!(cp[1].ends_with("android.jar"));
        let config = write_kotlin_classpath(&root).unwrap();
        let script = config.join("kotlin-language-server").join(if cfg!(windows) { "classpath.bat" } else { "classpath" });
        let text = std::fs::read_to_string(script).unwrap();
        assert!(text.contains("kotlin-stdlib-2.1.0.jar") && text.contains("android.jar") && !text.contains("jdk8"), "{text}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn java_options_point_at_the_android_jar_and_skip_gradle_import() {
        let root = tmp("lsp-opts");
        std::fs::create_dir_all(root.join("platforms/android-37.0")).unwrap();
        std::fs::write(root.join("platforms/android-37.0/android.jar"), b"x").unwrap();
        let o = init_options(&root, Lang::Java);
        assert_eq!(o["settings"]["java"]["import"]["gradle"]["enabled"], false);
        assert!(o["settings"]["java"]["project"]["referencedLibraries"][0].as_str().unwrap().ends_with("android.jar"));
        assert_eq!(init_options(&root, Lang::Kotlin), json!({}));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Pede completion até o servidor responder com algo (o jdtls devolve vazio enquanto ainda carrega o projeto).
    fn complete_until(client: &Client, uri: &str, line: u32, col: u32, want: &str, secs: u64) -> Result<Vec<String>, String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let v = client.completion(uri, line, col).recv_timeout(Duration::from_secs(60)).map_err(|_| "sem resposta")??;
            let labels: Vec<String> = parse_completion(&v).into_iter().map(|i| i.label).collect();
            if labels.iter().any(|l| l.starts_with(want)) {
                return Ok(labels);
            }
            if Instant::now() > deadline {
                return Err(format!("não apareceu {want:?}; vieram {} itens: {:?}", labels.len(), &labels[..labels.len().min(8)]));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Autocomplete real em Java, com o jdtls instalado no SDK do app: `System.` precisa oferecer `out`.
    /// `cargo test java_completion_with_the_real_jdtls -- --ignored --nocapture`
    #[test]
    #[ignore = "inicia o jdtls (precisa do JDK e do servidor instalados)"]
    fn java_completion_with_the_real_jdtls() {
        let root = crate::sdk::default_root();
        assert!(installed(&root, Lang::Java), "instale o jdtls pelo app (perfil Completa)");
        let project = tmp("jdtls-proj");
        std::fs::create_dir_all(project.join("src")).unwrap();
        let file = project.join("src/Main.java");
        let code = "public class Main {\n    public static void main(String[] args) {\n        System.\n    }\n}\n";
        std::fs::write(&file, code).unwrap();
        let t0 = Instant::now();
        let client = start(&root, Lang::Java, &project).expect("iniciar o jdtls");
        println!("jdtls inicializado em {:?}", t0.elapsed());
        let uri = lsp::path_to_uri(&file);
        client.did_open(&uri, "java", 1, code).unwrap();
        let (line, col) = (2, "        System.".len() as u32);
        let labels = complete_until(&client, &uri, line, col, "out", 120).expect("completion");
        println!("RESULTADO java: {} itens em {:?}; alguns: {:?}", labels.len(), t0.elapsed(), &labels[..labels.len().min(6)]);
        assert!(labels.iter().any(|l| l.starts_with("currentTimeMillis")), "esperava currentTimeMillis em {labels:?}");
        client.shutdown();
        let _ = std::fs::remove_dir_all(project);
    }

    /// Autocomplete real em Kotlin: `lista.` precisa oferecer funções de coleção como `map`.
    /// `cargo test kotlin_completion_with_the_real_server -- --ignored --nocapture`
    #[test]
    #[ignore = "inicia o kotlin-language-server (precisa do JDK e do servidor instalados)"]
    fn kotlin_completion_with_the_real_server() {
        let root = crate::sdk::default_root();
        assert!(installed(&root, Lang::Kotlin), "instale o servidor Kotlin pelo app (perfil Completa)");
        let project = tmp("kls-proj");
        let file = project.join("Main.kt");
        // `lista.ma`: o servidor devolve no máximo 75 itens e as extensões (map, filter…) ficam depois do
        // corte; com o prefixo digitado elas aparecem.
        let code = "fun main() {\n    val lista = listOf(1, 2, 3)\n    lista.ma\n}\n";
        std::fs::write(&file, code).unwrap();
        let t0 = Instant::now();
        let client = start(&root, Lang::Kotlin, &project).expect("iniciar o kotlin-language-server");
        println!("kotlin-language-server inicializado em {:?}", t0.elapsed());
        let uri = lsp::path_to_uri(&file);
        client.did_open(&uri, "kotlin", 1, code).unwrap();
        let (line, col) = (2, "    lista.ma".len() as u32);
        let found = complete_until(&client, &uri, line, col, "map", 40);
        for l in client.log_lines().iter().filter(|l| l.to_lowercase().contains("classpath") || l.to_lowercase().contains("stdlib")).take(12) {
            println!("log do servidor> {l}");
        }
        let labels = found.expect("completion");
        println!("RESULTADO kotlin: {} itens em {:?}; alguns: {:?}", labels.len(), t0.elapsed(), &labels[..labels.len().min(6)]);
        client.shutdown();
        let _ = std::fs::remove_dir_all(project);
    }

    /// Confere que as classes do Android (`Bundle`, `Activity`…) aparecem nas sugestões, em Java e em Kotlin,
    /// depois de `import android.os.Bu`. Usa a `android.jar` da plataforma instalada no SDK do app.
    /// `cargo test android_classes_are_suggested -- --ignored --nocapture`
    #[test]
    #[ignore = "inicia o jdtls e o kotlin-language-server"]
    fn android_classes_are_suggested() {
        let root = crate::sdk::default_root();
        assert!(android_jar(&root).is_some(), "instale a plataforma Android (perfil Completa)");
        for (lang, file, code) in [
            (Lang::Java, "app/src/main/java/com/exemplo/Main.java", "package com.exemplo;

import android.os.Bu;

public class Main {}
"),
            (Lang::Kotlin, "app/src/main/java/com/exemplo/Main.kt", "package com.exemplo

import android.os.Bu

class Main
"),
        ] {
            assert!(installed(&root, lang), "instale o servidor de {lang:?}");
            let project = tmp(&format!("android-{lang:?}"));
            let path = project.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, code).unwrap();
            let t0 = Instant::now();
            let client = start(&root, lang, &project).expect("iniciar o servidor");
            let uri = lsp::path_to_uri(&path);
            client.did_open(&uri, language_id(lang).unwrap(), 1, code).unwrap();
            let col = "import android.os.Bu".len() as u32;
            let labels = complete_until(&client, &uri, 2, col, "Bundle", 120).unwrap_or_else(|e| panic!("{lang:?}: {e}"));
            println!("RESULTADO {lang:?}: Bundle apareceu em {:?} ({} itens)", t0.elapsed(), labels.len());
            client.shutdown();
            let _ = std::fs::remove_dir_all(project);
        }
    }
}
