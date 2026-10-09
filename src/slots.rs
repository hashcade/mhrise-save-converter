//! Coordinated character-slot edits. Source files and account identities are never changed.

mod empty_summary;

use std::{
  collections::BTreeMap,
  fs,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};

use crate::{
  conversion::{
    TargetPlatform, class_stream_offset, find_curve_index, pack_payload, unpack_payload,
  },
  discover::{is_auxiliary_filename, is_core_filename},
  format::{ChecksumStatus, Platform, checksum_status, parse_header},
  payload::{
    ArrayValue, Field, FieldValue, SavePayload, array_ranges, encode_class_at_offset, field_ranges,
  },
};

const SYSTEM_FILE: &str = "data00-1.bin";
const LOAD_INFO: u32 = 0x0b19_f75b;
const HUNTERS: u32 = 0x01b0_5969;
const DETAIL: u32 = 0x85e9_04c1;
const SLOT_LABEL: u32 = 0xbc92_2b61;
const SLOT_NUMBER: u32 = 0x2794_5a5a;
const CONSISTENCY: u32 = 0xe40f_c0cd;
const CHARACTER: u32 = 0x356f_270b;
const HUNTER_RECORD: u32 = 0x355c_8c4f;
const HUNTER_ID: u32 = 0x0a96_0102;
const NAME: u32 = 0xf79f_3af6;
const HR: u32 = 0x5653_c091;
const MR: u32 = 0x4e42_8685;
const PLAYTIME: u32 = 0x74c5_61bf;

#[derive(Debug, Clone, Copy, Default)]
pub struct SlotOptions {
  pub steamid64: Option<u64>,
  pub curve_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotSummary {
  pub number: u8,
  pub name: Option<String>,
  pub hunter_rank: i32,
  pub master_rank: i32,
  pub playtime_seconds: f64,
}

impl SlotSummary {
  pub fn occupied(&self) -> bool {
    self.name.is_some()
  }

  pub fn playtime(&self) -> String {
    let seconds = self.playtime_seconds as u64;
    format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
  }
}

#[derive(Debug, Clone)]
pub struct SlotInspection {
  pub platform: Platform,
  pub curve_index: Option<usize>,
  pub slots: Vec<SlotSummary>,
}

struct Bundle {
  files: BTreeMap<String, Vec<u8>>,
  system: SavePayload,
  system_plain: Vec<u8>,
  roles: BTreeMap<u8, (Vec<u8>, SavePayload)>,
  inspection: SlotInspection,
  options: SlotOptions,
}

pub fn inspect_slots(input: &Path, options: SlotOptions) -> Result<SlotInspection> {
  Ok(load_bundle(input, options)?.inspection)
}

/// Exchange two positions, including moving a character into an empty position.
/// Writes a complete bundle to a new directory; there is deliberately no overwrite option.
pub fn swap_slots(
  input: &Path,
  output: &Path,
  first: u8,
  second: u8,
  options: SlotOptions,
) -> Result<Vec<PathBuf>> {
  validate_pair(first, second)?;
  let mut order = [1, 2, 3];
  order.swap(usize::from(first - 1), usize::from(second - 1));
  reorder_slots(input, output, order, options)
}

/// Write all three destination slots in the requested original-slot order.
/// For example, `[2, 3, 1]` moves original slot 2 to 1, 3 to 2, and 1 to 3.
pub fn reorder_slots(
  input: &Path,
  output: &Path,
  order: [u8; 3],
  options: SlotOptions,
) -> Result<Vec<PathBuf>> {
  edit_slots(input, output, order, [false; 3], options)
}

/// Apply a permutation and deletions together. `deleted` refers to ORIGINAL slot numbers.
/// Deleted characters and their album namespace are omitted only from the new output bundle.
pub fn edit_slots(
  input: &Path,
  output: &Path,
  order: [u8; 3],
  deleted: [bool; 3],
  options: SlotOptions,
) -> Result<Vec<PathBuf>> {
  let mut sorted = order;
  sorted.sort_unstable();
  ensure!(sorted == [1, 2, 3], "slot order must contain each position 1–3 exactly once");
  let source = fs::canonicalize(input).context("could not resolve source directory")?;
  ensure_output_absent(output)?;
  let parent =
    output.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
  let parent = fs::canonicalize(parent).context("output parent directory must already exist")?;
  ensure!(!parent.starts_with(&source), "output must be outside the source save directory");
  let mut bundle = load_bundle(&source, options)?;
  for (index, delete) in deleted.iter().enumerate() {
    ensure!(
      !delete || bundle.inspection.slots[index].occupied(),
      "slot {} is already empty",
      index + 1
    );
  }
  ensure!(
    deleted.iter().any(|delete| *delete)
      || order.iter().enumerate().any(|(to, from)| {
        usize::from(*from - 1) != to && bundle.inspection.slots[usize::from(*from - 1)].occupied()
      }),
    "slot edit does not move or delete any character"
  );
  let offset = class_stream_offset(bundle.inspection.platform);
  let mut expected = bundle.system.clone();
  let FieldValue::Array(hunters) = &mut field_mut(&mut expected, LOAD_INFO, HUNTERS)?.value else {
    bail!("hunter summaries are not an array");
  };
  let empty = if deleted.iter().any(|delete| *delete) {
    let source_empty = bundle.inspection.slots.iter().position(|slot| !slot.occupied());
    let empty = match source_empty {
      Some(index) => {
        let ArrayValue::Class(class) = &hunters.values[index] else {
          bail!("invalid empty summary")
        };
        (**class).clone()
      }
      None => empty_summary::builtin(bundle.inspection.platform)?,
    };
    for (index, delete) in deleted.iter().enumerate() {
      if *delete {
        let ArrayValue::Class(class) = &hunters.values[index] else {
          bail!("invalid hunter summary")
        };
        ensure!(
          empty_summary::compatible(class, &empty),
          "unsupported hunter-summary schema for deletion; no files written"
        );
      }
    }
    Some(empty)
  } else {
    None
  };
  hunters.values = order.map(|from| hunters.values[usize::from(from - 1)].clone()).to_vec();
  if let Some(hashes) = &mut hunters.class_hashes {
    *hashes = order.map(|from| hashes[usize::from(from - 1)]).to_vec();
  }
  if let Some(empty) = empty {
    for (index, from) in order.iter().enumerate() {
      if deleted[usize::from(*from - 1)] {
        hunters.values[index] = ArrayValue::Class(Box::new(empty.clone()));
        if let Some(hashes) = &mut hunters.class_hashes {
          hashes[index] = empty.hash;
        }
      }
    }
  }
  let system_plain = rewrite_summaries(&bundle.system_plain, &expected, offset, order, deleted)?;
  let system_bytes = repack(&system_plain, &bundle)?;
  bundle.files.insert(SYSTEM_FILE.to_owned(), system_bytes);

  for (index, from) in order.iter().copied().enumerate() {
    let to = index as u8 + 1;
    if from != to
      && !deleted[usize::from(from - 1)]
      && let Some((plain, role)) = bundle.roles.get(&from)
    {
      let moved = move_role(plain, role, offset, from, to)?;
      let packed = repack(&moved, &bundle)?;
      // Retain the source name until the single simultaneous filename permutation below.
      bundle.files.insert(role_filename(from), packed);
    }
  }
  let mut output_files = BTreeMap::new();
  for (name, bytes) in bundle.files {
    if (1..=3).any(|slot| {
      deleted[usize::from(slot - 1)]
        && (name == role_filename(slot) || name.starts_with(&format!("SS{slot}_")))
    }) {
      continue;
    }
    let new_name = reordered_filename(&name, order);
    ensure!(
      output_files.insert(new_name, bytes).is_none(),
      "slot edit produced a filename collision"
    );
  }
  write_bundle(&parent, output, output_files, bundle.options)
}

fn validate_pair(first: u8, second: u8) -> Result<()> {
  ensure!(
    (1..=3).contains(&first) && (1..=3).contains(&second),
    "slot numbers must be between 1 and 3"
  );
  ensure!(first != second, "choose two different slots");
  Ok(())
}

fn ensure_output_absent(output: &Path) -> Result<()> {
  match fs::symlink_metadata(output) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error).context("could not inspect output path"),
    Ok(_) => bail!("slot edits require a new output directory; overwrite is not supported"),
  }
}

fn checked_header(bytes: &[u8]) -> Result<crate::format::DsssHeader> {
  ensure!(matches!(checksum_status(bytes)?, ChecksumStatus::Valid), "invalid outer checksum");
  parse_header(bytes).context("invalid DSSS header")
}

fn load_bundle(input: &Path, mut options: SlotOptions) -> Result<Bundle> {
  ensure!(input.is_dir(), "slot operations require the complete save directory");
  let mut files = BTreeMap::new();
  for entry in fs::read_dir(input)? {
    let entry = entry?;
    ensure!(
      entry.file_type()?.is_file(),
      "save directory contains a subdirectory or symlink; refusing to omit it"
    );
    let name = entry
      .file_name()
      .into_string()
      .map_err(|_| anyhow::anyhow!("save filename is not valid UTF-8"))?;
    let bytes = fs::read(entry.path())?;
    if is_core_filename(&name) || is_auxiliary_filename(&name) {
      checked_header(&bytes).with_context(|| format!("invalid save file {name}"))?;
    }
    files.insert(name, bytes);
  }
  let system_bytes = files.get(SYSTEM_FILE).context("missing data00-1.bin system save")?;
  let header = checked_header(system_bytes)?;
  let platform = header.platform();
  ensure!(
    matches!(platform, Platform::Steam | Platform::NintendoSwitch),
    "unsupported core save platform"
  );
  if platform == Platform::Steam {
    let id =
      options.steamid64.context("Steam slot operations require the current account's SteamID64")?;
    if options.curve_index.is_none() {
      options.curve_index = Some(find_curve_index(&input.join(SYSTEM_FILE), id)?);
    }
  }
  let system_plain = unpack_payload(system_bytes, header, options.steamid64, options.curve_index)?;
  let offset = class_stream_offset(platform);
  let system = SavePayload::parse_at_offset(&system_plain, offset)?;
  let FieldValue::Array(hunters) = &field(&system, LOAD_INFO, HUNTERS)?.value else {
    bail!("hunter summaries are not an array");
  };
  ensure!(hunters.values.len() == 3, "expected exactly three hunter summaries");
  if let Some(hashes) = &hunters.class_hashes {
    ensure!(hashes.len() == 3, "invalid hunter summary class-hash table");
  }
  let mut slots = Vec::new();
  let mut roles = BTreeMap::new();
  for number in 1..=3 {
    let ArrayValue::Class(summary) = &hunters.values[usize::from(number - 1)] else {
      bail!("slot {number} summary is not a class");
    };
    let get = |hash| class_field(summary, hash);
    let hr = scalar_i32(get(HR)?)?;
    let mr = scalar_i32(get(MR)?)?;
    let FieldValue::Scalar { size: 8, bytes: playtime } = &get(PLAYTIME)?.value else {
      bail!("slot {number} has an unsupported playtime field");
    };
    let playtime = f64::from_le_bytes(playtime.as_slice().try_into()?);
    ensure!(playtime.is_finite() && playtime >= 0.0, "slot {number} has invalid playtime");
    // Switch summaries do not contain Steam's character-consistency ID.
    let summary_id =
      if platform == Platform::Steam { Some(character_id(get(CONSISTENCY)?)?) } else { None };
    let occupied = hr >= 0 && mr >= 0;
    let consistent = match summary_id {
      Some(guid) => guid.iter().any(|byte| *byte != 0) == occupied,
      None => occupied || (hr == -1 && mr == -1),
    };
    ensure!(consistent, "slot {number} summary has inconsistent empty-slot markers");
    let name = role_filename(number);
    if let Some(bytes) = files.get(&name) {
      ensure!(
        occupied,
        "slot {number} is marked empty but has a character file; refusing ambiguous data"
      );
      let role_header = checked_header(bytes)?;
      ensure!(role_header.platform() == platform, "slot {number} uses a different platform");
      let plain = unpack_payload(bytes, role_header, options.steamid64, options.curve_index)?;
      let role = SavePayload::parse_at_offset(&plain, offset)?;
      let role_id = if platform == Platform::Steam {
        field(&role, CHARACTER, CONSISTENCY)?
      } else {
        field(&role, HUNTER_RECORD, HUNTER_ID)?
      };
      let role_id = character_id(role_id)?;
      ensure!(role_id.iter().any(|byte| *byte != 0), "slot {number} has an empty character ID");
      if let Some(summary_id) = summary_id {
        ensure!(role_id == summary_id, "slot {number} summary and character IDs differ");
      }
      validate_role_position(&role, number)?;
      roles.insert(number, (plain, role));
    } else {
      ensure!(!occupied, "slot {number} is occupied but its character file is missing");
    }
    if !occupied {
      ensure!(
        !files.keys().any(|name| name.starts_with(&format!("SS{number}_"))),
        "empty slot {number} has orphaned album files"
      );
    }
    slots.push(SlotSummary {
      number,
      name: if occupied { Some(string(get(NAME)?)?) } else { None },
      hunter_rank: hr,
      master_rank: mr,
      playtime_seconds: playtime,
    });
  }
  Ok(Bundle {
    files,
    system,
    system_plain,
    roles,
    inspection: SlotInspection { platform, curve_index: options.curve_index, slots },
    options,
  })
}

fn field(save: &SavePayload, native: u32, hash: u32) -> Result<&Field> {
  let mut entries = save.entries.iter().filter(|entry| entry.native_hash == native);
  let entry = entries.next().with_context(|| format!("missing class {native:08x}"))?;
  ensure!(entries.next().is_none(), "duplicate class {native:08x}");
  class_field(&entry.class, hash)
}

fn class_field(class: &crate::payload::Class, hash: u32) -> Result<&Field> {
  let mut fields = class.fields.iter().filter(|field| field.hash == hash);
  let field = fields.next().with_context(|| format!("missing field {hash:08x}"))?;
  ensure!(fields.next().is_none(), "duplicate field {hash:08x}");
  Ok(field)
}

fn field_mut(save: &mut SavePayload, native: u32, hash: u32) -> Result<&mut Field> {
  field(save, native, hash)?;
  Ok(
    save
      .entries
      .iter_mut()
      .find(|entry| entry.native_hash == native)
      .unwrap()
      .class
      .fields
      .iter_mut()
      .find(|field| field.hash == hash)
      .unwrap(),
  )
}

fn scalar_i32(field: &Field) -> Result<i32> {
  let FieldValue::Scalar { size: 4, bytes } = &field.value else {
    bail!("field {:08x} is not a 32-bit value", field.hash);
  };
  Ok(i32::from_le_bytes(bytes.as_slice().try_into()?))
}

fn character_id(field: &Field) -> Result<&[u8]> {
  let FieldValue::Scalar { size: 16, bytes } = &field.value else {
    bail!("unsupported character ID field {:08x}", field.hash);
  };
  ensure!(bytes.len() == 16, "invalid character ID length");
  Ok(bytes)
}

fn string(field: &Field) -> Result<String> {
  let FieldValue::String(units) = &field.value else {
    bail!("field {:08x} is not a string", field.hash);
  };
  String::from_utf16(units).context("invalid UTF-16 character name or slot label")
}

fn validate_role_position(role: &SavePayload, number: u8) -> Result<()> {
  ensure!(
    scalar_i32(field(role, DETAIL, SLOT_NUMBER)?)? == i32::from(number),
    "character file has a mismatched slot number"
  );
  ensure!(
    string(field(role, DETAIL, SLOT_LABEL)?)? == format!("キャラスロット{}", number - 1),
    "character file has a mismatched slot label"
  );
  Ok(())
}

fn move_role(plain: &[u8], role: &SavePayload, offset: usize, from: u8, to: u8) -> Result<Vec<u8>> {
  validate_role_position(role, from)?;
  let mut expected = role.clone();
  field_mut(&mut expected, DETAIL, SLOT_LABEL)?.value =
    FieldValue::String(format!("キャラスロット{}", to - 1).encode_utf16().collect());
  field_mut(&mut expected, DETAIL, SLOT_NUMBER)?.value =
    FieldValue::Scalar { size: 4, bytes: i32::from(to).to_le_bytes().to_vec() };
  let paths: &[&[u32]] = &[&[DETAIL, SLOT_LABEL], &[DETAIL, SLOT_NUMBER]];
  let ranges = field_ranges(plain, offset, paths)?;
  ensure!(ranges.len() == 2, "slot metadata is missing or duplicated");
  let mut patched = plain.to_vec();
  for (index, range) in ranges {
    let hash = paths[index][1];
    let replacement = match &field(&expected, DETAIL, hash)?.value {
      FieldValue::String(units) => {
        units.iter().flat_map(|unit| unit.to_le_bytes()).collect::<Vec<_>>()
      }
      FieldValue::Scalar { bytes, .. } => bytes.clone(),
      _ => bail!("unsupported slot metadata"),
    };
    let start = range.start + 12;
    ensure!(start + replacement.len() <= range.end, "unsupported slot metadata layout");
    patched[start..start + replacement.len()].copy_from_slice(&replacement);
  }
  ensure!(
    SavePayload::parse_at_offset(&patched, offset)? == expected,
    "slot metadata patch changed unrelated fields"
  );
  Ok(patched)
}

#[cfg(test)]
fn reorder_summaries(
  plain: &[u8],
  expected: &SavePayload,
  offset: usize,
  order: [u8; 3],
) -> Result<Vec<u8>> {
  rewrite_summaries(plain, expected, offset, order, [false; 3])
}

fn rewrite_summaries(
  plain: &[u8],
  expected: &SavePayload,
  offset: usize,
  order: [u8; 3],
  deleted: [bool; 3],
) -> Result<Vec<u8>> {
  let ranges = array_ranges(plain, offset, &[LOAD_INFO, HUNTERS])?;
  ensure!(ranges.elements.len() == 3, "expected exactly three serialized summaries");
  let mut patched = plain[..ranges.elements[0].start].to_vec();
  let FieldValue::Array(hunters) = &field(expected, LOAD_INFO, HUNTERS)?.value else {
    bail!("hunter summaries are not an array");
  };
  if let Some(hashes) = &ranges.class_hashes {
    let expected_hashes = hunters.class_hashes.as_ref().context("missing summary hashes")?;
    for (to, hash) in expected_hashes.iter().enumerate() {
      let a = hashes.start + to * 4;
      patched[a..a + 4].copy_from_slice(&hash.to_le_bytes());
    }
  }
  for from in order {
    patched.extend_from_slice(&plain[ranges.elements[usize::from(from - 1)].clone()]);
  }
  patched.extend_from_slice(&plain[ranges.elements[2].end..]);
  if SavePayload::parse_at_offset(&patched, offset).is_ok_and(|parsed| parsed == *expected) {
    return Ok(patched);
  }
  // Relocation can require different padding. Re-encode only the moved
  // summaries, preserving the untouched summary and every surrounding byte.
  patched.truncate(ranges.elements[0].start);
  for (index, value) in hunters.values.iter().enumerate() {
    if usize::from(order[index] - 1) != index || deleted[usize::from(order[index] - 1)] {
      let ArrayValue::Class(class) = value else {
        bail!("hunter summary is not a class");
      };
      patched.extend_from_slice(&encode_class_at_offset(class, offset + patched.len())?);
    } else {
      patched.extend_from_slice(&plain[ranges.elements[index].clone()]);
    }
  }
  patched.extend_from_slice(&plain[ranges.elements[2].end..]);
  if deleted.iter().any(|delete| *delete)
    && !SavePayload::parse_at_offset(&patched, offset).is_ok_and(|parsed| parsed == *expected)
  {
    // A smaller empty summary can shift the alignment of following fields.
    // Canonicalize padding only when necessary; every parsed field must still match.
    patched = expected.encode_at_offset(offset)?;
  }
  ensure!(
    SavePayload::parse_at_offset(&patched, offset)? == *expected,
    "slot edit would alter unrelated fields; unsupported save layout"
  );
  Ok(patched)
}

fn repack(plain: &[u8], bundle: &Bundle) -> Result<Vec<u8>> {
  let target = match bundle.inspection.platform {
    Platform::Steam => TargetPlatform::Steam,
    Platform::NintendoSwitch => TargetPlatform::NintendoSwitch,
    _ => bail!("unsupported platform"),
  };
  let packed = pack_payload(plain, target, bundle.options.steamid64, bundle.options.curve_index)?;
  let header = checked_header(&packed)?;
  ensure!(
    unpack_payload(&packed, header, bundle.options.steamid64, bundle.options.curve_index)? == plain,
    "repacked save failed plaintext verification"
  );
  Ok(packed)
}

fn role_filename(number: u8) -> String {
  format!("data{number:03}Slot.bin")
}

fn reordered_filename(name: &str, order: [u8; 3]) -> String {
  for (index, from) in order.iter().copied().enumerate() {
    let to = index as u8 + 1;
    if name == role_filename(from) {
      return role_filename(to);
    }
    if let Some(suffix) = name.strip_prefix(&format!("SS{from}_")) {
      return format!("SS{to}_{suffix}");
    }
  }
  name.to_owned()
}

fn write_bundle(
  parent: &Path,
  output: &Path,
  files: BTreeMap<String, Vec<u8>>,
  options: SlotOptions,
) -> Result<Vec<PathBuf>> {
  let staging = parent.join(format!(".mhrise-slot-swap-{:016x}", rand::random::<u64>()));
  fs::create_dir(&staging).context("could not create slot-swap staging directory")?;
  let result = (|| {
    for (name, bytes) in &files {
      fs::write(staging.join(name), bytes)?;
    }
    load_bundle(&staging, options)
      .context("generated bundle failed slot consistency verification")?;
    ensure_output_absent(output)?;
    fs::rename(&staging, output).context("could not publish slot-swap output directory")?;
    Ok(files.keys().map(|name| output.join(name)).collect())
  })();
  if result.is_err() {
    for name in files.keys() {
      let _ = fs::remove_file(staging.join(name));
    }
    let _ = fs::remove_dir(&staging);
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::payload::{Array, Class, NativeClass};

  fn number(hash: u32, value: i32) -> Field {
    Field {
      hash,
      field_type: 7,
      value: FieldValue::Scalar { size: 4, bytes: value.to_le_bytes().to_vec() },
    }
  }

  fn text(hash: u32, value: &str) -> Field {
    Field { hash, field_type: 15, value: FieldValue::String(value.encode_utf16().collect()) }
  }

  fn role(slot: u8) -> SavePayload {
    SavePayload {
      entries: vec![NativeClass {
        native_hash: DETAIL,
        class: Class {
          hash: 10,
          fields: vec![
            text(SLOT_LABEL, &format!("キャラスロット{}", slot - 1)),
            number(SLOT_NUMBER, i32::from(slot)),
            number(99, 123),
          ],
        },
      }],
    }
  }

  #[test]
  fn role_swap_round_trip_preserves_every_original_byte() {
    for offset in [12, 16] {
      for from in 1..=3 {
        for to in 1..=3 {
          let original = role(from);
          let plain = original.encode_at_offset(offset).unwrap();
          let moved = move_role(&plain, &original, offset, from, to).unwrap();
          let parsed = SavePayload::parse_at_offset(&moved, offset).unwrap();
          assert_eq!(field(&parsed, DETAIL, 99).unwrap(), field(&original, DETAIL, 99).unwrap());
          assert_eq!(move_role(&moved, &parsed, offset, to, from).unwrap(), plain);
        }
      }
    }
  }

  #[test]
  fn patches_only_summary_array_and_preserves_following_data() {
    for offset in [12, 16] {
      let summaries = ["short", "a longer character name", ""]
        .into_iter()
        .map(|name| ArrayValue::Class(Box::new(Class { hash: 20, fields: vec![text(NAME, name)] })))
        .collect();
      let original = SavePayload {
        entries: vec![NativeClass {
          native_hash: LOAD_INFO,
          class: Class {
            hash: 30,
            fields: vec![
              Field {
                hash: HUNTERS,
                field_type: -1,
                value: FieldValue::Array(Array {
                  member_type: 17,
                  member_size: 8,
                  array_type: 1,
                  class_hashes: Some(vec![20; 3]),
                  values: summaries,
                }),
              },
              number(99, 456),
            ],
          },
        }],
      };
      let plain = original.encode_at_offset(offset).unwrap();
      let mut expected = original.clone();
      let FieldValue::Array(array) =
        &mut field_mut(&mut expected, LOAD_INFO, HUNTERS).unwrap().value
      else {
        unreachable!()
      };
      array.values.swap(0, 2);
      let patched = reorder_summaries(&plain, &expected, offset, [3, 2, 1]).unwrap();
      let restored = reorder_summaries(&patched, &original, offset, [3, 2, 1]).unwrap();
      assert_eq!(restored, plain);
    }
  }

  #[test]
  fn filename_permutation_handles_every_pair_without_touching_photo_index() {
    for first in 1..=3 {
      for second in 1..=3 {
        if first == second {
          continue;
        }
        let mut order = [1, 2, 3];
        order.swap(usize::from(first - 1), usize::from(second - 1));
        assert_eq!(reordered_filename(&role_filename(first), order), role_filename(second));
        assert_eq!(
          reordered_filename(&format!("SS{first}_data338Slot.bin"), order),
          format!("SS{second}_data338Slot.bin")
        );
        assert_eq!(
          reordered_filename(&format!("SS{first}_data00-1.bin"), order),
          format!("SS{second}_data00-1.bin")
        );
        assert_eq!(reordered_filename("SS10_data001Slot.bin", order), "SS10_data001Slot.bin");
        assert_eq!(reordered_filename("notes.txt", order), "notes.txt");
      }
    }
    assert!(validate_pair(0, 1).is_err());
    assert!(validate_pair(1, 4).is_err());
    assert!(validate_pair(1, 1).is_err());
  }

  #[test]
  fn untouched_summary_retains_nonzero_alignment_padding() {
    let summaries = ["short", "a longer name", "untouched"]
      .into_iter()
      .map(|name| {
        ArrayValue::Class(Box::new(Class {
          hash: 20,
          fields: vec![
            text(NAME, name),
            Field {
              hash: CONSISTENCY,
              field_type: 16,
              value: FieldValue::Scalar { size: 16, bytes: vec![7; 16] },
            },
          ],
        }))
      })
      .collect();
    let original = SavePayload {
      entries: vec![NativeClass {
        native_hash: LOAD_INFO,
        class: Class {
          hash: 30,
          fields: vec![Field {
            hash: HUNTERS,
            field_type: -1,
            value: FieldValue::Array(Array {
              member_type: 17,
              member_size: 8,
              array_type: 1,
              class_hashes: Some(vec![20; 3]),
              values: summaries,
            }),
          }],
        },
      }],
    };
    let mut plain = original.encode_at_offset(16).unwrap();
    let names = field_ranges(&plain, 16, &[&[LOAD_INFO, HUNTERS, NAME]]).unwrap();
    let last_name = &names[2].1;
    plain[last_name.end - 2..last_name.end].copy_from_slice(&[0xab, 0xcd]);
    assert_eq!(SavePayload::parse_at_offset(&plain, 16).unwrap(), original);
    let mut expected = original.clone();
    let FieldValue::Array(array) = &mut field_mut(&mut expected, LOAD_INFO, HUNTERS).unwrap().value
    else {
      unreachable!()
    };
    array.values.swap(0, 1);
    let patched = reorder_summaries(&plain, &expected, 16, [2, 1, 3]).unwrap();
    let before = array_ranges(&plain, 16, &[LOAD_INFO, HUNTERS]).unwrap();
    let after = array_ranges(&patched, 16, &[LOAD_INFO, HUNTERS]).unwrap();
    assert_eq!(&plain[before.elements[2].clone()], &patched[after.elements[2].clone()]);
    assert_eq!(&plain[before.elements[2].end..], &patched[after.elements[2].end..]);
  }

  struct TestDirectory(PathBuf);

  impl TestDirectory {
    fn new() -> Self {
      let path =
        std::env::temp_dir().join(format!("mhrise-slot-test-{:016x}", rand::random::<u64>()));
      fs::create_dir(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TestDirectory {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn fixture(path: &Path, platform: TargetPlatform, occupied: &[u8]) -> SlotOptions {
    fs::create_dir(path).unwrap();
    let options = if platform == TargetPlatform::Steam {
      SlotOptions { steamid64: Some(76561198652986089), curve_index: Some(67) }
    } else {
      SlotOptions::default()
    };
    let offset = if platform == TargetPlatform::Steam { 16 } else { 12 };
    let summary_platform =
      if platform == TargetPlatform::Steam { Platform::Steam } else { Platform::NintendoSwitch };
    let mut summaries = Vec::new();
    for slot in 1..=3 {
      let used = occupied.contains(&slot);
      let id = Field {
        hash: CONSISTENCY,
        field_type: 16,
        value: FieldValue::Scalar { size: 16, bytes: vec![if used { slot } else { 0 }; 16] },
      };
      let mut summary = empty_summary::builtin(summary_platform).unwrap();
      for field in &mut summary.fields {
        field.value = match field.hash {
          NAME if used => FieldValue::String(format!("Hunter {slot}").encode_utf16().collect()),
          HR | MR if used => number(field.hash, if field.hash == HR { 280 } else { 122 }).value,
          CONSISTENCY => id.value.clone(),
          PLAYTIME if used => {
            FieldValue::Scalar { size: 8, bytes: 941_646.0_f64.to_le_bytes().to_vec() }
          }
          _ => field.value.clone(),
        };
      }
      summaries.push(ArrayValue::Class(Box::new(summary)));
      if used {
        let mut save = role(slot);
        let (native_hash, mut role_id) =
          if platform == TargetPlatform::Steam { (CHARACTER, id) } else { (HUNTER_RECORD, id) };
        if platform == TargetPlatform::NintendoSwitch {
          role_id.hash = HUNTER_ID;
        }
        save
          .entries
          .push(NativeClass { native_hash, class: Class { hash: 40, fields: vec![role_id] } });
        let plain = save.encode_at_offset(offset).unwrap();
        fs::write(
          path.join(role_filename(slot)),
          pack_payload(&plain, platform, options.steamid64, options.curve_index).unwrap(),
        )
        .unwrap();
        let photo = text(123, "photo payload stays identical");
        let photo_payload = SavePayload {
          entries: vec![NativeClass {
            native_hash: 50,
            class: Class { hash: 60, fields: vec![photo] },
          }],
        };
        let mut photo_bytes = b"DSSS".to_vec();
        photo_bytes.extend_from_slice(&2u32.to_le_bytes());
        photo_bytes.extend_from_slice(&0u32.to_le_bytes());
        photo_bytes.extend_from_slice(&photo_payload.encode_at_offset(12).unwrap());
        let hash = murmur3::murmur3_32(&mut std::io::Cursor::new(&photo_bytes), u32::MAX).unwrap();
        photo_bytes.extend_from_slice(&hash.to_le_bytes());
        fs::write(path.join(format!("SS{slot}_data338Slot.bin")), photo_bytes).unwrap();
      }
    }
    let system = SavePayload {
      entries: vec![NativeClass {
        native_hash: LOAD_INFO,
        class: Class {
          hash: 30,
          fields: vec![
            Field {
              hash: HUNTERS,
              field_type: -1,
              value: FieldValue::Array(Array {
                member_type: 17,
                member_size: 8,
                array_type: 1,
                class_hashes: Some(vec![0x520d_e8df; 3]),
                values: summaries,
              }),
            },
            number(99, 123456),
          ],
        },
      }],
    };
    fs::write(
      path.join(SYSTEM_FILE),
      pack_payload(
        &system.encode_at_offset(offset).unwrap(),
        platform,
        options.steamid64,
        options.curve_index,
      )
      .unwrap(),
    )
    .unwrap();
    fs::write(path.join("unrecognized-file.dat"), b"preserve unknown file exactly").unwrap();
    options
  }

  #[test]
  fn complete_switch_bundle_supports_all_pairs_and_empty_destinations() {
    for occupied in [vec![1, 2, 3], vec![1]] {
      let temp = TestDirectory::new();
      let source = temp.0.join("source");
      let options = fixture(&source, TargetPlatform::NintendoSwitch, &occupied);
      let before = load_bundle(&source, options).unwrap();
      let FieldValue::Array(hunters) = &field(&before.system, LOAD_INFO, HUNTERS).unwrap().value
      else {
        panic!()
      };
      for value in &hunters.values {
        let ArrayValue::Class(summary) = value else { panic!() };
        assert!(!summary.fields.iter().any(|field| field.hash == CONSISTENCY));
      }
      for (first, second) in [(1, 2), (1, 3), (2, 3)] {
        let output = temp.0.join(format!("swapped{first}{second}"));
        if !occupied.contains(&first) && !occupied.contains(&second) {
          assert!(swap_slots(&source, &output, first, second, options).is_err());
          assert!(!output.exists());
          continue;
        }
        swap_slots(&source, &output, first, second, options).unwrap();
        let after = load_bundle(&output, options).unwrap();
        assert_eq!(after.files.len(), before.files.len());
        assert_eq!(
          fs::read(output.join("unrecognized-file.dat")).unwrap(),
          b"preserve unknown file exactly"
        );
        for slot in &before.inspection.slots {
          let to = if slot.number == first {
            second
          } else if slot.number == second {
            first
          } else {
            slot.number
          };
          assert_eq!(slot.name, after.inspection.slots[usize::from(to - 1)].name);
          if slot.occupied() {
            assert_eq!(
              before.files[&format!("SS{}_data338Slot.bin", slot.number)],
              after.files[&format!("SS{to}_data338Slot.bin")]
            );
          }
        }
        let restored = temp.0.join(format!("restored{first}{second}"));
        swap_slots(&output, &restored, first, second, options).unwrap();
        let restored = load_bundle(&restored, options).unwrap();
        assert_eq!(restored.system_plain, before.system_plain);
        for (slot, (plain, _)) in &before.roles {
          assert_eq!(&restored.roles[slot].0, plain);
        }
        assert_eq!(load_bundle(&source, options).unwrap().files, before.files);
      }
    }
  }

  #[test]
  fn reorders_complete_bundles_and_albums_for_every_permutation() {
    for platform in [TargetPlatform::Steam, TargetPlatform::NintendoSwitch] {
      for occupied in [vec![1, 2, 3], vec![1, 2], vec![1]] {
        let temp = TestDirectory::new();
        let source = temp.0.join("source");
        let options = fixture(&source, platform, &occupied);
        let before = load_bundle(&source, options).unwrap();
        for order in [[2, 1, 3], [1, 3, 2], [3, 2, 1], [2, 3, 1], [3, 1, 2]] {
          let output = temp.0.join(format!("order{order:?}"));
          let moved = order
            .iter()
            .enumerate()
            .any(|(index, from)| usize::from(*from) != index + 1 && occupied.contains(from));
          if !moved {
            assert!(reorder_slots(&source, &output, order, options).is_err());
            assert!(!output.exists());
            continue;
          }
          reorder_slots(&source, &output, order, options).unwrap();
          let after = load_bundle(&output, options).unwrap();
          assert_eq!(after.files.len(), before.files.len());
          let mut inverse = [0; 3];
          for (index, from) in order.into_iter().enumerate() {
            inverse[usize::from(from - 1)] = index as u8 + 1;
            assert_eq!(
              after.inspection.slots[index].name,
              before.inspection.slots[usize::from(from - 1)].name
            );
          }
          for (name, bytes) in &before.files {
            if name.starts_with("SS") || name == "unrecognized-file.dat" {
              assert_eq!(&after.files[&reordered_filename(name, order)], bytes);
            }
          }
          let restored = temp.0.join(format!("restored{order:?}"));
          reorder_slots(&output, &restored, inverse, options).unwrap();
          let restored = load_bundle(&restored, options).unwrap();
          assert_eq!(restored.system, before.system);
          for (slot, (plain, _)) in &before.roles {
            assert_eq!(&restored.roles[slot].0, plain);
          }
        }
        assert_eq!(load_bundle(&source, options).unwrap().files, before.files);
        for order in [[1, 1, 3], [0, 2, 3], [1, 2, 4], [1, 2, 3]] {
          let output = temp.0.join(format!("invalid{order:?}"));
          assert!(reorder_slots(&source, &output, order, options).is_err());
          assert!(!output.exists());
        }
      }
    }
  }

  #[test]
  fn deletion_clears_summaries_and_omits_only_the_selected_characters_and_albums() {
    for platform in [TargetPlatform::Steam, TargetPlatform::NintendoSwitch] {
      let temp = TestDirectory::new();
      let source = temp.0.join("source");
      let options = fixture(&source, platform, &[1, 2, 3]);
      fs::write(source.join("SS10_notes.txt"), b"not slot one").unwrap();
      fs::write(source.join("SS2_notes.txt"), b"slot two auxiliary metadata").unwrap();
      let before = load_bundle(&source, options).unwrap();
      let empty =
        ArrayValue::Class(Box::new(empty_summary::builtin(before.inspection.platform).unwrap()));
      for (case, (order, deleted)) in [
        ([1, 2, 3], [true, false, false]),
        ([1, 2, 3], [false, true, false]),
        ([1, 2, 3], [false, false, true]),
        ([3, 1, 2], [true, false, false]),
        ([2, 3, 1], [true, true, false]),
        ([1, 2, 3], [true; 3]),
      ]
      .into_iter()
      .enumerate()
      {
        let output = temp.0.join(format!("deleted{case}"));
        edit_slots(&source, &output, order, deleted, options).unwrap();
        let after = load_bundle(&output, options).unwrap();
        let mut expected = before.system.clone();
        let FieldValue::Array(hunters) =
          &mut field_mut(&mut expected, LOAD_INFO, HUNTERS).unwrap().value
        else {
          panic!()
        };
        let original = hunters.values.clone();
        for (index, from) in order.into_iter().enumerate() {
          if deleted[usize::from(from - 1)] {
            hunters.values[index] = empty.clone();
            assert!(!after.inspection.slots[index].occupied());
            assert!(!after.files.contains_key(&role_filename(index as u8 + 1)));
            assert!(!after.files.keys().any(|name| name.starts_with(&format!("SS{}_", index + 1))));
          } else {
            hunters.values[index] = original[usize::from(from - 1)].clone();
            assert_eq!(
              after.inspection.slots[index].name,
              before.inspection.slots[usize::from(from - 1)].name
            );
            assert_eq!(
              after.files[&format!("SS{}_data338Slot.bin", index + 1)],
              before.files[&format!("SS{from}_data338Slot.bin")]
            );
            if usize::from(from) == index + 1 {
              assert_eq!(after.files[&role_filename(from)], before.files[&role_filename(from)]);
            }
          }
        }
        assert_eq!(after.system, expected, "only summaries may change");
        assert_eq!(after.files["SS10_notes.txt"], b"not slot one");
        assert_eq!(after.files["unrecognized-file.dat"], before.files["unrecognized-file.dat"]);
        if !deleted[1] {
          assert_eq!(
            after.files[&reordered_filename("SS2_notes.txt", order)],
            before.files["SS2_notes.txt"]
          );
        }
      }
      assert_eq!(load_bundle(&source, options).unwrap().files, before.files);
    }
  }

  #[test]
  fn deletion_reuses_source_empty_summary_and_rejects_empty_or_unknown_slots() {
    let temp = TestDirectory::new();
    let source = temp.0.join("source");
    let options = fixture(&source, TargetPlatform::NintendoSwitch, &[1]);
    let before = load_bundle(&source, options).unwrap();
    let output = temp.0.join("deleted");
    edit_slots(&source, &output, [1, 2, 3], [true, false, false], options).unwrap();
    assert!(inspect_slots(&output, options).unwrap().slots.iter().all(|slot| !slot.occupied()));
    let output = temp.0.join("empty");
    assert!(edit_slots(&source, &output, [1, 2, 3], [false, true, false], options).is_err());
    assert!(!output.exists());
    assert_eq!(load_bundle(&source, options).unwrap().files, before.files);

    let full = temp.0.join("full");
    fixture(&full, TargetPlatform::NintendoSwitch, &[1, 2, 3]);
    let mut bundle = load_bundle(&full, options).unwrap();
    let FieldValue::Array(hunters) =
      &mut field_mut(&mut bundle.system, LOAD_INFO, HUNTERS).unwrap().value
    else {
      panic!()
    };
    let ArrayValue::Class(class) = &mut hunters.values[0] else { panic!() };
    class.fields.push(number(0xdead_beef, 99));
    let plain = bundle.system.encode_at_offset(12).unwrap();
    fs::write(full.join(SYSTEM_FILE), repack(&plain, &bundle).unwrap()).unwrap();
    let output = temp.0.join("unknown");
    let error = edit_slots(&full, &output, [1, 2, 3], [true, false, false], options).unwrap_err();
    assert!(error.to_string().contains("unsupported hunter-summary schema"));
    assert!(!output.exists());
  }

  #[test]
  fn steam_swap_keeps_account_and_detects_curve_without_target_template() {
    let temp = TestDirectory::new();
    let source = temp.0.join("source");
    let options = fixture(&source, TargetPlatform::Steam, &[1]);
    let output = temp.0.join("output");
    let autodetect = SlotOptions { curve_index: None, ..options };
    swap_slots(&source, &output, 1, 3, autodetect).unwrap();
    let report = inspect_slots(&output, options).unwrap();
    assert_eq!(report.platform, Platform::Steam);
    assert_eq!(report.curve_index, Some(67));
    assert!(!report.slots[0].occupied());
    assert_eq!(report.slots[2].name.as_deref(), Some("Hunter 1"));
  }

  #[test]
  fn switch_rejects_inconsistent_markers_and_invalid_character_files_without_writing() {
    for case in ["mixed-ranks", "invalid-id", "empty-id", "wrong-position"] {
      let temp = TestDirectory::new();
      let source = temp.0.join("source");
      let options = fixture(&source, TargetPlatform::NintendoSwitch, &[1]);
      let mut bundle = load_bundle(&source, options).unwrap();
      let (filename, plain, expected_error) = if case == "mixed-ranks" {
        let FieldValue::Array(hunters) =
          &mut field_mut(&mut bundle.system, LOAD_INFO, HUNTERS).unwrap().value
        else {
          panic!()
        };
        let ArrayValue::Class(summary) = &mut hunters.values[0] else { panic!() };
        summary.fields.iter_mut().find(|field| field.hash == HR).unwrap().value =
          number(HR, -1).value;
        (
          SYSTEM_FILE.to_owned(),
          bundle.system.encode_at_offset(12).unwrap(),
          "inconsistent empty-slot markers",
        )
      } else {
        let role = &mut bundle.roles.get_mut(&1).unwrap().1;
        let error = match case {
          "invalid-id" => {
            field_mut(role, HUNTER_RECORD, HUNTER_ID).unwrap().value = number(HUNTER_ID, 1).value;
            "unsupported character ID"
          }
          "empty-id" => {
            field_mut(role, HUNTER_RECORD, HUNTER_ID).unwrap().value =
              FieldValue::Scalar { size: 16, bytes: vec![0; 16] };
            "empty character ID"
          }
          "wrong-position" => {
            field_mut(role, DETAIL, SLOT_NUMBER).unwrap().value = number(SLOT_NUMBER, 2).value;
            "mismatched slot number"
          }
          _ => unreachable!(),
        };
        (role_filename(1), role.encode_at_offset(12).unwrap(), error)
      };
      fs::write(source.join(filename), repack(&plain, &bundle).unwrap()).unwrap();
      let output = temp.0.join("output");
      let error = swap_slots(&source, &output, 1, 3, options).unwrap_err();
      assert!(error.to_string().contains(expected_error), "{case}: {error:#}");
      assert!(!output.exists());
    }
  }

  #[test]
  fn steam_still_requires_matching_summary_and_character_ids() {
    for missing in [false, true] {
      let temp = TestDirectory::new();
      let source = temp.0.join("source");
      let options = fixture(&source, TargetPlatform::Steam, &[1]);
      let mut bundle = load_bundle(&source, options).unwrap();
      let FieldValue::Array(hunters) =
        &mut field_mut(&mut bundle.system, LOAD_INFO, HUNTERS).unwrap().value
      else {
        panic!()
      };
      let ArrayValue::Class(summary) = &mut hunters.values[0] else { panic!() };
      if missing {
        summary.fields.retain(|field| field.hash != CONSISTENCY);
      } else {
        summary.fields.iter_mut().find(|field| field.hash == CONSISTENCY).unwrap().value =
          FieldValue::Scalar { size: 16, bytes: vec![9; 16] };
      }
      let plain = bundle.system.encode_at_offset(16).unwrap();
      fs::write(source.join(SYSTEM_FILE), repack(&plain, &bundle).unwrap()).unwrap();
      let error = inspect_slots(&source, options).unwrap_err();
      let expected = if missing { "missing field e40fc0cd" } else { "character IDs differ" };
      assert!(error.to_string().contains(expected), "{error:#}");
    }
  }

  #[test]
  fn refuses_overwrite_nested_output_and_inconsistent_role_without_writing() {
    let temp = TestDirectory::new();
    let source = temp.0.join("source");
    let options = fixture(&source, TargetPlatform::NintendoSwitch, &[1, 2]);
    let existing = temp.0.join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep.txt"), b"do not overwrite").unwrap();
    assert!(swap_slots(&source, &existing, 1, 2, options).is_err());
    assert_eq!(fs::read(existing.join("keep.txt")).unwrap(), b"do not overwrite");
    assert!(swap_slots(&source, &source.join("nested"), 1, 2, options).is_err());
    fs::rename(source.join(role_filename(1)), source.join("backup.dat")).unwrap();
    let output = temp.0.join("output");
    assert!(swap_slots(&source, &output, 1, 2, options).is_err());
    assert!(!output.exists());
    assert!(
      fs::read_dir(&temp.0).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".mhrise-slot-swap"))
    );
  }
}
