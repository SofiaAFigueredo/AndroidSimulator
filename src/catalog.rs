//! Lê o catálogo oficial de pacotes do Google (formato `repository2`/`sys-img2`).

use std::collections::HashMap;

pub const BASE: &str = "https://dl.google.com/android/repository";

/// Catálogos consultados. Os de imagens de sistema são opcionais: se um falhar,
/// os outros continuam valendo.
pub const SOURCES: &[&str] = &[
    "repository2-3.xml",
    "sys-img/android/sys-img2-3.xml",
    "sys-img/google_apis/sys-img2-3.xml",
    "sys-img/google_apis_playstore/sys-img2-3.xml",
    "sys-img/google_atd/sys-img2-3.xml",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Emulator,
    PlatformTools,
    SystemImage,
}

#[derive(Clone, Debug)]
pub struct Package {
    pub path: String,
    pub name: String,
    pub kind: Kind,
    pub api: Option<String>,
    pub abi: Option<String>,
    pub revision: (u32, u32, u32),
    pub size: u64,
    pub url: String,
    pub sha1: String,
    pub license: String,
}

#[derive(Default, Debug)]
pub struct Catalog {
    pub packages: Vec<Package>,
    pub licenses: HashMap<String, String>,
}

impl Package {
    pub fn revision_text(&self) -> String {
        let (a, b, c) = self.revision;
        format!("{a}.{b}.{c}")
    }

    /// Diretório (relativo ao SDK) em que o zip é extraído. Os zips de imagem
    /// trazem a pasta da ABI na raiz; os de emulator/platform-tools, a própria pasta.
    pub fn extract_dir(&self) -> std::path::PathBuf {
        let parts: Vec<&str> = self.path.split(';').collect();
        let keep = if parts.len() > 1 { parts.len() - 1 } else { 0 };
        parts[..keep].iter().collect()
    }
}

impl Catalog {
    pub fn merge(&mut self, other: Catalog) {
        self.packages.extend(other.packages);
        self.licenses.extend(other.licenses);
    }

    /// Imagens que o emulador deste computador consegue rodar, da menor para a maior.
    pub fn images(&self) -> Vec<&Package> {
        let mut v: Vec<&Package> = self
            .packages
            .iter()
            .filter(|p| p.kind == Kind::SystemImage)
            .filter(|p| p.abi.as_deref().is_some_and(host_abi_ok))
            .collect();
        v.sort_by_key(|p| p.size);
        v
    }

    pub fn tool(&self, kind: Kind) -> Option<&Package> {
        self.packages.iter().find(|p| p.kind == kind)
    }
}

fn host_abi_ok(abi: &str) -> bool {
    if cfg!(target_arch = "aarch64") {
        abi == "arm64-v8a"
    } else {
        abi == "x86_64" || abi == "x86"
    }
}

fn host_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macosx"
    } else {
        "linux"
    }
}

fn host_arch_ok(arch: &str) -> bool {
    match arch {
        "aarch64" => cfg!(target_arch = "aarch64"),
        "x64" | "x86_64" => cfg!(target_arch = "x86_64"),
        _ => false,
    }
}

type Node<'a, 'b> = roxmltree::Node<'a, 'b>;

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn text(n: Node, name: &str) -> Option<String> {
    child(n, name).and_then(|c| c.text()).map(|t| t.trim().to_owned())
}

/// `base_dir` é a URL da pasta do XML; as `<url>` dos arquivos são relativas a ela.
pub fn parse(xml: &str, base_dir: &str) -> Result<Catalog, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let mut cat = Catalog::default();
    let mut stable: Vec<String> = Vec::new();

    for n in root.children().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "license" => {
                if let (Some(id), Some(t)) = (n.attribute("id"), n.text()) {
                    cat.licenses.insert(id.to_owned(), t.trim().to_owned());
                }
            }
            "channel" => {
                if n.text().is_some_and(|t| t.trim().eq_ignore_ascii_case("stable")) {
                    stable.extend(n.attribute("id").map(str::to_owned));
                }
            }
            _ => {}
        }
    }

    let mut best: HashMap<String, Package> = HashMap::new();
    for n in root.children().filter(|n| n.is_element() && n.tag_name().name() == "remotePackage") {
        let Some(path) = n.attribute("path") else { continue };
        if let Some(ch) = child(n, "channelRef").and_then(|c| c.attribute("ref"))
            && !stable.iter().any(|s| s == ch)
        {
            continue;
        }
        let kind = if path == "emulator" {
            Kind::Emulator
        } else if path == "platform-tools" {
            Kind::PlatformTools
        } else if path.starts_with("system-images;") {
            Kind::SystemImage
        } else {
            continue;
        };
        let Some(archives) = child(n, "archives") else { continue };
        let Some(complete) = archives
            .children()
            .filter(|a| a.is_element() && a.tag_name().name() == "archive")
            .find(|a| {
                text(*a, "host-os").is_none_or(|o| o == host_os())
                    && text(*a, "host-arch").is_none_or(|h| host_arch_ok(&h))
            })
            .and_then(|a| child(a, "complete"))
        else {
            continue;
        };
        let (Some(file), Some(size), Some(sha1)) = (
            text(complete, "url"),
            text(complete, "size").and_then(|s| s.parse::<u64>().ok()),
            text(complete, "checksum"),
        ) else {
            continue;
        };
        let rev = child(n, "revision");
        let num = |k: &str| rev.and_then(|r| text(r, k)).and_then(|s| s.parse().ok()).unwrap_or(0);
        let details = child(n, "type-details");
        let pkg = Package {
            path: path.to_owned(),
            name: text(n, "display-name").unwrap_or_else(|| path.to_owned()),
            kind,
            api: details.and_then(|d| text(d, "api-level")),
            abi: details.and_then(|d| text(d, "abi")),
            revision: (num("major"), num("minor"), num("micro")),
            size,
            url: format!("{base_dir}/{file}"),
            sha1,
            license: child(n, "uses-license")
                .and_then(|l| l.attribute("ref"))
                .unwrap_or_default()
                .to_owned(),
        };
        match best.get(path) {
            Some(old) if old.revision >= pkg.revision => {}
            _ => {
                best.insert(path.to_owned(), pkg);
            }
        }
    }
    cat.packages = best.into_values().collect();
    Ok(cat)
}

/// Baixa e junta todos os catálogos. Retorna também os avisos dos que falharam.
pub fn fetch_all() -> (Catalog, Vec<String>) {
    let mut cat = Catalog::default();
    let mut warnings = Vec::new();
    for src in SOURCES {
        let url = format!("{BASE}/{src}");
        let base_dir = url.rsplit_once('/').map(|(d, _)| d.to_owned()).unwrap_or_default();
        let body = ureq::get(&url)
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string()));
        match body.and_then(|b| parse(&b, &base_dir)) {
            Ok(c) => cat.merge(c),
            Err(e) => warnings.push(format!("{src}: {e}")),
        }
    }
    (cat, warnings)
}

pub fn human_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<sys:sdk-sys-img xmlns:sys="x" xmlns:xsi="y">
 <license id="lic-a" type="text">Termos de teste</license>
 <channel id="channel-0">Stable</channel>
 <channel id="channel-3">Canary</channel>
 <remotePackage path="system-images;android-10;default;x86">
  <type-details><api-level>10</api-level><tag><id>default</id><display>Default</display></tag><abi>x86</abi></type-details>
  <revision><major>5</major></revision>
  <display-name>Intel x86 Atom System Image</display-name>
  <uses-license ref="lic-a"/><channelRef ref="channel-0"/>
  <archives><archive><complete><size>100</size><checksum type="sha1">abc</checksum><url>x86-10.zip</url></complete></archive></archives>
 </remotePackage>
 <remotePackage path="system-images;android-10;default;x86">
  <type-details><api-level>10</api-level><tag><id>default</id></tag><abi>x86</abi></type-details>
  <revision><major>9</major></revision><channelRef ref="channel-3"/>
  <archives><archive><complete><size>1</size><checksum type="sha1">zzz</checksum><url>canary.zip</url></complete></archive></archives>
 </remotePackage>
 <remotePackage path="platforms;android-10"><archives/></remotePackage>
</sys:sdk-sys-img>"#;

    #[test]
    fn parses_stable_image_and_skips_the_rest() {
        let c = parse(XML, "https://h/dir").unwrap();
        assert_eq!(c.packages.len(), 1);
        let p = &c.packages[0];
        assert_eq!(p.kind, Kind::SystemImage);
        assert_eq!(p.api.as_deref(), Some("10"));
        assert_eq!(p.url, "https://h/dir/x86-10.zip");
        assert_eq!(p.revision_text(), "5.0.0");
        assert_eq!(p.license, "lic-a");
        assert_eq!(c.licenses["lic-a"], "Termos de teste");
    }

    #[test]
    fn extract_dir_drops_last_segment_for_images_only() {
        let c = parse(XML, "https://h/dir").unwrap();
        assert_eq!(
            c.packages[0].extract_dir(),
            std::path::PathBuf::from("system-images/android-10/default")
        );
        let mut tool = c.packages[0].clone();
        tool.path = "emulator".into();
        assert_eq!(tool.extract_dir(), std::path::PathBuf::new());
    }
}
