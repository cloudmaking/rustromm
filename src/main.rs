// Hide the console window that Windows would otherwise attach to a GUI binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rustromm::app::RustRomm;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("RustRomM"),
        ..Default::default()
    };

    eframe::run_native(
        "RustRomM",
        options,
        Box::new(|cc| {
            // Needed for the PNG/JPEG cover art fetched from RomM.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(RustRomm::new(cc)))
        }),
    )
}
