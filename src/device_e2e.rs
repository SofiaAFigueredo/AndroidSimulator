//! Testes de ponta a ponta do simulador, com emulador, Gradle e interação de verdade.

use super::*;

/// A árvore de interface do Android (`uiautomator dump`), para conferir o que o app mostra sem olhar pixels.
fn ui_dump(root: &Path, port: u16) -> String {
    avd::adb(root, &["-s", &avd::serial(port), "shell", "uiautomator dump /sdcard/ui.xml >/dev/null 2>&1; cat /sdcard/ui.xml"]).unwrap_or_default()
}

/// `[x1,y1][x2,y2]` do nó que contém `needle`.
fn node_bounds(xml: &str, needle: &str) -> Option<(f32, f32, f32, f32)> {
    let at = xml.find(needle)?;
    let from = xml[..at].rfind("<node")?;
    let to = at + xml[at..].find('>')?;
    let node = &xml[from..to];
    let b = node.split("bounds=\"").nth(1)?.split('"').next()?;
    let n: Vec<f32> = b.replace("][", ",").trim_matches(['[', ']']).split(',').filter_map(|v| v.parse().ok()).collect();
    (n.len() == 4).then(|| (n[0], n[1], n[2], n[3]))
}

fn wait_for(root: &Path, port: u16, needle: &str, secs: u64) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let xml = ui_dump(root, port);
        if xml.contains(needle) {
            return Ok(xml);
        }
        if Instant::now() > deadline {
            return Err(format!("o app nunca mostrou {needle:?}; árvore: {}", &xml[..xml.len().min(600)]));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// BMP 32 bits sem compressão: dá para ver o quadro sem uma biblioteca de imagens.
fn write_bmp(path: &Path, f: &Frame) -> std::io::Result<()> {
    let size = 54 + f.rgba.len();
    let mut out = Vec::with_capacity(size);
    out.extend(b"BM");
    out.extend((size as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((f.width as i32).to_le_bytes());
    out.extend((-(f.height as i32)).to_le_bytes()); // altura negativa: de cima para baixo
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);
    for px in f.rgba.chunks_exact(4) {
        out.extend([px[2], px[1], px[0], 255]);
    }
    std::fs::write(path, out)
}

#[test]
fn parses_node_bounds_from_a_ui_dump() {
    let xml = r#"<hierarchy><node index="0" text="Cliques: 0" class="android.widget.TextView" bounds="[32,120][448,160]" /><node index="1" text="Toque aqui" class="android.widget.Button" bounds="[32,170][448,262]" /></hierarchy>"#;
    assert_eq!(node_bounds(xml, "text=\"Toque aqui\""), Some((32.0, 170.0, 448.0, 262.0)));
    assert_eq!(node_bounds(xml, "text=\"Cliques: 0\""), Some((32.0, 120.0, 448.0, 160.0)));
    assert_eq!(node_bounds(xml, "text=\"nao existe\""), None);
}

/// Simula um app em ação de ponta a ponta, num emulador sem janela: constrói o app de exemplo, instala,
/// liga a tela ao vivo e a entrada, toca no botão, arrasta para fora dele, digita e apaga texto, e confere
/// cada efeito na árvore de interface do Android. Salva a última tela em `ANDSIM_SHOT` (BMP).
/// `cargo test simulates_an_app_in_action -- --ignored --nocapture`
#[test]
#[ignore = "compila, sobe o emulador e interage com ele (alguns minutos)"]
fn simulates_an_app_in_action() {
    use crate::catalog::Kind;
    let root = crate::sdk::default_root();
    let (cat, _) = crate::catalog::fetch_all();
    crate::sdk::ensure_metadata(&root, &cat);
    let image = std::env::var("ANDSIM_IMAGE").unwrap_or_else(|_| "system-images;android-24;default;x86".into());
    assert!(crate::sdk::installed(&root).contains_key(&image), "{image} não instalada");
    let img = cat.packages.iter().find(|p| p.path == image).expect("imagem fora do catálogo");

    // O app de exemplo, compilado de verdade.
    let (platform, tools) = (cat.latest(Kind::Platform).unwrap(), cat.latest(Kind::BuildTools).unwrap());
    let dir = std::env::temp_dir().join(format!("andsim-sim-{}", std::process::id()));
    let api = crate::catalog::platform_api(&platform.path).unwrap();
    let bt = tools.path.split(';').nth(1).unwrap().to_owned();
    let spec = crate::project::NewProject { name: "Teste App", package: "com.exemplo.sim", compile_sdk: api, build_tools: &bt, sdk_root: &root };
    crate::project::create(&dir, &spec).unwrap();
    let apk = crate::project::build(&root, &dir, "assembleDebug", &mut |_| {}).expect("build").expect("sem APK");

    let name = format!("probe_sim_{}", avd::default_name(img));
    avd::create(&root, &name, img).unwrap();
    let port = 5588;
    let mut emu = avd::launch(&root, &name, &avd::Launch::new(true, port)).unwrap();
    let result = (|| -> Result<(), String> {
        avd::wait_boot(&root, &mut emu, port, Duration::from_secs(300))?;
        avd::run_apk(&root, port, &apk, "com.exemplo.sim", &mut |l| println!("run> {l}"))?;
        let mut sim = Simulator::start(&root, port)?;

        // 1. Tela ao vivo.
        let deadline = Instant::now() + Duration::from_secs(30);
        let first = loop {
            if let Some((seq, f)) = sim.screen.frame_after(0) {
                break (seq, f);
            }
            if Instant::now() > deadline {
                return Err("nenhum quadro em 30 s".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (w, h) = (first.1.width as f32, first.1.height as f32);
        println!("tela ao vivo: {}x{}", first.1.width, first.1.height);
        let t0 = Instant::now();
        let mut last_seq = first.0;
        while t0.elapsed() < Duration::from_secs(5) {
            if let Some((seq, _)) = sim.screen.frame_after(last_seq) {
                last_seq = seq;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let measured = (last_seq - first.0) as f32 / t0.elapsed().as_secs_f32();
        println!("RESULTADO fps: {measured:.1} quadros/s (medido em 5 s)");
        if measured < 1.0 {
            return Err(format!("a tela ao vivo está lenta demais: {measured:.1} fps"));
        }

        // 2. Toque: o botão conta cliques.
        let xml = wait_for(&root, port, "Cliques: 0", 30)?;
        let (x1, y1, x2, y2) = node_bounds(&xml, "class=\"android.widget.Button\"").ok_or("sem o botão na árvore")?;
        let (cx, cy) = ((x1 + x2) / 2.0 / w, (y1 + y2) / 2.0 / h);
        sim.touch_down(cx, cy)?;
        std::thread::sleep(Duration::from_millis(120));
        sim.touch_up()?;
        wait_for(&root, port, "Cliques: 1", 15).map_err(|e| format!("o toque não chegou ao botão: {e}"))?;
        println!("RESULTADO toque: Cliques: 1");

        // 3. Arrastar para fora do botão antes de soltar não conta como clique.
        sim.touch_down(cx, cy)?;
        for step in 1..=8 {
            sim.touch_move(cx, cy + 0.04 * step as f32)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        sim.touch_up()?;
        std::thread::sleep(Duration::from_secs(1));
        let xml = ui_dump(&root, port);
        if !xml.contains("Cliques: 1") {
            return Err(format!("o arrastar contou como clique: {}", &xml[..xml.len().min(400)]));
        }
        println!("RESULTADO arrastar: sem clique extra");

        // 4. Texto e tecla.
        let (ex1, ey1, ex2, ey2) = node_bounds(&xml, "class=\"android.widget.EditText\"").ok_or("sem o campo de texto")?;
        sim.touch_down((ex1 + ex2) / 2.0 / w, (ey1 + ey2) / 2.0 / h)?;
        std::thread::sleep(Duration::from_millis(120));
        sim.touch_up()?;
        std::thread::sleep(Duration::from_secs(1));
        sim.text("ola mundo")?;
        wait_for(&root, port, "text=\"ola mundo\"", 20).map_err(|e| format!("o texto não chegou ao campo: {e}"))?;
        println!("RESULTADO texto: ola mundo");
        sim.key(KEY_DEL)?;
        wait_for(&root, port, "text=\"ola mund\"", 20).map_err(|e| format!("a tecla apagar não chegou: {e}"))?;
        println!("RESULTADO tecla: apagar");

        // 5. A tela mudou de verdade.
        std::thread::sleep(Duration::from_millis(600));
        let (_, last) = sim.screen.frame_after(0).ok_or("sem quadro final")?;
        if last.rgba == first.1.rgba {
            return Err("a tela ao vivo não mudou depois das interações".into());
        }
        if let Ok(path) = std::env::var("ANDSIM_SHOT") {
            write_bmp(Path::new(&path), &last).map_err(|e| e.to_string())?;
        }
        sim.screen.stop();
        Ok(())
    })();
    avd::stop(&root, &mut emu, port);
    let _ = avd::delete(&root, &name);
    let _ = std::fs::remove_dir_all(&dir);
    result.expect("simular o app");
}
