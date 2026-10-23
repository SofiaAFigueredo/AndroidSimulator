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

pub fn fetch_packages() -> (Vec<Package>, Vec<String>) {
    let mut pkgs = Vec::new();
    let mut warnings = Vec::new();
    for (what, r) in [("JDK", jdk()), ("Gradle", gradle())] {
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
    fn garbage_gives_none() {
        assert!(jdk_from_json("{}").is_none());
    }
}
