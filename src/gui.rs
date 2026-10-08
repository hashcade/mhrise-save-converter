#[path = "gui/view.rs"]
mod view;
pub use view::run;

#[cfg(feature = "gui-test")]
#[allow(unused_imports)]
pub use view::run_visual_checks;

use mhrise_save_converter::format::{Platform, parse_header};
use std::{
  io::Read,
  path::{Path, PathBuf},
  process::Command,
  sync::mpsc::{self, Receiver, TryRecvError},
  thread,
};

use mhrise_save_converter::conversion::{
  ConversionProgress, ConversionRequest, PreflightReport, TargetPlatform,
  convert_path_with_progress, preflight_path,
};
use mhrise_save_converter::slots::{
  SlotInspection, SlotOptions, SlotSummary, edit_slots, inspect_slots,
};

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

#[derive(Debug, Clone)]
struct PendingSlotDeletion {
  key: SlotInputKey,
  order: [u8; 3],
  index: usize,
  name: String,
}

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

pub struct GuiModel {
  mode: GuiMode,
  slots: Option<SlotInspection>,
  slots_key: Option<SlotInputKey>,
  slot_order: [u8; 3],
  deleted_slots: [bool; 3],
  pending_slot_deletion: Option<PendingSlotDeletion>,
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

impl Default for GuiModel {
  fn default() -> Self {
    Self {
      mode: GuiMode::Convert,
      slots: None,
      slots_key: None,
      slot_order: [1, 2, 3],
      deleted_slots: [false; 3],
      pending_slot_deletion: None,
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

impl GuiModel {
  fn invalidate_slots(&mut self) {
    if self.slots_key.as_ref().is_some_and(|key| key != &self.slot_input_key()) {
      self.slots = None;
      self.slots_key = None;
      self.slot_order = [1, 2, 3];
      self.deleted_slots = [false; 3];
      self.pending_slot_deletion = None;
    }
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
        self.deleted_slots.iter().any(|deleted| *deleted)
          || self.slot_order.iter().enumerate().any(|(index, from)| {
            usize::from(*from) != index + 1
              && report.slots.iter().any(|slot| slot.number == *from && slot.occupied())
          })
      })
  }

  fn slot_character(&self, index: usize) -> Option<&SlotSummary> {
    let from = *self.slot_order.get(index)?;
    self.slots.as_ref()?.slots.iter().find(|slot| {
      slot.number == from && slot.occupied() && !self.deleted_slots[usize::from(from - 1)]
    })
  }

  fn move_slot(&mut self, index: usize, up: bool) {
    if self.worker.is_some() || self.slots_key.as_ref() != Some(&self.slot_input_key()) {
      return;
    }
    if self.slot_character(index).is_none() {
      return;
    }
    let adjacent = if up { index.checked_sub(1) } else { index.checked_add(1) };
    if let Some(adjacent) = adjacent.filter(|other| *other < self.slot_order.len()) {
      self.slot_order.swap(index, adjacent);
    }
  }

  fn request_slot_deletion(&mut self, index: usize) {
    if self.worker.is_some() || self.slots_key.as_ref() != Some(&self.slot_input_key()) {
      return;
    }
    let Some(slot) = self.slot_character(index) else { return };
    self.pending_slot_deletion = Some(PendingSlotDeletion {
      key: self.slot_input_key(),
      order: self.slot_order,
      index,
      name: slot.name.clone().unwrap_or_default(),
    });
  }

  fn confirm_slot_deletion(&mut self, pending: PendingSlotDeletion) {
    if self.worker.is_none()
      && pending.key == self.slot_input_key()
      && self.slots_key.as_ref() == Some(&pending.key)
      && pending.order == self.slot_order
      && self.slot_character(pending.index).and_then(|slot| slot.name.as_ref())
        == Some(&pending.name)
    {
      self.deleted_slots[usize::from(self.slot_order[pending.index] - 1)] = true;
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
    self.deleted_slots = [false; 3];
    self.pending_slot_deletion = None;
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
        return Err(
          "Read the current save and move or delete a character before saving.".to_owned(),
        );
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
    let deleted = self.deleted_slots;
    let (sender, receiver) = mpsc::channel();
    self.preflight = None;
    self.progress = None;
    self.output_directory = Some(output.clone());
    self.status = "Saving slot changes…".to_owned();
    self.worker = Some(receiver);
    thread::spawn(move || {
      let result =
        edit_slots(&source, &output, order, deleted, options).map_err(|error| format!("{error:#}"));
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
              self.deleted_slots = [false; 3];
              self.pending_slot_deletion = None;
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
}

fn optional_number<T: std::str::FromStr>(value: &str, label: &str) -> Result<Option<T>, String> {
  if value.trim().is_empty() {
    Ok(None)
  } else {
    value.trim().parse().map(Some).map_err(|_| format!("{label} must be a number"))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
    let mut app = GuiModel {
      source: source.display().to_string(),
      output: output.display().to_string(),
      target: GuiTarget::Switch,
      ..GuiModel::default()
    };
    app.start_conversion(None);
    assert!(app.pending_conversion.is_some());
    assert!(app.worker.is_none(), "do not start writing before confirmation");
    app.pending_conversion = None;
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
  fn hidden_switch_account_settings_are_ignored() {
    let app = GuiModel {
      source: "switch-save".to_owned(),
      source_platform: Some(("switch-save".to_owned(), Some(Platform::NintendoSwitch))),
      target: GuiTarget::Switch,
      source_steamid64: "invalid but hidden".to_owned(),
      target_steamid64: "invalid but hidden".to_owned(),
      source_curve_index: "invalid but hidden".to_owned(),
      target_curve_index: "invalid but hidden".to_owned(),
      ..GuiModel::default()
    };
    let request = app.request().unwrap();
    assert_eq!(request.source_steamid64, None);
    assert_eq!(request.target_steamid64, None);
    assert_eq!(request.source_curve_index, None);
    assert_eq!(request.target_curve_index, None);
  }

  #[test]
  fn default_request_targets_steam_without_optional_values() {
    let app = GuiModel::default();
    let request = app.request().expect("default GUI state should be parseable");

    assert_eq!(request.target, TargetPlatform::Steam);
    assert_eq!(request.source_steamid64, None);
    assert_eq!(request.target_reference, None);
    assert!(!request.force);
    assert!(app.status.is_empty());
  }

  #[test]
  fn request_rejects_non_numeric_steam_id() {
    let app = GuiModel { target_steamid64: "not-a-steamid".to_owned(), ..GuiModel::default() };

    let error = app.request().expect_err("invalid SteamID64 should be rejected");

    assert_eq!(error, "Target SteamID64 must be a number");
  }

  #[test]
  fn paths_require_source_and_output() {
    let app = GuiModel::default();
    assert_eq!(
      app.paths().expect_err("source is required"),
      "Choose a source save directory first."
    );

    let app = GuiModel { source: "/tmp/source".to_owned(), ..GuiModel::default() };
    assert_eq!(app.paths().expect_err("output is required"), "Choose an output directory first.");
  }

  #[test]
  fn slot_options_ignore_unrelated_conversion_target_settings() {
    let app = GuiModel {
      source_steamid64: "76561198652986089".to_owned(),
      target_steamid64: "invalid but unused here".to_owned(),
      target_curve_index: "invalid but unused here".to_owned(),
      ..GuiModel::default()
    };
    assert_eq!(app.slot_options().unwrap().steamid64, Some(76561198652986089));
  }

  #[test]
  fn finished_event_is_not_overwritten_by_normal_channel_disconnect() {
    let (sender, receiver) = mpsc::channel();
    sender.send(WorkerEvent::Finished(Ok(vec![PathBuf::from("saved")]))).unwrap();
    drop(sender);
    let mut app = GuiModel {
      worker: Some(receiver),
      output_directory: Some(PathBuf::from("output")),
      ..GuiModel::default()
    };
    app.poll_worker();
    assert_eq!(app.status, "Finished: 1 file(s) written.");
    assert_eq!(app.output_directory, Some(PathBuf::from("output")));
    assert!(app.worker.is_none());
  }

  #[test]
  fn inspected_event_remains_successful_when_worker_exits() {
    let mut app = GuiModel::default();
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
    let mut app = GuiModel::default();
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
    let mut app = GuiModel { slots: Some(report), ..GuiModel::default() };
    app.slots_key = Some(app.slot_input_key());
    app.move_slot(1, false);
    assert_eq!(app.slot_order, [1, 3, 2]);
    assert!(app.can_save_slots());
    app.move_slot(1, true);
    assert_eq!(app.slot_order, [1, 3, 2], "empty rows cannot initiate movement");
  }

  #[test]
  fn deletion_requires_confirmation_and_tracks_original_identity_after_moves() {
    let mut app = slot_deletion_app();
    assert!(!app.can_save_slots());
    app.request_slot_deletion(2);
    assert!(app.pending_slot_deletion.is_none(), "cannot delete an empty slot");
    app.request_slot_deletion(usize::MAX);
    assert!(app.pending_slot_deletion.is_none());
    app.move_slot(1, false);
    app.request_slot_deletion(2);
    assert_eq!(app.pending_slot_deletion.as_ref().unwrap().name, "Hunter 2");
    assert_eq!(app.deleted_slots, [false; 3]);
    let pending = app.pending_slot_deletion.take().unwrap();
    app.confirm_slot_deletion(pending);
    assert_eq!(app.deleted_slots, [false, true, false]);
    assert!(app.slot_character(2).is_none());
    app.move_slot(2, true);
    assert_eq!(app.slot_order, [1, 3, 2], "deleted characters cannot move");
    app.move_slot(0, false);
    assert_eq!(app.slot_order, [3, 1, 2]);
    app.request_slot_deletion(1);
    let pending = app.pending_slot_deletion.take().unwrap();
    app.confirm_slot_deletion(pending);
    assert!(app.can_save_slots());
    assert!((0..3).all(|index| app.slot_character(index).is_none()));
    assert!(app.worker.is_none(), "confirmation changes preview only");
  }

  fn slot_deletion_app() -> GuiModel {
    let mut app = GuiModel {
      slots: Some(SlotInspection {
        platform: Platform::Steam,
        curve_index: Some(67),
        slots: (1..=3)
          .map(|number| SlotSummary {
            number,
            name: (number != 3).then(|| format!("Hunter {number}")),
            hunter_rank: 1,
            master_rank: 1,
            playtime_seconds: 34.0,
          })
          .collect(),
      }),
      ..GuiModel::default()
    };
    app.slots_key = Some(app.slot_input_key());
    app
  }

  #[test]
  fn deletion_approval_cannot_follow_changed_source_order_or_running_worker() {
    for change in 0..3 {
      let mut app = slot_deletion_app();
      app.request_slot_deletion(0);
      let pending = app.pending_slot_deletion.take().unwrap();
      match change {
        0 => app.source = "changed".to_owned(),
        1 => app.move_slot(0, false),
        _ => {
          let (_sender, receiver) = mpsc::channel();
          app.worker = Some(receiver);
        }
      }
      app.confirm_slot_deletion(pending);
      assert_eq!(app.deleted_slots, [false; 3]);
    }
  }

  #[test]
  fn repeated_arrow_moves_support_cycles_and_return_to_original_order() {
    let mut app = GuiModel {
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
      ..GuiModel::default()
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
