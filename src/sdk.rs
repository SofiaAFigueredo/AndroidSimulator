//! SDK próprio do app: download verificado, extração e registro do que está instalado.
//! Nada aqui lê ou escreve na instalação do Android Studio.

use crate::catalog::Package;
use sha1::{Digest, Sha1};
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
    let dir = root.join(pkg.extract_dir());
    let target = match pkg.path.split(';').next_back() {
        Some(last) if pkg.path.contains(';') => dir.join(last),
        _ => root.join(&pkg.path),
    };
    if target.starts_with(root) && target != root && target.exists() {
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
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(io)?;
        done += n as u64;
        emit(Event::Downloaded(done));
    }
    drop(out);

    emit(Event::Phase("Verificando SHA-1".into()));
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(&pkg.sha1) {
        let _ = fs::remove_file(&part);
        return Err(format!("SHA-1 não confere (esperado {}, obtido {got})", pkg.sha1));
    }
    fs::rename(&part, &zip_path).map_err(io)?;

    emit(Event::Phase("Extraindo".into()));
    let dest = root.join(pkg.extract_dir());
    fs::create_dir_all(&dest).map_err(io)?;
    extract(&zip_path, &dest)?;
    let _ = fs::remove_file(&zip_path);

    let mut map = installed(root);
    map.insert(pkg.path.clone(), pkg.revision_text());
    write_installed(root, &map).map_err(io)?;
    Ok(())
}

fn extract(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(zip_path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name rejeita caminhos que escapam do destino (zip-slip).
        let Some(rel) = entry.enclosed_name() else { continue };
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
        assert!(cat.tool(crate::catalog::Kind::PlatformTools).is_some(), "{warnings:?}");
        let pkg = cat.tool(crate::catalog::Kind::PlatformTools).unwrap();
        let root = std::env::temp_dir().join(format!("andsim-test-{}", std::process::id()));
        install(&root, pkg, &mut |_| {}).expect("install");
        assert!(root.join("platform-tools").join(if cfg!(windows) { "adb.exe" } else { "adb" }).exists());
        assert_eq!(installed(&root).get("platform-tools").map(String::as_str), Some(pkg.revision_text().as_str()));
        uninstall(&root, pkg).expect("uninstall");
        assert!(!root.join("platform-tools").exists());
        assert!(installed(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
