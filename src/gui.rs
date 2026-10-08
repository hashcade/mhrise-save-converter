use mhrise_save_converter::format::{Platform, parse_header};
use std::{
  io::Read,
  path::{Path, PathBuf},
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
use mhrise_save_converter::slots::{SlotInspection, SlotOptions, inspect_slots, reorder_slots};

type SlotInputKey = (String, String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiMode {
  Convert,
  ManageSlots,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingConversion {
  source: PathBuf,
  output: PathBuf,
  request: ConversionRequest,
  conflicts: Vec<PathBuf>,
}

const FORM_LABEL_WIDTH: f32 = 160.0;
const FORM_BUTTON_WIDTH: f32 = 84.0;
const FORM_COLUMN_SPACING: f32 = 12.0;
const FORM_ROW_HEIGHT: f32 = 28.0;
const SLOT_ROW_HEIGHT: f32 = 40.0;

enum WorkerEvent {
  Progress(ConversionProgress),
  Finished(Result<Vec<PathBuf>, String>),
  Inspected(SlotInputKey, Result<SlotInspection, String>),
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
  mode: GuiMode,
  slots: Option<SlotInspection>,
  slots_key: Option<SlotInputKey>,
  slot_order: [u8; 3],
  source: String,
  output: String,
  target_reference: String,
  source_steamid64: String,
  target_steamid64: String,
  source_curve_index: String,
  target_curve_index: String,
  target: GuiTarget,
  source_platform: Option<(String, Option<Platform>)>,
  pending_conversion: Option<PendingConversion>,
  preflight: Option<PreflightReport>,
  status: String,
  progress: Option<ConversionProgress>,
  output_directory: Option<PathBuf>,
  worker: Option<Receiver<WorkerEvent>>,
}

impl Default for GuiApp {
  fn default() -> Self {
    Self {
      mode: GuiMode::Convert,
      slots: None,
      slots_key: None,
      slot_order: [1, 2, 3],
      source: String::new(),
      output: String::new(),
      target_reference: String::new(),
      source_steamid64: String::new(),
      target_steamid64: String::new(),
      source_curve_index: String::new(),
      target_curve_index: String::new(),
      target: GuiTarget::Steam,
      source_platform: None,
      pending_conversion: None,
      preflight: None,
      status: String::new(),
      progress: None,
      output_directory: None,
      worker: None,
    }
  }
}

impl GuiApp {
  pub fn new(context: &eframe::CreationContext<'_>) -> Self {
    // Character names may contain CJK text, which egui's default fonts do not cover.
    for path in [
      "/System/Library/Fonts/PingFang.ttc",
      "/System/Library/Fonts/STHeiti Light.ttc",
      "C:\\Windows\\Fonts\\msyh.ttc",
      "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ] {
      if let Ok(bytes) = std::fs::read(path) {
        let mut fonts = egui::FontDefinitions::default();
        fonts
          .font_data
          .insert("character_names".to_owned(), egui::FontData::from_owned(bytes).into());
        fonts
          .families
          .entry(egui::FontFamily::Proportional)
          .or_default()
          .push("character_names".to_owned());
        context.egui_ctx.set_fonts(fonts);
        break;
      }
    }
    Self::default()
  }

  fn slot_input_key(&self) -> SlotInputKey {
    (
      self.source.trim().to_owned(),
      self.source_steamid64.trim().to_owned(),
      self.source_curve_index.trim().to_owned(),
    )
  }

  fn refresh_source_platform(&mut self) {
    let source = self.source.trim();
    if self.source_platform.as_ref().is_some_and(|(path, _)| path == source) {
      return;
    }
    let path = Path::new(source);
    let file = if path.is_dir() { path.join("data00-1.bin") } else { path.to_path_buf() };
    let platform = (|| {
      let mut bytes = [0; 16];
      std::fs::File::open(file).ok()?.read_exact(&mut bytes).ok()?;
      Some(parse_header(&bytes).ok()?.platform())
    })();
    self.source_platform = Some((source.to_owned(), platform));
  }

  fn source_is_switch(&self) -> bool {
    self.source_platform.as_ref().is_some_and(|(path, platform)| {
      path == self.source.trim() && *platform == Some(Platform::NintendoSwitch)
    })
  }

  fn slot_options(&self) -> Result<SlotOptions, String> {
    if self.source_is_switch() {
      return Ok(SlotOptions { steamid64: None, curve_index: None });
    }
    Ok(SlotOptions {
      steamid64: optional_number(&self.source_steamid64, "Current SteamID64")?,
      curve_index: optional_number(&self.source_curve_index, "Curve Index")?,
    })
  }

  fn can_save_slots(&self) -> bool {
    self.slots_key.as_ref() == Some(&self.slot_input_key())
      && self.slots.as_ref().is_some_and(|report| {
        self.slot_order.iter().enumerate().any(|(index, from)| {
          usize::from(*from) != index + 1
            && report.slots.iter().any(|slot| slot.number == *from && slot.occupied())
        })
      })
  }

  fn move_slot(&mut self, index: usize, up: bool) {
    if self.worker.is_some() || self.slots_key.as_ref() != Some(&self.slot_input_key()) {
      return;
    }
    let Some(from) = self.slot_order.get(index) else { return };
    if !self
      .slots
      .as_ref()
      .is_some_and(|report| report.slots.iter().any(|slot| slot.number == *from && slot.occupied()))
    {
      return;
    }
    let adjacent = if up { index.checked_sub(1) } else { index.checked_add(1) };
    if let Some(adjacent) = adjacent.filter(|other| *other < self.slot_order.len()) {
      self.slot_order.swap(index, adjacent);
    }
  }

  fn load_slots(&mut self) {
    self.refresh_source_platform();
    let prepared = (|| {
      if self.source.trim().is_empty() {
        return Err("Choose a source save directory first.".to_owned());
      }
      Ok((PathBuf::from(self.source.trim()), self.slot_options()?))
    })();
    let (source, options) = match prepared {
      Ok(value) => value,
      Err(error) => {
        self.status = error;
        return;
      }
    };
    let key = self.slot_input_key();
    let (sender, receiver) = mpsc::channel();
    self.slots = None;
    self.slots_key = None;
    self.slot_order = [1, 2, 3];
    self.preflight = None;
    self.progress = None;
    self.output_directory = None;
    self.status = "Reading character slots…".to_owned();
    self.worker = Some(receiver);
    thread::spawn(move || {
      let result = inspect_slots(&source, options).map_err(|error| format!("{error:#}"));
      let _ = sender.send(WorkerEvent::Inspected(key, result));
    });
  }

  fn save_slot_order(&mut self) {
    let prepared = (|| {
      let (source, output) = self.paths()?;
      if !self.can_save_slots() {
        return Err("Read the current save and move a character before saving.".to_owned());
      }
      Ok((source, output, self.slot_options()?))
    })();
    let (source, output, options) = match prepared {
      Ok(value) => value,
      Err(error) => {
        self.status = error;
        return;
      }
    };
    let order = self.slot_order;
    let (sender, receiver) = mpsc::channel();
    self.preflight = None;
    self.progress = None;
    self.output_directory = Some(output.clone());
    self.status = "Saving slot order…".to_owned();
    self.worker = Some(receiver);
    thread::spawn(move || {
      let result =
        reorder_slots(&source, &output, order, options).map_err(|error| format!("{error:#}"));
      let _ = sender.send(WorkerEvent::Finished(result));
    });
  }

  fn request(&self) -> Result<ConversionRequest, String> {
    Ok(ConversionRequest {
      target: self.target.platform(),
      source_steamid64: if self.source_is_switch() {
        None
      } else {
        optional_number(&self.source_steamid64, "Source SteamID64")?
      },
      target_steamid64: if self.target == GuiTarget::Steam {
        optional_number(&self.target_steamid64, "Target SteamID64")?
      } else {
        None
      },
      source_curve_index: if self.source_is_switch() {
        None
      } else {
        optional_number(&self.source_curve_index, "Source Curve Index")?
      },
      target_curve_index: if self.target == GuiTarget::Steam {
        optional_number(&self.target_curve_index, "Target Curve Index")?
      } else {
        None
      },
      target_reference: (!self.target_reference.trim().is_empty())
        .then(|| PathBuf::from(self.target_reference.trim())),
      force: false,
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

  fn start_conversion(&mut self, approved: Option<PendingConversion>) {
    self.refresh_source_platform();
    let prepared = (|| {
      let (source, output) = self.paths()?;
      let request = self.request()?;
      let validation = ConversionRequest { force: true, ..request.clone() };
      let report =
        preflight_path(&source, &output, &validation).map_err(|error| error.to_string())?;
      if !report.can_convert() {
        let message = report.errors().collect::<Vec<_>>().join("; ");
        return Err(message);
      }
      Ok((source, output, request, report))
    })();

    let Ok((source, output, mut request, report)) = prepared else {
      self.run_preflight();
      return;
    };
    if !report.output_conflicts.is_empty() {
      let pending = PendingConversion {
        source: source.clone(),
        output: output.clone(),
        request: request.clone(),
        conflicts: report.output_conflicts.clone(),
      };
      if approved.as_ref() != Some(&pending) {
        self.pending_conversion = Some(pending);
        return;
      }
      request.force = true;
    }
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
        Ok(WorkerEvent::Inspected(key, result)) => {
          keep_receiver = false;
          if key != self.slot_input_key() {
            self.status = "Source settings changed. Load the slots again.".to_owned();
            break;
          }
          match result {
            Ok(report) => {
              self.slot_order = [1, 2, 3];
              self.status.clear();
              self.slots = Some(report);
              self.slots_key = Some(key);
            }
            Err(error) => self.status = format!("Could not read slots: {error}"),
          }
          break;
        }
        Ok(WorkerEvent::Progress(progress)) => self.progress = Some(progress),
        Ok(WorkerEvent::Finished(result)) => {
          keep_receiver = false;
          match result {
            Ok(files) => self.status = format!("Finished: {} file(s) written.", files.len()),
            Err(error) => {
              self.output_directory = None;
              self.status = format!("Save operation failed: {error}");
            }
          }
          break;
        }
        Err(TryRecvError::Empty) => break,
        Err(TryRecvError::Disconnected) => {
          keep_receiver = false;
          self.output_directory = None;
          self.status = "Save worker stopped unexpectedly.".to_owned();
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

  fn render_mode_tabs(&mut self, ui: &mut egui::Ui) {
    ui.add_enabled_ui(self.worker.is_none(), |ui| {
      ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (mode, label) in
          [(GuiMode::Convert, "Convert save"), (GuiMode::ManageSlots, "Manage slots")]
        {
          let selected = self.mode == mode;
          let color =
            if selected { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
          let fill =
            if selected { ui.visuals().faint_bg_color } else { egui::Color32::TRANSPARENT };
          let response = ui.add_sized(
            [160.0, 32.0],
            egui::Button::new(egui::RichText::new(label).color(color))
              .selected(selected)
              .fill(fill)
              .stroke(egui::Stroke::NONE),
          );
          if selected {
            let rect = response.rect;
            ui.painter().line_segment(
              [rect.left_bottom(), rect.right_bottom()],
              egui::Stroke::new(2.0_f32, color),
            );
          }
          if response.clicked() {
            self.mode = mode;
          }
        }
      });
    });
    ui.separator();
    ui.add_space(12.0);
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

  fn render_conversion_options(&mut self, ui: &mut egui::Ui, input_width: f32) {
    egui::Grid::new("conversion_accounts")
      .num_columns(2)
      .spacing([FORM_COLUMN_SPACING, 10.0])
      .show(ui, |ui| {
        if !self.source_is_switch() {
          Self::render_form_label(ui, "Source SteamID64 *");
          let hint = if self
            .source_platform
            .as_ref()
            .is_some_and(|(_, platform)| *platform == Some(Platform::Steam))
          {
            "Required — account that owns the source save"
          } else {
            "Required for Steam source saves"
          };
          Self::render_text_input(ui, &mut self.source_steamid64, hint, input_width);
          ui.end_row();
        }
        if self.target == GuiTarget::Steam {
          Self::render_form_label(ui, "Target SteamID64 *");
          Self::render_text_input(
            ui,
            &mut self.target_steamid64,
            "Required — destination Steam account",
            input_width,
          );
          ui.end_row();
        }
      });
    ui.add_space(12.0);
    if self.source_is_switch() && self.target == GuiTarget::Switch {
      return;
    }
    egui::CollapsingHeader::new("Advanced options").show_unindented(ui, |ui| {
      egui::Grid::new("conversion_curves")
        .num_columns(2)
        .spacing([FORM_COLUMN_SPACING, 10.0])
        .show(ui, |ui| {
          if !self.source_is_switch() {
            Self::render_form_label(ui, "Source Curve Index");
            Self::render_text_input(
              ui,
              &mut self.source_curve_index,
              "Detected automatically",
              input_width,
            );
            ui.end_row();
          }
          if self.target == GuiTarget::Steam {
            Self::render_form_label(ui, "Target Curve Index");
            Self::render_text_input(
              ui,
              &mut self.target_curve_index,
              "Detected from target save",
              input_width,
            );
            ui.end_row();
          }
        });
    });
  }

  fn render_overwrite_confirmation(&mut self, ctx: &egui::Context) {
    let Some(pending) = self.pending_conversion.take() else {
      return;
    };
    let mut confirm = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("confirm_save_overwrite")).show(ctx, |ui| {
      ui.set_width(420.0);
      ui.heading("Overwrite existing files?");
      ui.label(format!(
        "{} file(s) with the same names already exist in:",
        pending.conflicts.len()
      ));
      ui.label(pending.output.display().to_string());
      egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
        for path in &pending.conflicts {
          ui.label(path.file_name().unwrap_or_default().to_string_lossy());
        }
      });
      ui.add_space(12.0);
      ui.horizontal(|ui| {
        cancel = ui.button("Cancel").clicked();
        confirm = ui.button("Overwrite and convert").clicked();
      });
    });
    if confirm {
      self.start_conversion(Some(pending));
    } else if !cancel && !modal.should_close() {
      self.pending_conversion = Some(pending);
    }
  }

  fn render_slot_controls(&mut self, ui: &mut egui::Ui, input_width: f32) {
    if !self.source_is_switch() {
      egui::Grid::new("slot_account").num_columns(2).spacing([FORM_COLUMN_SPACING, 10.0]).show(
        ui,
        |ui| {
          Self::render_form_label(ui, "Current SteamID64 *");
          Self::render_text_input(
            ui,
            &mut self.source_steamid64,
            "Required for Steam saves; leave blank for Switch",
            input_width,
          );
          ui.end_row();
        },
      );
      egui::CollapsingHeader::new("Advanced slot options").show_unindented(ui, |ui| {
        egui::Grid::new("slot_curve").num_columns(2).spacing([FORM_COLUMN_SPACING, 10.0]).show(
          ui,
          |ui| {
            Self::render_form_label(ui, "Curve Index");
            Self::render_text_input(
              ui,
              &mut self.source_curve_index,
              "Detected automatically",
              input_width,
            );
            ui.end_row();
          },
        );
      });
    }
    if self.slots_key.as_ref().is_some_and(|key| key != &self.slot_input_key()) {
      self.slots = None;
      self.slots_key = None;
      self.slot_order = [1, 2, 3];
    }
    ui.add_space(8.0);
    if ui.add_enabled(self.worker.is_none(), egui::Button::new("Read save")).clicked() {
      self.load_slots();
    }
    ui.add_space(16.0);
    let Some(report) = &self.slots else {
      ui.weak("Read a save to view its three slots.");
      return;
    };
    let mut movement = None;
    egui::Grid::new("character_slots")
      .num_columns(4)
      .min_col_width(32.0)
      .spacing([12.0, 12.0])
      .striped(true)
      .show(ui, |ui| {
        for title in ["Slot", "Character", "Progress", "Move"] {
          ui.strong(title);
        }
        ui.end_row();
        for (index, from) in self.slot_order.iter().enumerate() {
          let slot = &report.slots[usize::from(*from - 1)];
          ui.label(format!("Slot {}", index + 1));
          ui.allocate_ui_with_layout(
            egui::vec2(160.0, SLOT_ROW_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
              ui.set_min_size(egui::vec2(160.0, SLOT_ROW_HEIGHT));
              ui.label(slot.name.as_deref().unwrap_or("Empty"));
            },
          );
          ui.allocate_ui_with_layout(
            egui::vec2(140.0, SLOT_ROW_HEIGHT),
            if slot.occupied() {
              egui::Layout::top_down(egui::Align::Min)
            } else {
              egui::Layout::left_to_right(egui::Align::Center)
            },
            |ui| {
              ui.set_min_size(egui::vec2(140.0, SLOT_ROW_HEIGHT));
              ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
              if slot.occupied() {
                let text_height = ui.text_style_height(&egui::TextStyle::Body);
                let block_height = 2.0 * text_height + ui.spacing().item_spacing.y;
                ui.add_space(((SLOT_ROW_HEIGHT - block_height) / 2.0).max(0.0));
                ui.label(format!("HR {} · MR {}", slot.hunter_rank, slot.master_rank));
                ui.weak(slot.playtime());
              } else {
                ui.weak("Empty slot");
              }
            },
          );
          ui.allocate_ui_with_layout(
            egui::vec2(68.0, SLOT_ROW_HEIGHT),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
              ui.set_min_size(egui::vec2(68.0, SLOT_ROW_HEIGHT));
              for (up, label, available) in [(true, "↑", index > 0), (false, "↓", index < 2)] {
                if ui
                  .add_enabled(
                    self.worker.is_none() && slot.occupied() && available,
                    egui::Button::new(label).min_size(egui::vec2(28.0, FORM_ROW_HEIGHT)),
                  )
                  .on_hover_text(if up { "Move up" } else { "Move down" })
                  .clicked()
                {
                  movement = Some((index, up));
                }
              }
            },
          );
          ui.end_row();
        }
      });
    if let Some((index, up)) = movement {
      self.move_slot(index, up);
    }
  }
}

fn optional_number<T: std::str::FromStr>(value: &str, label: &str) -> Result<Option<T>, String> {
  if value.trim().is_empty() {
    Ok(None)
  } else {
    value.trim().parse().map(Some).map_err(|_| format!("{label} must be a number"))
  }
}

impl eframe::App for GuiApp {
  fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    self.poll_worker();
    egui::CentralPanel::default()
      .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(24))
      .show(ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
          self.render_mode_tabs(ui);
          let input_width = (ui.available_width()
            - FORM_LABEL_WIDTH
            - FORM_BUTTON_WIDTH
            - 2.0 * FORM_COLUMN_SPACING)
            .max(120.0);
          egui::Grid::new("save_paths").num_columns(3).spacing([FORM_COLUMN_SPACING, 10.0]).show(
            ui,
            |ui| {
              Self::render_path_row(
                ui,
                "Source save",
                "Existing save folder",
                &mut self.source,
                input_width,
              );
              self.refresh_source_platform();
              if self.mode == GuiMode::Convert {
                Self::render_path_row(
                  ui,
                  "Target save",
                  "Existing save folder for the target platform",
                  &mut self.target_reference,
                  input_width,
                );
              }
              if self.mode == GuiMode::ManageSlots {
                Self::render_form_label(ui, "Output folder");
                Self::render_text_input(
                  ui,
                  &mut self.output,
                  "New folder; must not already exist",
                  input_width,
                );
                if ui
                  .add_sized([FORM_BUTTON_WIDTH, FORM_ROW_HEIGHT], egui::Button::new("Browse…"))
                  .on_hover_text(
                    "Choose a parent folder; output will be its new swapped-save subfolder.",
                  )
                  .clicked()
                  && let Some(parent) = rfd::FileDialog::new().pick_folder()
                {
                  self.output = parent.join("swapped-save").display().to_string();
                }
                ui.end_row();
              } else {
                Self::render_path_row(
                  ui,
                  "Output folder",
                  "Folder for converted saves",
                  &mut self.output,
                  input_width,
                );
              }
              if self.mode == GuiMode::Convert {
                Self::render_form_label(ui, "Convert to");
                ui.horizontal(|ui| {
                  ui.radio_value(&mut self.target, GuiTarget::Steam, "Steam");
                  ui.radio_value(&mut self.target, GuiTarget::Switch, "Nintendo Switch");
                });
                ui.end_row();
              }
            },
          );

          ui.add_space(12.0);
          if self.mode == GuiMode::ManageSlots {
            self.render_slot_controls(ui, input_width);
          } else {
            self.render_conversion_options(ui, input_width);
          }

          ui.add_space(12.0);
          ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), FORM_ROW_HEIGHT),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
              let managing_slots = self.mode == GuiMode::ManageSlots;
              if ui
                .add_enabled(
                  self.worker.is_none() && (!managing_slots || self.can_save_slots()),
                  egui::Button::new(if managing_slots {
                    "Save slots".to_owned()
                  } else {
                    "Convert save".to_owned()
                  }),
                )
                .on_hover_text(if managing_slots {
                  "Saves the displayed slot order to a new directory without modifying the source."
                } else {
                  "Checks the save first, then converts it."
                })
                .clicked()
              {
                if managing_slots {
                  self.save_slot_order();
                } else {
                  self.start_conversion(None);
                }
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
      });
    self.render_overwrite_confirmation(ctx);
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
  fn overwrite_prompt_can_be_cancelled_and_approval_does_not_follow_changed_output() {
    let root =
      std::env::temp_dir().join(format!("mhrise-gui-confirm-{:016x}", rand::random::<u64>()));
    let source = root.join("source");
    let output = root.join("output");
    let other = root.join("other");
    for path in [&source, &output, &other] {
      std::fs::create_dir_all(path).unwrap();
    }
    // A valid Switch container holding an empty deflate stream; preflight does not parse classes.
    let mut data = b"DSSS\x02\0\0\0\x08\0\0\0\x03\0\0\0".to_vec();
    let hash = murmur3::murmur3_32(&mut std::io::Cursor::new(&data), 0xffff_ffff).unwrap();
    data.extend_from_slice(&hash.to_le_bytes());
    std::fs::write(source.join("data00-1.bin"), &data).unwrap();
    for path in [&output, &other] {
      std::fs::write(path.join("data00-1.bin"), b"existing save").unwrap();
    }
    let mut app = GuiApp {
      source: source.display().to_string(),
      output: output.display().to_string(),
      target: GuiTarget::Switch,
      ..GuiApp::default()
    };
    app.start_conversion(None);
    assert!(app.pending_conversion.is_some());
    assert!(app.worker.is_none(), "do not start writing before confirmation");
    let context = egui::Context::default();
    for frame in 0..3 {
      let _ = context.run(
        egui::RawInput {
          events: if frame == 2 {
            vec![egui::Event::Key {
              key: egui::Key::Escape,
              physical_key: None,
              pressed: true,
              repeat: false,
              modifiers: egui::Modifiers::NONE,
            }]
          } else {
            Vec::new()
          },
          ..Default::default()
        },
        |ctx| app.render_overwrite_confirmation(ctx),
      );
    }
    assert!(app.pending_conversion.is_none());
    assert!(app.worker.is_none());
    app.start_conversion(None);
    let approved = app.pending_conversion.take().unwrap();
    app.output = other.display().to_string();
    app.start_conversion(Some(approved));
    assert_eq!(app.pending_conversion.as_ref().unwrap().output, other);
    assert!(app.worker.is_none(), "a changed output needs its own confirmation");
    for path in [&output, &other] {
      assert_eq!(std::fs::read(path.join("data00-1.bin")).unwrap(), b"existing save");
    }
    assert_eq!(std::fs::read(source.join("data00-1.bin")).unwrap(), data);
    std::fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn required_ids_are_visible_without_expanding_advanced_options() {
    for source in [Platform::Steam, Platform::NintendoSwitch] {
      for target in [GuiTarget::Steam, GuiTarget::Switch] {
        let mut app = GuiApp {
          source: "source".to_owned(),
          source_platform: Some(("source".to_owned(), Some(source))),
          target,
          ..GuiApp::default()
        };
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |ctx| {
          egui::CentralPanel::default().show(ctx, |ui| app.render_conversion_options(ui, 400.0));
        });
        let labels: Vec<_> = output
          .shapes
          .iter()
          .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
          })
          .collect();
        assert_eq!(labels.contains(&"Source SteamID64 *"), source == Platform::Steam);
        assert_eq!(labels.contains(&"Target SteamID64 *"), target == GuiTarget::Steam);
        assert!(
          !labels.iter().any(|label| label.contains("Curve Index") || label.contains("non-empty"))
        );
      }
    }
  }

  #[test]
  fn hidden_switch_account_settings_are_ignored() {
    let app = GuiApp {
      source: "switch-save".to_owned(),
      source_platform: Some(("switch-save".to_owned(), Some(Platform::NintendoSwitch))),
      target: GuiTarget::Switch,
      source_steamid64: "invalid but hidden".to_owned(),
      target_steamid64: "invalid but hidden".to_owned(),
      source_curve_index: "invalid but hidden".to_owned(),
      target_curve_index: "invalid but hidden".to_owned(),
      ..GuiApp::default()
    };
    let request = app.request().unwrap();
    assert_eq!(request.source_steamid64, None);
    assert_eq!(request.target_steamid64, None);
    assert_eq!(request.source_curve_index, None);
    assert_eq!(request.target_curve_index, None);
  }

  #[test]
  fn tabs_switch_modes_in_both_themes_and_stay_disabled_while_busy() {
    for dark in [false, true] {
      for busy in [false, true] {
        let context = egui::Context::default();
        context.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
        let mut app = GuiApp::default();
        let (_sender, receiver) = mpsc::channel();
        if busy {
          app.worker = Some(receiver);
        }
        let mut target = egui::Pos2::ZERO;
        for frame in 0..4 {
          let events = match frame {
            2 => vec![
              egui::Event::PointerMoved(target),
              egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
              },
            ],
            3 => vec![egui::Event::PointerButton {
              pos: target,
              button: egui::PointerButton::Primary,
              pressed: false,
              modifiers: egui::Modifiers::NONE,
            }],
            _ => Vec::new(),
          };
          let output = context.run(
            egui::RawInput {
              screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
              )),
              events,
              ..Default::default()
            },
            |ctx| {
              egui::CentralPanel::default().show(ctx, |ui| app.render_mode_tabs(ui));
            },
          );
          if frame < 2 {
            let text = output
              .shapes
              .iter()
              .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "Manage slots" => {
                  Some(text)
                }
                _ => None,
              })
              .expect("the slot tab is always visible");
            target = text.pos + text.galley.size() / 2.0;
          }
        }
        assert_eq!(app.mode, if busy { GuiMode::Convert } else { GuiMode::ManageSlots });
      }
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

  #[test]
  fn slot_options_ignore_unrelated_conversion_target_settings() {
    let app = GuiApp {
      source_steamid64: "76561198652986089".to_owned(),
      target_steamid64: "invalid but unused here".to_owned(),
      target_curve_index: "invalid but unused here".to_owned(),
      ..GuiApp::default()
    };
    assert_eq!(app.slot_options().unwrap().steamid64, Some(76561198652986089));
  }

  #[test]
  fn finished_event_is_not_overwritten_by_normal_channel_disconnect() {
    let (sender, receiver) = mpsc::channel();
    sender.send(WorkerEvent::Finished(Ok(vec![PathBuf::from("saved")]))).unwrap();
    drop(sender);
    let mut app = GuiApp {
      worker: Some(receiver),
      output_directory: Some(PathBuf::from("output")),
      ..GuiApp::default()
    };
    app.poll_worker();
    assert_eq!(app.status, "Finished: 1 file(s) written.");
    assert_eq!(app.output_directory, Some(PathBuf::from("output")));
    assert!(app.worker.is_none());
  }

  #[test]
  fn inspected_event_remains_successful_when_worker_exits() {
    let mut app = GuiApp::default();
    let (sender, receiver) = mpsc::channel();
    sender
      .send(WorkerEvent::Inspected(
        app.slot_input_key(),
        Ok(SlotInspection {
          platform: mhrise_save_converter::format::Platform::Steam,
          curve_index: Some(67),
          slots: Vec::new(),
        }),
      ))
      .unwrap();
    drop(sender);
    app.worker = Some(receiver);
    app.poll_worker();
    assert!(app.status.is_empty());
    assert!(app.slots.is_some());
    assert!(app.worker.is_none());
  }

  #[test]
  fn saving_requires_fresh_inspection_and_a_moved_character() {
    let mut app = GuiApp::default();
    assert!(!app.can_save_slots());
    app.slots = Some(SlotInspection {
      platform: mhrise_save_converter::format::Platform::Steam,
      curve_index: Some(67),
      slots: (1..=3)
        .map(|number| mhrise_save_converter::slots::SlotSummary {
          number,
          name: (number == 1).then(|| "Hunter".to_owned()),
          hunter_rank: 1,
          master_rank: 1,
          playtime_seconds: 0.0,
        })
        .collect(),
    });
    app.slots_key = Some(app.slot_input_key());
    assert!(!app.can_save_slots());
    app.slot_order = [1, 3, 2];
    assert!(!app.can_save_slots());
    app.slot_order = [3, 2, 1];
    assert!(app.can_save_slots());
    app.source = "changed".to_owned();
    assert!(!app.can_save_slots());
  }

  #[test]
  fn arrows_preview_movement_into_slot_three_without_radio_selections() {
    let report = SlotInspection {
      platform: mhrise_save_converter::format::Platform::Steam,
      curve_index: Some(67),
      slots: (1..=3)
        .map(|number| mhrise_save_converter::slots::SlotSummary {
          number,
          name: (number != 3).then(|| format!("Hunter {number}")),
          hunter_rank: 280,
          master_rank: 122,
          playtime_seconds: 34.0,
        })
        .collect(),
    };
    let mut app = GuiApp { slots: Some(report), ..GuiApp::default() };
    app.slots_key = Some(app.slot_input_key());
    app.move_slot(1, false);
    assert_eq!(app.slot_order, [1, 3, 2]);
    assert!(app.can_save_slots());
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |ctx| {
      egui::CentralPanel::default().show(ctx, |ui| app.render_slot_controls(ui, 300.0));
    });
    let labels: Vec<_> = output
      .shapes
      .iter()
      .filter_map(|shape| match &shape.shape {
        egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
        _ => None,
      })
      .collect();
    for label in ["Slot 1", "Slot 2", "Slot 3", "Empty", "↑", "↓"] {
      assert!(labels.contains(&label), "the table must show {label}");
    }
    for removed in ["Select", "Here", "Move character", "Destination"] {
      assert!(!labels.contains(&removed));
    }
    let position = |label: &str| {
      output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
          egui::epaint::Shape::Text(text) if text.galley.job.text == label => Some(text.pos.y),
          _ => None,
        })
        .unwrap()
    };
    assert!(position("Hunter 1") < position("Empty"));
    assert!(position("Empty") < position("Hunter 2"));
    assert!(!labels.iter().any(|label| label.starts_with("Select the character")
      || label.starts_with("Move:")
      || label.starts_with("Exchange:")
      || label.contains("albums")
      || label.contains("original files")));
    app.move_slot(1, true);
    assert_eq!(app.slot_order, [1, 3, 2], "empty rows cannot initiate movement");
  }

  #[test]
  fn slot_row_buttons_and_labels_are_vertically_centered() {
    for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
      for width in [680.0, 820.0] {
        let context = egui::Context::default();
        context.set_visuals(visuals.clone());
        let mut app = GuiApp {
          slots: Some(SlotInspection {
            platform: Platform::Steam,
            curve_index: Some(67),
            slots: (1..=3)
              .map(|number| mhrise_save_converter::slots::SlotSummary {
                number,
                name: (number != 3).then(|| format!("Hunter {number}")),
                hunter_rank: i32::from(number),
                master_rank: i32::from(number),
                playtime_seconds: f64::from(number),
              })
              .collect(),
          }),
          ..GuiApp::default()
        };
        app.slots_key = Some(app.slot_input_key());
        for _ in 0..3 {
          let output = context.run(
            egui::RawInput {
              screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 520.0),
              )),
              ..Default::default()
            },
            |ctx| {
              egui::CentralPanel::default().show(ctx, |ui| app.render_slot_controls(ui, 300.0));
            },
          );
          let centers = |label: &str| {
            output
              .shapes
              .iter()
              .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                  Some(text.pos.y + text.galley.size().y / 2.0)
                }
                _ => None,
              })
              .collect::<Vec<_>>()
          };
          let up = centers("↑");
          let down = centers("↓");
          for (index, name) in ["Hunter 1", "Hunter 2", "Empty"].iter().enumerate() {
            let center = centers(name)[0];
            for actual in [up[index], down[index], centers(&format!("Slot {}", index + 1))[0]] {
              assert!((actual - center).abs() < 1.0, "row {index}: {actual} vs {center}");
            }
            if index == 2 {
              assert!((centers("Empty slot")[0] - center).abs() < 1.0);
            } else {
              let progress_center = (centers(&format!("HR {} · MR {}", index + 1, index + 1))[0]
                + centers(&format!("0:00:0{}", index + 1))[0])
                / 2.0;
              assert!(
                (progress_center - center).abs() < 1.0,
                "progress row {index}: {progress_center} vs {center}"
              );
            }
          }
        }
      }
    }
  }

  #[test]
  fn clicking_down_arrow_reorders_the_preview_in_both_themes() {
    for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
      let context = egui::Context::default();
      context.set_visuals(visuals);
      let mut app = GuiApp {
        slots: Some(SlotInspection {
          platform: Platform::NintendoSwitch,
          curve_index: None,
          slots: (1..=3)
            .map(|number| mhrise_save_converter::slots::SlotSummary {
              number,
              name: (number != 3).then(|| format!("Hunter {number}")),
              hunter_rank: 1,
              master_rank: 1,
              playtime_seconds: 34.0,
            })
            .collect(),
        }),
        ..GuiApp::default()
      };
      app.slots_key = Some(app.slot_input_key());
      let mut target = egui::Pos2::ZERO;
      for frame in 0..4 {
        let events = match frame {
          2 => vec![
            egui::Event::PointerMoved(target),
            egui::Event::PointerButton {
              pos: target,
              button: egui::PointerButton::Primary,
              pressed: true,
              modifiers: egui::Modifiers::NONE,
            },
          ],
          3 => vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
          }],
          _ => Vec::new(),
        };
        let output = context.run(
          egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
              egui::Pos2::ZERO,
              egui::vec2(680.0, 520.0),
            )),
            events,
            ..Default::default()
          },
          |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.render_slot_controls(ui, 300.0));
          },
        );
        if frame < 2 {
          let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
              egui::epaint::Shape::Text(text) if text.galley.job.text == "↓" => Some(text),
              _ => None,
            })
            .nth(1)
            .expect("the second row has a down arrow");
          target = text.pos + text.galley.size() / 2.0;
        }
      }
      assert_eq!(app.slot_order, [1, 3, 2]);
      assert!(app.can_save_slots());
      assert!(app.worker.is_none(), "arrows only preview; saving starts the writer");
    }
  }

  #[test]
  fn repeated_arrow_moves_support_cycles_and_return_to_original_order() {
    let mut app = GuiApp {
      slots: Some(SlotInspection {
        platform: Platform::Steam,
        curve_index: Some(67),
        slots: (1..=3)
          .map(|number| mhrise_save_converter::slots::SlotSummary {
            number,
            name: Some(format!("Hunter {number}")),
            hunter_rank: 1,
            master_rank: 1,
            playtime_seconds: 0.0,
          })
          .collect(),
      }),
      ..GuiApp::default()
    };
    app.slots_key = Some(app.slot_input_key());
    app.move_slot(0, true);
    app.move_slot(2, false);
    app.move_slot(usize::MAX, false);
    assert_eq!(app.slot_order, [1, 2, 3]);
    app.move_slot(0, false);
    app.move_slot(1, false);
    assert_eq!(app.slot_order, [2, 3, 1]);
    assert!(app.can_save_slots());
    app.move_slot(2, true);
    app.move_slot(1, true);
    assert_eq!(app.slot_order, [1, 2, 3]);
    assert!(!app.can_save_slots());
    app.source = "changed".to_owned();
    app.move_slot(0, false);
    assert_eq!(app.slot_order, [1, 2, 3]);
    app.slots_key = Some(app.slot_input_key());
    let (_sender, receiver) = mpsc::channel();
    app.worker = Some(receiver);
    app.move_slot(0, false);
    assert_eq!(app.slot_order, [1, 2, 3]);
  }
}
