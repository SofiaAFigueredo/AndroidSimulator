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
    let ext = if pkg.url.ends_with(".tar.gz") { "tar.gz" } else { "zip" };
    let zip_path = downloads.join(format!("{}.{ext}", pkg.path.replace(';', "_")));
    let part = PathBuf::from(format!("{}.part", zip_path.display()));

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
    // Reinstalação (atualização ou troca de versão): começa de uma pasta limpa, sem restos da versão anterior.
    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(io)?;
    }
    fs::create_dir_all(&dest).map_err(io)?;
    if ext == "tar.gz" {
        extract_tar_gz(&zip_path, &dest)?;
    } else {
        extract(&zip_path, &dest)?;
    }
    let _ = fs::remove_file(&zip_path);

    write_package_xml(root, pkg).map_err(io)?;
    let mut map = installed(root);
    map.insert(pkg.path.clone(), pkg.revision_text());
    write_installed(root, &map).map_err(io)?;
    Ok(())
}

/// Extrai um `.tar.gz` com o `tar` do sistema (no Windows, o `tar.exe` que vem com o sistema; o caminho
/// absoluto evita pegar um GNU tar do Git, que lê `C:\\x` como "máquina C"). Sem dependências extras.
fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("tar.exe");
    #[cfg(not(windows))]
    let tar = PathBuf::from("tar");
    let mut cmd = std::process::Command::new(tar);
    cmd.arg("-xzf").arg(archive).arg("-C").arg(dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().map_err(|e| format!("não consegui rodar o tar: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("o tar falhou: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
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

/// Arquivos a partir deste tamanho são gravados como esparsos.
const SPARSE_MIN: u64 = 1 << 20;
const BLOCK: usize = 64 * 1024;

#[cfg(windows)]
fn mark_sparse(f: &File) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn DeviceIoControl(
            h: *mut c_void,
            code: u32,
            input: *const c_void,
            input_len: u32,
            output: *mut c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    // Se o sistema de arquivos não suporta (FAT), a chamada falha e o arquivo sai inteiro, só maior.
    unsafe {
        DeviceIoControl(
            f.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(not(windows))]
fn mark_sparse(_: &File) {}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Copia `r` para `f` pulando os blocos só de zeros, que viram buracos no arquivo. As imagens do
/// Android são em grande parte zeros (a API 30 baixa 614 MB e vira 3 GB; a API 10 gera 2 GB de
/// `userdata.img` quase vazio), então isso devolve vários GB de disco.
pub fn write_sparse(r: &mut dyn Read, f: &mut File) -> std::io::Result<u64> {
    use std::io::{Seek, SeekFrom};
    mark_sparse(f);
    let mut buf = vec![0u8; BLOCK];
    let mut total = 0u64;
    loop {
        let n = read_full(r, &mut buf)?;
        if n == 0 {
            break;
        }
        if buf[..n].iter().all(|b| *b == 0) {
            f.seek(SeekFrom::Current(n as i64))?;
        } else {
            f.write_all(&buf[..n])?;
        }
        total += n as u64;
        if n < BLOCK {
            break;
        }
    }
    f.set_len(total)?;
    Ok(total)
}

/// Espaço realmente alocado em disco (menor que o tamanho lógico em arquivos esparsos).
pub fn allocated_size(path: &Path) -> std::io::Result<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        unsafe extern "system" {
            fn GetCompressedFileSizeW(name: *const u16, high: *mut u32) -> u32;
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut high = 0u32;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == u32::MAX && std::io::Error::last_os_error().raw_os_error() != Some(0) {
            return Err(std::io::Error::last_os_error());
        }
        Ok(((high as u64) << 32) | low as u64)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(fs::metadata(path)?.blocks() * 512)
    }
}

/// Regrava um arquivo já extraído como esparso. Devolve `(antes, depois)` em bytes alocados; só
/// substitui o original quando de fato ocupa menos.
pub fn compact_file(path: &Path) -> std::io::Result<(u64, u64)> {
    let before = allocated_size(path)?;
    let tmp = path.with_extension("compacting");
    let mut src = File::open(path)?;
    let mut dst = File::create(&tmp)?;
    write_sparse(&mut src, &mut dst)?;
    drop((src, dst));
    if allocated_size(&tmp)? < before {
        fs::rename(&tmp, path)?;
        // Mede de novo já no destino: o número do arquivo temporário não é o que fica em disco.
        Ok((before, allocated_size(path)?))
    } else {
        fs::remove_file(&tmp)?;
        Ok((before, before))
    }
}

/// Compacta as imagens de sistema já instaladas. Devolve quantos bytes foram liberados.
pub fn compact_installed(root: &Path, on_line: &mut dyn FnMut(&str)) -> u64 {
    let mut saved = 0;
    let mut stack = vec![root.join("system-images")];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if e.metadata().is_ok_and(|m| m.len() >= SPARSE_MIN) {
                match compact_file(&p) {
                    Ok((b, a)) if a < b => {
                        saved += b - a;
                        on_line(&format!("{}: {} MB liberados", p.file_name().unwrap_or_default().to_string_lossy(), (b - a) >> 20));
                    }
                    Ok(_) => {}
                    Err(e) => on_line(&format!("{}: {e}", p.display())),
                }
            }
        }
    }
    saved
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
        if entry.size() >= SPARSE_MIN {
            write_sparse(&mut entry, &mut f).map_err(|e| e.to_string())?;
        } else {
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
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
    fn extracts_a_tar_gz_with_the_system_tar() {
        let base = std::env::temp_dir().join(format!("andsim-tar-{}", std::process::id()));
        let (src, out) = (base.join("src"), base.join("out"));
        fs::create_dir_all(src.join("plugins")).unwrap();
        fs::create_dir_all(&out).unwrap();
        fs::write(src.join("plugins").join("a.jar"), b"conteudo").unwrap();
        fs::write(src.join("config.ini"), b"x=1").unwrap();
        let archive = base.join("pacote.tar.gz");
        let tar_exe = if cfg!(windows) {
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("tar.exe")
        } else {
            PathBuf::from("tar")
        };
        let ok = std::process::Command::new(tar_exe).arg("-czf").arg(&archive).arg("-C").arg(&src).arg(".").status().unwrap();
        assert!(ok.success());
        extract_tar_gz(&archive, &out).unwrap();
        assert_eq!(fs::read(out.join("plugins").join("a.jar")).unwrap(), b"conteudo");
        assert_eq!(fs::read(out.join("config.ini")).unwrap(), b"x=1");
        assert!(extract_tar_gz(&base.join("nao-existe.tar.gz"), &out).is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn sparse_copy_keeps_the_bytes_and_frees_the_zero_blocks() {
        let mut data = vec![7u8; 1 << 20];
        data.extend(vec![0u8; 6 << 20]);
        data.extend((0..(1 << 20)).map(|i| (i % 251) as u8 + 1));
        data.extend(vec![0u8; 10]); // cauda de zeros que não fecha um bloco
        let path = std::env::temp_dir().join(format!("andsim-sparse-{}", std::process::id()));
        let mut f = File::create(&path).unwrap();
        let n = write_sparse(&mut &data[..], &mut f).unwrap();
        drop(f);
        assert_eq!(n, data.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), data, "o conteúdo precisa ser idêntico");
        let allocated = allocated_size(&path).unwrap();
        assert!(allocated < (data.len() as u64) / 2, "alocado {allocated} de {}", data.len());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn compact_file_reports_what_is_really_left_on_disk() {
        let path = std::env::temp_dir().join(format!("andsim-compact-{}", std::process::id()));
        let mut data = vec![0u8; 8 << 20];
        data[..4096].fill(9);
        fs::write(&path, &data).unwrap();
        let (before, after) = compact_file(&path).unwrap();
        assert!(after < before, "{before} -> {after}");
        assert_eq!(after, allocated_size(&path).unwrap(), "o relatório precisa bater com o disco");
        assert_eq!(fs::read(&path).unwrap(), data);
        assert_eq!(compact_file(&path).unwrap().0, compact_file(&path).unwrap().1, "segunda passada não muda nada");
        assert!(!path.with_extension("compacting").exists());
        let _ = fs::remove_file(path);
    }

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
            assert!(pkg.license.is_empty() || accepted_licenses(&root).contains(&pkg.license), "licença {} não aceita; aceite pelo app", pkg.license);
            if installed(&root).get(path) == Some(&pkg.revision_text()) {
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

    /// Compacta as imagens realmente instaladas no SDK do app e mostra o espaço liberado.
    /// Feche os emuladores antes. `cargo test compacts_installed_images -- --ignored --nocapture`
    #[test]
    #[ignore = "regrava as imagens instaladas"]
    fn compacts_installed_images() {
        let root = default_root();
        let used = |dir: &Path| -> u64 {
            let mut total = 0;
            let mut stack = vec![dir.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        total += allocated_size(&p).unwrap_or(0);
                    }
                }
            }
            total
        };
        let images = root.join("system-images");
        let before = used(&images);
        let saved = compact_installed(&root, &mut |l| println!("  {l}"));
        let after = used(&images);
        println!("system-images: {} MB -> {} MB (liberados {} MB)", before >> 20, after >> 20, saved >> 20);
        assert_eq!(before - after, saved);
    }
}
