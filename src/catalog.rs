//! Lê o catálogo oficial de pacotes do Google (formato `repository2`/`sys-img2`).

use std::collections::HashMap;

/// Maior API que o plugin do Android para Gradle do modelo suporta (AGP 9.4).
pub const MAX_PLATFORM_API: f32 = 37.0;

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
    Platform,
    BuildTools,
    CmdlineTools,
    Sources,
    Ndk,
    Cmake,
    Jdk,
    Gradle,
}

/// Perfil de instalação: o que baixar de uma vez, sempre na versão mais recente.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    /// Só o necessário para rodar um app: emulador, adb e a imagem escolhida.
    Compact,
    /// Rodar e compilar: a Compacta mais plataforma, build-tools, cmdline-tools,
    /// código-fonte do Android, NDK e CMake.
    Complete,
}

impl Profile {
    pub fn kinds(self) -> &'static [Kind] {
        match self {
            Profile::Compact => &[Kind::Emulator, Kind::PlatformTools],
            Profile::Complete => &[
                Kind::Emulator,
                Kind::PlatformTools,
                Kind::Platform,
                Kind::BuildTools,
                Kind::CmdlineTools,
                Kind::Sources,
                Kind::Ndk,
                Kind::Cmake,
                Kind::Jdk,
                Kind::Gradle,
            ],
        }
    }
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
    /// SHA-1 (40 hex, catálogo do Google) ou SHA-256 (64 hex, JDK e Gradle).
    pub checksum: String,
    pub license: String,
    /// Conteúdo do `package.xml` que o sdkmanager grava na pasta do pacote (vazio fora do catálogo do Google).
    pub package_xml: String,
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

    /// Pasta do pacote dentro do SDK (`build-tools;35.0.0` vira `build-tools/35.0.0`).
    /// O zip traz uma pasta raiz com outro nome, que é descartada na extração.
    pub fn install_dir(&self) -> std::path::PathBuf {
        self.path.split(';').collect()
    }

    fn api_number(&self) -> Option<f32> {
        self.path.rsplit_once("android-").and_then(|(_, n)| n.parse().ok())
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

    /// A versão estável mais recente de cada tipo de pacote.
    pub fn latest(&self, kind: Kind) -> Option<&Package> {
        let of_kind = self.packages.iter().filter(|p| p.kind == kind);
        match kind {
            Kind::Platform => of_kind
                .filter_map(|p| p.api_number().filter(|n| n.fract() == 0.0 && *n <= MAX_PLATFORM_API).map(|n| (n, p)))
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, p)| p),
            Kind::Sources => {
                let api = self.latest(Kind::Platform)?.api_number()?;
                of_kind.into_iter().find(|p| p.api_number() == Some(api))
            }
            Kind::BuildTools => of_kind.filter(|p| !p.path.contains("rc")).max_by_key(|p| p.revision),
            _ => of_kind.max_by_key(|p| p.revision),
        }
    }

    /// O que baixar para o perfil e a imagem escolhida, sem repetir o que já está instalado.
    pub fn plan(&self, profile: Profile, image: Option<&str>, installed: &dyn Fn(&str) -> bool) -> Vec<Package> {
        let mut v: Vec<Package> = profile
            .kinds()
            .iter()
            .filter_map(|k| self.latest(*k))
            .chain(image.and_then(|path| self.packages.iter().find(|p| p.path == path)))
            .filter(|p| !installed(&p.path))
            .cloned()
            .collect();
        v.dedup_by(|a, b| a.path == b.path);
        v
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

/// `package.xml` de um pacote instalado: o mesmo `remotePackage` do catálogo, como `localPackage`,
/// sem `archives` e `channelRef`, dentro de um `repository` com os namespaces do catálogo e a licença.
/// O AGP e o sdkmanager só reconhecem como instalado o pacote que tem esse arquivo.
fn local_xml(src: &str, root: Node, node: Node, license_xml: &HashMap<String, String>) -> String {
    const COMMON: &str = "http://schemas.android.com/repository/android/common/02";
    let prefix = root
        .namespaces()
        .find(|n| n.uri() == COMMON)
        .and_then(|n| n.name())
        .unwrap_or("ns2")
        .to_owned();
    let mut decls = String::new();
    for ns in root.namespaces() {
        match ns.name() {
            Some(p) => decls += &format!(" xmlns:{p}=\"{}\"", ns.uri()),
            None => decls += &format!(" xmlns=\"{}\"", ns.uri()),
        }
    }
    if !root.namespaces().any(|n| n.uri() == COMMON) {
        decls += &format!(" xmlns:{prefix}=\"{COMMON}\"");
    }
    let base = node.range().start;
    let mut cuts: Vec<std::ops::Range<usize>> = node
        .children()
        .filter(|c| c.is_element() && matches!(c.tag_name().name(), "archives" | "channelRef"))
        .map(|c| c.range())
        .collect();
    cuts.sort_by_key(|r| std::cmp::Reverse(r.start));
    let mut body = src[node.range()].to_owned();
    for r in cuts {
        body.replace_range(r.start - base..r.end - base, "");
    }
    let body = body.replacen("<remotePackage", "<localPackage", 1);
    let body = match body.rfind("</remotePackage>") {
        Some(i) => format!("{}</localPackage>", &body[..i]),
        None => body,
    };
    let license = child(node, "uses-license")
        .and_then(|l| l.attribute("ref"))
        .and_then(|id| license_xml.get(id))
        .cloned()
        .unwrap_or_default();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><{prefix}:repository{decls}>{license}{body}</{prefix}:repository>")
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
    let mut license_xml: HashMap<String, String> = HashMap::new();

    for n in root.children().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "license" => {
                if let (Some(id), Some(t)) = (n.attribute("id"), n.text()) {
                    license_xml.insert(id.to_owned(), xml[n.range()].to_owned());
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
        let kind = match path {
            "emulator" => Kind::Emulator,
            "platform-tools" => Kind::PlatformTools,
            p if p.starts_with("system-images;") => Kind::SystemImage,
            p if p.starts_with("platforms;android-") => Kind::Platform,
            p if p.starts_with("build-tools;") => Kind::BuildTools,
            p if p.starts_with("cmdline-tools;") => Kind::CmdlineTools,
            p if p.starts_with("sources;android-") => Kind::Sources,
            p if p.starts_with("ndk;") => Kind::Ndk,
            p if p.starts_with("cmake;") => Kind::Cmake,
            _ => continue,
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
        let (Some(file), Some(size), Some(checksum)) = (
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
            checksum,
            license: child(n, "uses-license")
                .and_then(|l| l.attribute("ref"))
                .unwrap_or_default()
                .to_owned(),
            package_xml: local_xml(xml, root, n, &license_xml),
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
    let (tools, tool_warnings) = crate::toolchain::fetch_packages();
    cat.packages.extend(tools);
    warnings.extend(tool_warnings);
    (cat, warnings)
}

/// API inteira de um pacote `platforms;android-37.0` ou `platforms;android-36`; `None` para
/// versões com extensão (`37.2`) ou prévias.
pub fn platform_api(path: &str) -> Option<u32> {
    let n: f32 = path.strip_prefix("platforms;android-")?.parse().ok()?;
    (n.fract() == 0.0 && n <= MAX_PLATFORM_API).then_some(n as u32)
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
    fn builds_a_local_package_xml() {
        let c = parse(XML, "https://h/dir").unwrap();
        let x = &c.packages[0].package_xml;
        assert!(x.contains("<localPackage path=\"system-images;android-10;default;x86\""), "{x}");
        assert!(!x.contains("archives") && !x.contains("channelRef") && !x.contains("remotePackage"), "{x}");
        assert!(x.contains("Termos de teste"));
        let doc = roxmltree::Document::parse(x).expect("package.xml deve ser XML válido");
        assert_eq!(doc.root_element().tag_name().name(), "repository");
    }

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

    fn pkg(path: &str, kind: Kind, rev: u32, size: u64) -> Package {
        Package {
            path: path.into(),
            name: path.into(),
            kind,
            api: None,
            abi: None,
            revision: (rev, 0, 0),
            size,
            url: String::new(),
            checksum: String::new(),
            license: String::new(),
            package_xml: String::new(),
        }
    }

    fn sample() -> Catalog {
        Catalog {
            packages: vec![
                pkg("emulator", Kind::Emulator, 37, 400),
                pkg("platform-tools", Kind::PlatformTools, 37, 8),
                pkg("platforms;android-34", Kind::Platform, 3, 60),
                pkg("platforms;android-36", Kind::Platform, 2, 70),
                pkg("platforms;android-35", Kind::Platform, 2, 65),
                pkg("platforms;android-37.2", Kind::Platform, 2, 70),
                pkg("platforms;android-38", Kind::Platform, 2, 70),
                pkg("sources;android-34", Kind::Sources, 1, 40),
                pkg("sources;android-36", Kind::Sources, 1, 45),
                pkg("build-tools;35.0.0", Kind::BuildTools, 35, 55),
                pkg("build-tools;36.0.0", Kind::BuildTools, 36, 60),
                pkg("build-tools;37.0.0-rc1", Kind::BuildTools, 37, 61),
                pkg("ndk;27.0.1", Kind::Ndk, 27, 900),
                pkg("ndk;28.0.1", Kind::Ndk, 28, 1000),
                pkg("cmake;3.22.1", Kind::Cmake, 3, 20),
                pkg("cmdline-tools;19.0", Kind::CmdlineTools, 19, 150),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn install_dir_mirrors_the_package_path() {
        assert_eq!(pkg("emulator", Kind::Emulator, 1, 1).install_dir(), std::path::PathBuf::from("emulator"));
        assert_eq!(
            pkg("system-images;android-10;default;x86", Kind::SystemImage, 1, 1).install_dir(),
            std::path::PathBuf::from("system-images/android-10/default/x86")
        );
        assert_eq!(
            pkg("build-tools;35.0.0", Kind::BuildTools, 1, 1).install_dir(),
            std::path::PathBuf::from("build-tools/35.0.0")
        );
    }

    #[test]
    fn latest_picks_newest_stable_and_matching_sources() {
        let c = sample();
        assert_eq!(c.latest(Kind::Platform).unwrap().path, "platforms;android-36");
        assert_eq!(c.latest(Kind::Sources).unwrap().path, "sources;android-36");
        assert_eq!(c.latest(Kind::BuildTools).unwrap().path, "build-tools;36.0.0");
        assert_eq!(c.latest(Kind::Ndk).unwrap().path, "ndk;28.0.1");
    }

    #[test]
    fn compact_is_a_subset_of_complete_and_skips_installed() {
        let c = sample();
        let none = |_: &str| false;
        let compact: Vec<String> = c.plan(Profile::Compact, None, &none).into_iter().map(|p| p.path).collect();
        let complete: Vec<String> = c.plan(Profile::Complete, None, &none).into_iter().map(|p| p.path).collect();
        assert_eq!(compact, ["emulator", "platform-tools"]);
        assert!(complete.contains(&"platforms;android-36".to_string()));
        assert!(complete.contains(&"build-tools;36.0.0".to_string()));
        let with_image = c.plan(Profile::Compact, Some("platforms;android-34"), &none);
        assert_eq!(with_image.last().unwrap().path, "platforms;android-34");
        assert!(compact.iter().all(|p| complete.contains(p)));
        assert!(complete.contains(&"ndk;28.0.1".to_string()) && !compact.contains(&"ndk;28.0.1".to_string()));
        let have = |p: &str| p == "emulator";
        assert!(!c.plan(Profile::Compact, None, &have).iter().any(|p| p.path == "emulator"));
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// Confere os perfis contra o catálogo real. Rode com `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore = "usa a rede"]
    fn profiles_on_the_real_catalog() {
        let (cat, warnings) = fetch_all();
        assert!(warnings.is_empty(), "{warnings:?}");
        let image = cat.images().first().map(|p| p.path.clone());
        for profile in [Profile::Compact, Profile::Complete] {
            let plan = cat.plan(profile, image.as_deref(), &|_| false);
            let total: u64 = plan.iter().map(|p| p.size).sum();
            println!("{profile:?}: {} pacotes, {}", plan.len(), human_size(total));
            for p in &plan {
                println!("  {:<42} {}", p.path, human_size(p.size));
            }
            assert!(plan.iter().any(|p| p.kind == Kind::Emulator));
        }
        let complete = cat.plan(Profile::Complete, None, &|_| false);
        assert!(complete.iter().any(|p| p.kind == Kind::Ndk && !p.path.contains("rc")));
        assert!(complete.iter().any(|p| p.kind == Kind::Sources));
    }
}

#[cfg(test)]
mod platform_api_tests {
    use super::*;

    #[test]
    fn reads_whole_apis_only() {
        assert_eq!(platform_api("platforms;android-37.0"), Some(37));
        assert_eq!(platform_api("platforms;android-36"), Some(36));
        assert_eq!(platform_api("platforms;android-37.2"), None);
        assert_eq!(platform_api("platforms;android-38"), None);
        assert_eq!(platform_api("platforms;android-Baklava"), None);
        assert_eq!(platform_api("build-tools;36.0.0"), None);
    }
}
