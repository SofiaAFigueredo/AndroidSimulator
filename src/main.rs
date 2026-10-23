#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod autocomplete;
mod avd;
mod catalog;
mod diagnostics;
mod highlight;
mod logcat;
mod lsp;
mod project;
mod sdk;
mod servers;
mod terminal;
mod toolchain;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            .with_title("Android Simulator"),
        ..Default::default()
    };
    eframe::run_native(
        "Android Simulator",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
