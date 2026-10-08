use std::{
  path::PathBuf,
  process::Command,
  sync::mpsc::{self, Receiver, TryRecvError},
  thread,
  time::Duration,
};

use eframe::egui;
use mhrise_save_converter::conversion::{
  ConversionProgress, ConversionRequest, PreflightReport, TargetPlatform,
  convert_path_with_progress, preflight_path,
};

const FORM_LABEL_WIDTH: f32 = 160.0;
const FORM_BUTTON_WIDTH: f32 = 84.0;
const FORM_COLUMN_SPACING: f32 = 12.0;
const FORM_ROW_HEIGHT: f32 = 28.0;

enum WorkerEvent {
  Progress(ConversionProgress),
  Finished(Result<Vec<PathBuf>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiTarget {
  Steam,
  Switch,
}

impl GuiTarget {
  fn platform(self) -> TargetPlatform {
    match self {
      Self::Steam => TargetPlatform::Steam,
      Self::Switch => TargetPlatform::NintendoSwitch,
    }
  }
}

pub struct GuiApp {
  source: String,
  output: String,
  target_reference: String,
  source_steamid64: String,
  target_steamid64: String,
  source_curve_index: String,
  target_curve_index: String,
  target: GuiTarget,
  overwrite: bool,
  preflight: Option<PreflightReport>,
  status: String,
  progress: Option<ConversionProgress>,
  output_directory: Option<PathBuf>,
  worker: Option<Receiver<WorkerEvent>>,
}

impl Default for GuiApp {
  fn default() -> Self {
    Self {
      source: String::new(),
      output: String::new(),
      target_reference: String::new(),
      source_steamid64: String::new(),
      target_steamid64: String::new(),
      source_curve_index: String::new(),
      target_curve_index: String::new(),
      target: GuiTarget::Steam,
      overwrite: false,
      preflight: None,
      status: String::new(),
      progress: None,
      output_directory: None,
      worker: None,
    }
  }
}

impl GuiApp {
  fn request(&self) -> Result<ConversionRequest, String> {
    let parse_id = |value: &str, label: &str| {
      if value.trim().is_empty() {
        Ok(None)
      } else {
        value.trim().parse::<u64>().map(Some).map_err(|_| format!("{label} must be a number"))
      }
    };
    let parse_curve = |value: &str, label: &str| {
      if value.trim().is_empty() {
        Ok(None)
      } else {
        value.trim().parse::<usize>().map(Some).map_err(|_| format!("{label} must be a number"))
      }
    };

    Ok(ConversionRequest {
      target: self.target.platform(),
      source_steamid64: parse_id(&self.source_steamid64, "Source SteamID64")?,
      target_steamid64: parse_id(&self.target_steamid64, "Target SteamID64")?,
      source_curve_index: parse_curve(&self.source_curve_index, "Source Curve Index")?,
      target_curve_index: parse_curve(&self.target_curve_index, "Target Curve Index")?,
      target_reference: (!self.target_reference.trim().is_empty())
        .then(|| PathBuf::from(self.target_reference.trim())),
      force: self.overwrite,
    })
  }

  fn paths(&self) -> Result<(PathBuf, PathBuf), String> {
    if self.source.trim().is_empty() {
      return Err("Choose a source save directory first.".to_owned());
    }
    if self.output.trim().is_empty() {
      return Err("Choose an output directory first.".to_owned());
    }
    Ok((PathBuf::from(self.source.trim()), PathBuf::from(self.output.trim())))
  }

  fn choose_folder(value: &mut String) {
    if let Some(path) = rfd::FileDialog::new().pick_folder() {
      *value = path.display().to_string();
    }
  }

  fn run_preflight(&mut self) {
    let result = (|| {
      let (source, output) = self.paths()?;
      let request = self.request()?;
      preflight_path(&source, &output, &request).map_err(|error| error.to_string())
    })();
    match result {
      Ok(report) => {
        self.status = if report.can_convert() {
          "Preflight passed. Ready to convert.".to_owned()
        } else {
          "Preflight found errors. Fix them before converting.".to_owned()
        };
        self.preflight = Some(report);
      }
      Err(error) => {
        self.preflight = None;
        self.status = error;
      }
    }
  }

  fn start_conversion(&mut self) {
    let prepared = (|| {
      let (source, output) = self.paths()?;
      let request = self.request()?;
      let report = preflight_path(&source, &output, &request).map_err(|error| error.to_string())?;
      if !report.can_convert() {
        let message = report.errors().collect::<Vec<_>>().join("; ");
        return Err(message);
      }
      Ok((source, output, request, report))
    })();

    let Ok((source, output, request, report)) = prepared else {
      self.run_preflight();
      return;
    };
    let (sender, receiver) = mpsc::channel();
    self.preflight = Some(report);
    self.progress = None;
    self.output_directory = Some(output.clone());
    self.status = "Converting…".to_owned();
    self.worker = Some(receiver);
    thread::spawn(move || {
      let result = convert_path_with_progress(&source, &output, request, |progress| {
        let _ = sender.send(WorkerEvent::Progress(progress));
      })
      .map_err(|error| error.to_string());
      let _ = sender.send(WorkerEvent::Finished(result));
    });
  }

  fn poll_worker(&mut self) {
    let Some(receiver) = self.worker.take() else {
      return;
    };
    let mut keep_receiver = true;
    loop {
      match receiver.try_recv() {
        Ok(WorkerEvent::Progress(progress)) => self.progress = Some(progress),
        Ok(WorkerEvent::Finished(result)) => {
          keep_receiver = false;
          match result {
            Ok(files) => self.status = format!("Finished: {} file(s) written.", files.len()),
            Err(error) => {
              self.output_directory = None;
              self.status = format!("Conversion failed: {error}");
            }
          }
        }
        Err(TryRecvError::Empty) => break,
        Err(TryRecvError::Disconnected) => {
          keep_receiver = false;
          self.output_directory = None;
          self.status = "Conversion worker stopped unexpectedly.".to_owned();
          break;
        }
      }
    }
    if keep_receiver {
      self.worker = Some(receiver);
    }
  }

  fn open_output_directory(&mut self) {
    let Some(path) = self.output_directory.as_deref() else {
      self.status = "No converted output is available yet.".to_owned();
      return;
    };
    let command = if cfg!(target_os = "macos") {
      "open"
    } else if cfg!(target_os = "windows") {
      "explorer"
    } else {
      "xdg-open"
    };
    match Command::new(command).arg(path).spawn() {
      Ok(_) => self.status = format!("Opened {}", path.display()),
      Err(error) => self.status = format!("Could not open output directory: {error}"),
    }
  }

  fn render_form_label(ui: &mut egui::Ui, label: &str) {
    ui.allocate_ui_with_layout(
      egui::vec2(FORM_LABEL_WIDTH, FORM_ROW_HEIGHT),
      egui::Layout::left_to_right(egui::Align::Center),
      |ui| {
        ui.set_min_size(egui::vec2(FORM_LABEL_WIDTH, FORM_ROW_HEIGHT));
        ui.label(label);
      },
    );
  }

  fn render_path_row(ui: &mut egui::Ui, label: &str, hint: &str, value: &mut String, width: f32) {
    Self::render_form_label(ui, label);
    Self::render_text_input(ui, value, hint, width);
    if ui.add_sized([FORM_BUTTON_WIDTH, FORM_ROW_HEIGHT], egui::Button::new("Browse…")).clicked()
    {
      Self::choose_folder(value);
    }
    ui.end_row();
  }

  fn render_text_input(
    ui: &mut egui::Ui,
    value: &mut String,
    hint: &str,
    width: f32,
  ) -> egui::Response {
    ui.add_sized(
      [width, FORM_ROW_HEIGHT],
      egui::TextEdit::singleline(value).hint_text(hint).vertical_align(egui::Align::Center),
    )
  }
}

impl eframe::App for GuiApp {
  fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    self.poll_worker();
    egui::CentralPanel::default()
      .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(24))
      .show(ctx, |ui| {
        let input_width =
          (ui.available_width() - FORM_LABEL_WIDTH - FORM_BUTTON_WIDTH - 2.0 * FORM_COLUMN_SPACING)
            .max(120.0);
        egui::Grid::new("save_paths").num_columns(3).spacing([FORM_COLUMN_SPACING, 10.0]).show(
          ui,
          |ui| {
            Self::render_path_row(
              ui,
              "Source save",
              "Original save folder to convert",
              &mut self.source,
              input_width,
            );
            Self::render_path_row(
              ui,
              "Target save",
              "Existing save folder for the target platform",
              &mut self.target_reference,
              input_width,
            );
            Self::render_path_row(
              ui,
              "Output folder",
              "New folder for converted saves",
              &mut self.output,
              input_width,
            );
            Self::render_form_label(ui, "Convert to");
            ui.horizontal(|ui| {
              ui.radio_value(&mut self.target, GuiTarget::Steam, "Steam");
              ui.radio_value(&mut self.target, GuiTarget::Switch, "Nintendo Switch");
            });
            ui.end_row();
          },
        );

        ui.add_space(12.0);
        egui::CollapsingHeader::new("Steam and advanced options").show_unindented(ui, |ui| {
          egui::Grid::new("steam_options")
            .num_columns(2)
            .spacing([FORM_COLUMN_SPACING, 10.0])
            .show(ui, |ui| {
              for (label, value) in [
                ("Source SteamID64", &mut self.source_steamid64),
                ("Target SteamID64", &mut self.target_steamid64),
                ("Source Curve Index", &mut self.source_curve_index),
                ("Target Curve Index", &mut self.target_curve_index),
              ] {
                Self::render_form_label(ui, label);
                Self::render_text_input(ui, value, "", input_width);
                ui.end_row();
              }
            });
          ui.add_space(8.0);
          ui.checkbox(&mut self.overwrite, "Allow writing to a non-empty output directory");
        });

        ui.add_space(12.0);
        ui.allocate_ui_with_layout(
          egui::vec2(ui.available_width(), FORM_ROW_HEIGHT),
          egui::Layout::right_to_left(egui::Align::Center),
          |ui| {
            if ui
              .add_enabled(self.worker.is_none(), egui::Button::new("Convert save"))
              .on_hover_text("Checks the save first, then converts it.")
              .clicked()
            {
              self.start_conversion();
            }
          },
        );

        if !self.status.is_empty() {
          ui.add_space(8.0);
          ui.separator();
          ui.label(&self.status);
        }
        let output_available = self.worker.is_none()
          && self.output_directory.as_deref().is_some_and(|path| path.is_dir());
        if output_available && ui.button("Open output folder").clicked() {
          self.open_output_directory();
        }
        if let Some(progress) = &self.progress {
          let fraction = progress.completed as f32 / progress.total.max(1) as f32;
          ui.add(
            egui::ProgressBar::new(fraction)
              .text(format!("{} / {}", progress.completed, progress.total)),
          );
          ui.label(format!("Current: {}", progress.current_file.display()));
        }
        if let Some(report) = &self.preflight {
          ui.separator();
          ui.label(format!(
            "Files: {} ({} core, {} album/photo)",
            report.file_count, report.core_file_count, report.auxiliary_file_count
          ));
          for error in report.errors() {
            ui.colored_label(egui::Color32::RED, format!("Error: {error}"));
          }
          for warning in report.warnings() {
            ui.colored_label(egui::Color32::YELLOW, format!("Warning: {warning}"));
          }
        }
      });
    if self.worker.is_some() {
      ctx.request_repaint_after(Duration::from_millis(100));
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn form_labels_keep_inputs_aligned_at_supported_window_widths() {
    for width in [680.0, 820.0, 1120.0] {
      let context = egui::Context::default();
      let mut inputs = Vec::new();
      for _ in 0..3 {
        inputs.clear();
        let _ = context.run(
          egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
              egui::Pos2::ZERO,
              egui::vec2(width, 680.0),
            )),
            ..Default::default()
          },
          |ctx| {
            egui::CentralPanel::default()
              .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(24))
              .show(ctx, |ui| {
                egui::Grid::new("alignment_test")
                  .num_columns(2)
                  .spacing([FORM_COLUMN_SPACING, 10.0])
                  .show(ui, |ui| {
                    for label in ["Source save", "Target save", "Source Curve Index"] {
                      GuiApp::render_form_label(ui, label);
                      let mut value = String::new();
                      inputs.push(GuiApp::render_text_input(ui, &mut value, "", 200.0).rect);
                      ui.end_row();
                    }
                  });
              });
          },
        );
      }
      for input in inputs {
        assert!((input.left() - (24.0 + FORM_LABEL_WIDTH + FORM_COLUMN_SPACING)).abs() < 1.0);
        assert!((input.height() - FORM_ROW_HEIGHT).abs() < 1.0);
      }
    }
  }

  #[test]
  fn placeholders_and_entered_text_are_vertically_centered() {
    for entered_text in ["", "/example/win64_save"] {
      let context = egui::Context::default();
      let hint = "Existing save folder for the target platform";
      let mut value = entered_text.to_owned();
      let mut input_rect = egui::Rect::NOTHING;
      let output = context.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
          input_rect = GuiApp::render_text_input(ui, &mut value, hint, 500.0).rect;
        });
      });
      let expected_text = if entered_text.is_empty() { hint } else { entered_text };
      let text = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
          egui::epaint::Shape::Text(text) if text.galley.job.text == expected_text => Some(text),
          _ => None,
        })
        .expect("input text should be painted");
      let text_center = text.pos.y + text.galley.size().y / 2.0;
      assert!((text_center - input_rect.center().y).abs() < 1.0);
    }
  }

  #[test]
  fn default_request_targets_steam_without_optional_values() {
    let app = GuiApp::default();
    let request = app.request().expect("default GUI state should be parseable");

    assert_eq!(request.target, TargetPlatform::Steam);
    assert_eq!(request.source_steamid64, None);
    assert_eq!(request.target_reference, None);
    assert!(!request.force);
    assert!(app.status.is_empty());
  }

  #[test]
  fn request_rejects_non_numeric_steam_id() {
    let app = GuiApp { target_steamid64: "not-a-steamid".to_owned(), ..GuiApp::default() };

    let error = app.request().expect_err("invalid SteamID64 should be rejected");

    assert_eq!(error, "Target SteamID64 must be a number");
  }

  #[test]
  fn paths_require_source_and_output() {
    let app = GuiApp::default();
    assert_eq!(
      app.paths().expect_err("source is required"),
      "Choose a source save directory first."
    );

    let app = GuiApp { source: "/tmp/source".to_owned(), ..GuiApp::default() };
    assert_eq!(app.paths().expect_err("output is required"), "Choose an output directory first.");
  }
}
