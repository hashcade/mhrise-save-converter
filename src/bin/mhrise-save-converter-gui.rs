#[path = "../gui.rs"]
mod app;

use eframe::egui;

fn main() -> eframe::Result {
  let options = eframe::NativeOptions {
    viewport: egui::ViewportBuilder::default()
      .with_inner_size([820.0, 680.0])
      .with_min_inner_size([680.0, 520.0]),
    ..Default::default()
  };
  eframe::run_native(
    "Monster Hunter Rise Save Converter",
    options,
    Box::new(|context| Ok(Box::new(app::GuiApp::new(context)))),
  )
}
