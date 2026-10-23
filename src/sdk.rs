//! SDK próprio do app: download verificado, extração e registro do que está instalado.
//! Nada aqui lê ou escreve na instalação do Android Studio.

use crate::catalog::Package;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub enum Event {
    Phase(String),
    /// Bytes baixados até agora.
    Downloaded(u64),
    Finished(Result<(), String>),
}

pub fn default_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AndroidSimulator")
        .join("sdk")
}

fn installed_file(root: &Path) -> PathBuf {
    root.join("installed.txt")
}

fn licenses_file(root: &Path) -> PathBuf {
    root.join("licenses").join("accepted.txt")
}

/// `path do pacote -> revisão` dos pacotes instalados.
pub fn installed(root: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(installed_file(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(p, r)| (p.to_owned(), r.to_owned()))
        .collect()
}

fn write_installed(root: &Path, map: &BTreeMap<String, String>) -> std::io::Result<()> {
    let body: String = map.iter().map(|(p, r)| format!("{p}\t{r}\n")).collect();
    fs::create_dir_all(root)?;
    fs::write(installed_file(root), body)
}

pub fn accepted_licenses(root: &Path) -> BTreeSet<String> {
    fs::read_to_string(licenses_file(root))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn accept_license(root: &Path, id: &str) -> std::io::Result<()> {
    let mut set = accepted_licenses(root);
    set.insert(id.to_owned());
    let file = licenses_file(root);
    fs::create_dir_all(file.parent().expect("licenses dir"))?;
    fs::write(file, set.into_iter().map(|l| l + "\n").collect::<String>())
}

pub fn uninstall(root: &Path, pkg: &Package) -> Result<(), String> {
    let target = root.join(pkg.install_dir());
    if target != root && target.exists() {
        fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
    }
    let mut map = installed(root);
    map.remove(&pkg.path);
    write_installed(root, &map).map_err(|e| e.to_string())
}

/// Baixa, confere o SHA-1, extrai e registra. Bloqueante: rode em outra thread.
pub fn install(root: &Path, pkg: &Package, emit: &mut dyn FnMut(Event)) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let downloads = root.join(".downloads");
    fs::create_dir_all(&downloads).map_err(io)?;
    let zip_path = downloads.join(format!("{}.zip", pkg.path.replace(';', "_")));
    let part = zip_path.with_extension("zip.part");

    emit(Event::Phase(format!("Baixando {}", pkg.name)));
    let resp = ureq::get(&pkg.url).call().map_err(|e| e.to_string())?;
    let mut body = resp.into_body().into_reader();
    let mut out = File::create(&part).map_err(io)?;
    // 40 hex = SHA-1 (catálogo do Google); 64 hex = SHA-256 (JDK e Gradle).
    let sha256 = pkg.checksum.len() == 64;
    let mut sha1 = Sha1::new();
    let mut sha2 = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        if sha256 {
            sha2.update(&buf[..n]);
        } else {
            sha1.update(&buf[..n]);
        }
        out.write_all(&buf[..n]).map_err(io)?;
        done += n as u64;
        emit(Event::Downloaded(done));
    }
    drop(out);

    emit(Event::Phase(format!("Verificando {}", if sha256 { "SHA-256" } else { "SHA-1" })));
    let got: String = if sha256 {
        sha2.finalize().iter().map(|b| format!("{b:02x}")).collect()
    } else {
        sha1.finalize().iter().map(|b| format!("{b:02x}")).collect()
    };
    if !got.eq_ignore_ascii_case(&pkg.checksum) {
        let _ = fs::remove_file(&part);
        return Err(format!("hash não confere (esperado {}, obtido {got})", pkg.checksum));
    }
    fs::rename(&part, &zip_path).map_err(io)?;

    emit(Event::Phase("Extraindo".into()));
    let dest = root.join(pkg.install_dir());
    fs::create_dir_all(&dest).map_err(io)?;
    extract(&zip_path, &dest)?;
    let _ = fs::remove_file(&zip_path);

    write_package_xml(root, pkg).map_err(io)?;
    let mut map = installed(root);
    map.insert(pkg.path.clone(), pkg.revision_text());
    write_installed(root, &map).map_err(io)?;
    Ok(())
}

/// Grava o `package.xml` que o sdkmanager grava; sem ele o AGP não reconhece o pacote instalado.
fn write_package_xml(root: &Path, pkg: &Package) -> std::io::Result<()> {
    if pkg.package_xml.is_empty() {
        return Ok(());
    }
    fs::write(root.join(pkg.install_dir()).join("package.xml"), &pkg.package_xml)
}

/// Completa o `package.xml` dos pacotes instalados antes de o app gravá-lo. Só mexe nos que estão na
/// mesma revisão do catálogo, para não descrever um pacote com os dados de outra versão.
pub fn ensure_metadata(root: &Path, catalog: &crate::catalog::Catalog) {
    for (path, revision) in installed(root) {
        let Some(pkg) = catalog.packages.iter().find(|p| p.path == path) else { continue };
        let missing = !root.join(pkg.install_dir()).join("package.xml").exists();
        if missing && pkg.revision_text() == revision {
            let _ = write_package_xml(root, pkg);
        }
    }
}

/// A pasta raiz comum a todas as entradas, se houver exatamente uma.
fn common_root(zip: &mut zip::ZipArchive<File>) -> Option<PathBuf> {
    let mut root: Option<PathBuf> = None;
    for i in 0..zip.len() {
        let name = zip.by_index(i).ok()?.enclosed_name()?;
        let first = PathBuf::from(name.components().next()?.as_os_str());
        match &root {
            None => root = Some(first),
            Some(r) if *r == first => {}
            Some(_) => return None,
        }
    }
    root
}

fn extract(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(zip_path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    // Os zips do Google trazem uma única pasta raiz (`android-15/`, `x86/`…), que o
    // sdkmanager descarta: o conteúdo vai direto para a pasta do pacote.
    let strip = common_root(&mut zip);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name rejeita caminhos que escapam do destino (zip-slip).
        let Some(rel) = entry.enclosed_name() else { continue };
        let rel = match &strip {
            Some(root) => match rel.strip_prefix(root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_owned(),
                _ => continue,
            },
            None => rel,
        };
        let target = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut f = File::create(&target).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baixa o platform-tools de verdade (≈8 MB). Rode com `cargo test -- --ignored`.
    #[test]
    #[ignore = "usa a rede"]
    fn installs_platform_tools_end_to_end() {
        let (cat, warnings) = crate::catalog::fetch_all();
        assert!(cat.latest(crate::catalog::Kind::PlatformTools).is_some(), "{warnings:?}");
        let pkg = cat.latest(crate::catalog::Kind::PlatformTools).unwrap();
        let root = std::env::temp_dir().join(format!("andsim-test-{}", std::process::id()));
        install(&root, pkg, &mut |_| {}).expect("install");
        assert!(root.join("platform-tools").join(if cfg!(windows) { "adb.exe" } else { "adb" }).exists());
        assert_eq!(installed(&root).get("platform-tools").map(String::as_str), Some(pkg.revision_text().as_str()));
        uninstall(&root, pkg).expect("uninstall");
        assert!(!root.join("platform-tools").exists());
        assert!(installed(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    /// Baixa pacotes pelo caminho para o SDK do app, exigindo licença já aceita na interface.
    /// `ANDSIM_INSTALL="system-images;android-30;google_atd;x86" cargo test installs_listed -- --ignored --nocapture`
    #[test]
    #[ignore = "usa a rede"]
    fn installs_listed_packages() {
        let root = default_root();
        let wanted = std::env::var("ANDSIM_INSTALL").expect("defina ANDSIM_INSTALL");
        let (cat, _) = crate::catalog::fetch_all();
        for path in wanted.split(',') {
            let pkg = cat.packages.iter().find(|p| p.path == path).unwrap_or_else(|| panic!("{path} fora do catálogo"));
            assert!(accepted_licenses(&root).contains(&pkg.license), "licença {} não aceita; aceite pelo app", pkg.license);
            if installed(&root).contains_key(path) {
                continue;
            }
            let mut last = 0;
            install(&root, pkg, &mut |e| {
                if let Event::Downloaded(n) = e
                    && n / (100 << 20) > last
                {
                    last = n / (100 << 20);
                    println!("  {path}: {} MB", n >> 20);
                }
            })
            .expect("install");
            println!("instalado {path}");
        }
    }
}
