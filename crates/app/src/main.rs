#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod settings;

use eframe::egui;

fn icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../../../assets/icon-256.png")).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Pressa")
        .with_inner_size([1080.0, 680.0])
        .with_min_inner_size([780.0, 460.0])
        .with_drag_and_drop(true);
    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Pressa", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
