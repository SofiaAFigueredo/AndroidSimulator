//! JDK e Gradle como pacotes do mesmo SDK do app. O catálogo do Google não traz nenhum dos
//! dois, então eles vêm de fontes próprias, sempre com o hash conferido antes de extrair.

use crate::catalog::{Kind, Package};

pub const JDK_MAJOR: u32 = 21;
/// O AGP 9.4 exige Gradle 9.6 ou mais novo e JDK 17 ou mais novo.
pub const GRADLE_VERSION: &str = "9.8.2";

fn adoptium_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn adoptium_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "aarch64" } else { "x64" }
}

fn get(url: &str) -> Result<String, String> {
    ureq::get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())
}

/// Primeiro valor de `"chave": "texto"` depois de `from` (o JSON da Adoptium é regular o bastante
/// para dispensar um parser completo e manter o binário pequeno).
fn json_str(json: &str, from: &str, key: &str) -> Option<String> {
    let start = json.find(from)?;
    let rest = &json[start..];
    let k = rest.find(&format!("\"{key}\""))?;
    let after = &rest[k + key.len() + 2..];
    let q1 = after.find('"')?;
    let value = &after[q1 + 1..];
    Some(value[..value.find('"')?].to_owned())
}

fn json_num(json: &str, from: &str, key: &str) -> Option<u64> {
    let start = json.find(from)?;
    let rest = &json[start..];
    let k = rest.find(&format!("\"{key}\""))?;
    let after = rest[k + key.len() + 2..].trim_start_matches([':', ' ']);
    after.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

pub fn jdk_from_json(json: &str) -> Option<Package> {
    let release = json_str(json, "", "release_name")?;
    let link = json_str(json, "\"package\"", "link")?;
    let checksum = json_str(json, "\"package\"", "checksum")?;
    let size = json_num(json, "\"package\"", "size")?;
    Some(Package {
        path: format!("jdk;{JDK_MAJOR}"),
        name: format!("Eclipse Temurin {release}"),
        kind: Kind::Jdk,
        api: None,
        abi: None,
        revision: (JDK_MAJOR, 0, 0),
        size,
        url: link,
        checksum,
        license: String::new(),
        package_xml: String::new(),
    })
}

fn jdk() -> Result<Package, String> {
    let url = format!(
        "https://api.adoptium.net/v3/assets/latest/{JDK_MAJOR}/hotspot?architecture={}&image_type=jdk&os={}&vendor=eclipse",
        adoptium_arch(),
        adoptium_os()
    );
    jdk_from_json(&get(&url)?).ok_or_else(|| "resposta inesperada da Adoptium".to_owned())
}

fn gradle() -> Result<Package, String> {
    let url = format!("https://services.gradle.org/distributions/gradle-{GRADLE_VERSION}-bin.zip");
    let checksum = get(&format!("{url}.sha256"))?.trim().to_owned();
    if checksum.len() != 64 {
        return Err(format!("checksum do Gradle inválido: {checksum}"));
    }
    let size = ureq::head(&url)
        .call()
        .ok()
        .and_then(|r| r.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()))
        .unwrap_or(0);
    Ok(Package {
        path: format!("gradle;{GRADLE_VERSION}"),
        name: format!("Gradle {GRADLE_VERSION}"),
        kind: Kind::Gradle,
        api: None,
        abi: None,
        revision: (9, 8, 2),
        size,
        url,
        checksum,
        license: String::new(),
        package_xml: String::new(),
    })
}

/// Versão do Eclipse JDT LS usada. A 1.62 em diante exige Java 25 (`JavaSE-25` no bundle), e o JDK do
/// app é o 21, então esta é a última milestone compatível. Para subir de versão, suba o JDK junto.
pub const JDTLS_VERSION: &str = "1.61.0";

/// Servidor de linguagem Java: Eclipse JDT LS (EPL-2.0), com o SHA-256 que o Eclipse publica ao lado
/// do arquivo.
fn jdtls() -> Result<Package, String> {
    let base = format!("https://download.eclipse.org/jdtls/milestones/{JDTLS_VERSION}");
    let name = get(&format!("{base}/latest.txt"))?.trim().to_owned();
    if !name.starts_with("jdt-language-server-") || !name.ends_with(".tar.gz") || name.contains('/') {
        return Err(format!("nome de arquivo inesperado em latest.txt: {name:?}"));
    }
    let url = format!("{base}/{name}");
    let checksum = get(&format!("{url}.sha256"))?.split_whitespace().next().unwrap_or_default().to_owned();
    if checksum.len() != 64 {
        return Err(format!("checksum do jdtls inválido: {checksum:?}"));
    }
    let size = ureq::head(&url).call().ok().and_then(|r| r.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok())).unwrap_or(0);
    let version: Vec<u32> = name.trim_start_matches("jdt-language-server-").split(['.', '-']).filter_map(|n| n.parse().ok()).collect();
    Ok(Package {
        path: "lsp;jdtls".into(),
        name: "Autocomplete Java (Eclipse JDT LS)".into(),
        kind: Kind::JavaLs,
        api: None,
        abi: None,
        revision: (*version.first().unwrap_or(&0), *version.get(1).unwrap_or(&0), *version.get(2).unwrap_or(&0)),
        size,
        url,
        checksum,
        license: String::new(),
        package_xml: String::new(),
    })
}

/// Servidor de linguagem Kotlin (fwcd, MIT). O GitHub não publica hash desse arquivo, então o
/// SHA-256 fica fixado aqui, medido no download da versão 1.3.13: se o arquivo mudar, o app recusa.
pub const KOTLIN_LS_VERSION: &str = "1.3.13";
pub const KOTLIN_LS_SHA256: &str = "4fe7d71d087b307c7869036171bd9d8c6a4284cd7c25b89098b0a24eb2d9b6d2";

fn kotlin_ls() -> Package {
    Package {
        path: "lsp;kotlin".into(),
        name: format!("Autocomplete Kotlin (kotlin-language-server {KOTLIN_LS_VERSION})"),
        kind: Kind::KotlinLs,
        api: None,
        abi: None,
        revision: (1, 3, 13),
        size: 87_291_855,
        url: format!("https://github.com/fwcd/kotlin-language-server/releases/download/{KOTLIN_LS_VERSION}/server.zip"),
        checksum: KOTLIN_LS_SHA256.into(),
        license: String::new(),
        package_xml: String::new(),
    }
}

pub fn fetch_packages() -> (Vec<Package>, Vec<String>) {
    let mut pkgs = Vec::new();
    let mut warnings = Vec::new();
    pkgs.push(kotlin_ls());
    for (what, r) in [("JDK", jdk()), ("Gradle", gradle()), ("Autocomplete Java", jdtls())] {
        match r {
            Ok(p) => pkgs.push(p),
            Err(e) => warnings.push(format!("{what}: {e}")),
        }
    }
    (pkgs, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[{"binary":{"architecture":"x64","image_type":"jdk","installer":{"checksum":"zzz","link":"https://h/instalador.msi","size":1},"package":{"checksum":"f9d6","link":"https://h/jdk.zip","name":"jdk.zip","size":205520896}},"release_name":"jdk-21.0.12.1+1"}]"#;

    #[test]
    fn reads_the_package_block_and_not_the_installer() {
        let p = jdk_from_json(SAMPLE).unwrap();
        assert_eq!(p.url, "https://h/jdk.zip");
        assert_eq!(p.checksum, "f9d6");
        assert_eq!(p.size, 205_520_896);
        assert_eq!(p.name, "Eclipse Temurin jdk-21.0.12.1+1");
        assert_eq!(p.install_dir(), std::path::PathBuf::from("jdk/21"));
    }

    #[test]
    fn kotlin_server_is_pinned_by_hash() {
        let p = kotlin_ls();
        assert_eq!(p.checksum.len(), 64);
        assert!(p.url.contains(KOTLIN_LS_VERSION) && p.url.ends_with("server.zip"));
        assert_eq!(p.install_dir(), std::path::PathBuf::from("lsp/kotlin"));
    }

    #[test]
    fn garbage_gives_none() {
        assert!(jdk_from_json("{}").is_none());
    }
}
