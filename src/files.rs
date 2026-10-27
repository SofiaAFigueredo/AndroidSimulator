//! Criar arquivos e pastas pelo app: validação do nome (sem sair da pasta escolhida e sem nomes que o
//! Windows não aceita) e o conteúdo inicial de um arquivo Kotlin ou Java, com o `package` certo.

use std::fs;
use std::path::{Component, Path, PathBuf};

const INVALID: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];

/// O caminho de um arquivo ou pasta novo dentro de `base`. `name` pode ter subpastas (`app/src/Foo.kt`).
/// Recusa caminho vazio, absoluto, com `..`, com caracteres inválidos, nomes reservados e o que já existe.
pub fn validate_new_path(base: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim().replace('\\', "/");
    if name.is_empty() {
        return Err("Digite um nome.".into());
    }
    if name.starts_with('/') || name.contains(':') {
        return Err("Use um caminho relativo à pasta escolhida.".into());
    }
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty()).collect();
    for part in &parts {
        if *part == "." || *part == ".." {
            return Err("O caminho não pode ter «.» nem «..».".into());
        }
        if part.chars().any(|c| INVALID.contains(&c) || c.is_control()) {
            return Err(format!("«{part}» tem caractere inválido (não use < > : \" | ? *)."));
        }
        if part.ends_with('.') || part.ends_with(' ') {
            return Err(format!("«{part}» não pode terminar em ponto ou espaço."));
        }
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            return Err(format!("«{part}» é um nome reservado do Windows."));
        }
    }
    let target = parts.iter().fold(base.to_path_buf(), |p, part| p.join(part));
    if target.exists() {
        return Err(format!("«{}» já existe.", target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    Ok(target)
}

fn is_identifier(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_alphabetic() || f == '_') && c.all(|x| x.is_alphanumeric() || x == '_')
}

/// O pacote de um arquivo em `src/<conjunto>/java/…` ou `src/<conjunto>/kotlin/…`, pelas pastas.
pub fn package_of(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| if let Component::Normal(s) = c { Some(s.to_string_lossy().into_owned()) } else { None })
        .collect();
    let at = (2..comps.len()).find(|&i| matches!(comps[i].as_str(), "java" | "kotlin") && comps[i - 2] == "src")?;
    let pkg: Vec<&str> = comps[at + 1..comps.len().saturating_sub(1)].iter().map(String::as_str).collect();
    (!pkg.is_empty() && pkg.iter().all(|p| is_identifier(p))).then(|| pkg.join("."))
}

/// Conteúdo inicial de um arquivo novo, pela extensão.
pub fn template_for(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let class = is_identifier(&stem).then_some(stem);
    match path.extension().and_then(|e| e.to_str()) {
        Some("kt") => {
            let pkg = package_of(path).map(|p| format!("package {p}\n\n")).unwrap_or_default();
            let body = class.map(|c| format!("class {c}\n")).unwrap_or_default();
            format!("{pkg}{body}")
        }
        Some("java") => {
            let pkg = package_of(path).map(|p| format!("package {p};\n\n")).unwrap_or_default();
            let body = class.map(|c| format!("public class {c} {{\n}}\n")).unwrap_or_default();
            format!("{pkg}{body}")
        }
        Some("xml") => "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n".to_owned(),
        _ => String::new(),
    }
}

/// Cria o arquivo (e as pastas do caminho) com o conteúdo inicial. Devolve o caminho criado.
pub fn create_file(base: &Path, name: &str) -> Result<PathBuf, String> {
    let path = validate_new_path(base, name)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("não consegui criar a pasta: {e}"))?;
    }
    fs::write(&path, template_for(&path)).map_err(|e| format!("não consegui criar o arquivo: {e}"))?;
    Ok(path)
}

pub fn create_dir(base: &Path, name: &str) -> Result<PathBuf, String> {
    let path = validate_new_path(base, name)?;
    fs::create_dir_all(&path).map_err(|e| format!("não consegui criar a pasta: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("andsim-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn accepts_plain_and_nested_names() {
        let base = tmp("ok");
        assert_eq!(validate_new_path(&base, "Foo.kt").unwrap(), base.join("Foo.kt"));
        assert_eq!(validate_new_path(&base, "  app/src\\Foo.kt ").unwrap(), base.join("app").join("src").join("Foo.kt"), "barras dos dois tipos e espaços nas pontas");
        assert_eq!(validate_new_path(&base, "a//b").unwrap(), base.join("a").join("b"), "barras repetidas");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_names_that_escape_or_windows_would_refuse() {
        let base = tmp("bad");
        for (name, why) in [
            ("", "vazio"),
            ("   ", "só espaços"),
            ("../fora.kt", "sai da pasta"),
            ("a/../../fora", "sai da pasta no meio"),
            ("./x", "ponto"),
            ("/etc/passwd", "absoluto"),
            ("C:\\x.kt", "unidade"),
            ("a:b.kt", "dois pontos"),
            ("a?.kt", "interrogação"),
            ("a*.kt", "asterisco"),
            ("a\"b", "aspas"),
            ("a|b", "barra vertical"),
            ("con", "reservado"),
            ("NUL.txt", "reservado com extensão"),
            ("pasta/aux.kt", "reservado dentro de subpasta"),
            ("fim.", "termina em ponto"),
            ("a\u{7}b", "caractere de controle"),
        ] {
            assert!(validate_new_path(&base, name).is_err(), "deveria recusar {name:?} ({why})");
        }
        assert!(validate_new_path(&base, "console.kt").is_ok(), "só o nome inteiro é reservado");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_what_already_exists() {
        let base = tmp("exists");
        fs::write(base.join("Foo.kt"), "x").unwrap();
        let err = validate_new_path(&base, "Foo.kt").unwrap_err();
        assert!(err.contains("já existe"), "{err}");
        assert!(create_file(&base, "Foo.kt").is_err());
        assert_eq!(fs::read_to_string(base.join("Foo.kt")).unwrap(), "x", "não sobrescreve");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn package_comes_from_the_source_set_folders() {
        assert_eq!(package_of(Path::new("proj/app/src/main/java/com/exemplo/app/Main.kt")).as_deref(), Some("com.exemplo.app"));
        assert_eq!(package_of(Path::new("proj/app/src/test/kotlin/br/com/Teste.kt")).as_deref(), Some("br.com"));
        assert_eq!(package_of(Path::new("proj/app/src/main/java/Main.kt")), None, "direto na pasta java não tem pacote");
        assert_eq!(package_of(Path::new("proj/docs/Main.kt")), None);
        assert_eq!(package_of(Path::new("proj/app/src/main/java/com/1ruim/Main.kt")), None, "pasta que não é identificador");
    }

    #[test]
    fn templates_by_extension() {
        let kt = Path::new("p/app/src/main/java/com/exemplo/Tela.kt");
        assert_eq!(template_for(kt), "package com.exemplo\n\nclass Tela\n");
        let java = Path::new("p/app/src/main/java/com/exemplo/Tela.java");
        assert_eq!(template_for(java), "package com.exemplo;\n\npublic class Tela {\n}\n");
        assert_eq!(template_for(Path::new("solto/Util.kt")), "class Util\n", "sem pacote conhecido");
        assert_eq!(template_for(Path::new("solto/meu-arquivo.kt")), "", "nome que não é identificador não vira classe");
        assert!(template_for(Path::new("res/layout/main.xml")).starts_with("<?xml"));
        assert_eq!(template_for(Path::new("notas.txt")), "");
    }

    #[test]
    fn creates_nested_files_and_folders() {
        let base = tmp("create");
        let f = create_file(&base, "app/src/main/java/com/novo/Tela.kt").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "package com.novo\n\nclass Tela\n");
        let d = create_dir(&base, "docs/imagens").unwrap();
        assert!(d.is_dir());
        assert!(create_dir(&base, "docs/imagens").is_err(), "pasta repetida");
        let _ = fs::remove_dir_all(base);
    }
}
