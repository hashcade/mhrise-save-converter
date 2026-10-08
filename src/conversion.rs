use std::{
  fs,
  io::{Read, Write},
  path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use flate2::read::DeflateDecoder;
use murmur3::murmur3_32;

use crate::{
  crypto::Citrus,
  defaults::steam_template_from_source,
  discover::{SaveFile, SaveFileKind, discover_save_files},
  format::{ChecksumStatus, DsssHeader, Platform, SaveFlags, checksum_status, parse_header},
  identity::resign_owner_identities,
  payload::SavePayload,
  translation::merge_onto_template,
};

const DSSS_HEADER_LEN: usize = 12;
const FILE_HASH_LEN: usize = 4;
const CITRUS_SIZE_FIELD_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPlatform {
  NintendoSwitch,
  Steam,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionRequest {
  pub target: TargetPlatform,
  pub source_steamid64: Option<u64>,
  pub target_steamid64: Option<u64>,
  pub source_curve_index: Option<usize>,
  pub target_curve_index: Option<usize>,
  pub target_reference: Option<PathBuf>,
  pub force: bool,
}

pub type ConversionOptions = ConversionRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionProgress {
  pub completed: usize,
  pub total: usize,
  pub current_file: PathBuf,
  pub kind: SaveFileKind,
}

pub fn convert_path(
  input: &Path,
  output: &Path,
  request: ConversionRequest,
) -> Result<Vec<PathBuf>> {
  convert_path_with_progress(input, output, request, |_| {})
}

pub fn convert_path_with_progress<F>(
  input: &Path,
  output: &Path,
  request: ConversionRequest,
  mut on_progress: F,
) -> Result<Vec<PathBuf>>
where
  F: FnMut(ConversionProgress),
{
  let files = discover_save_files(input)?;
  let total = files.len();
  let output_is_directory = input.is_dir();
  let conflicts =
    find_output_conflicts(&files, input, output, request.target_reference.as_deref())?;
  if !conflicts.is_empty() && !request.force {
    bail!("{} output file(s) already exist; use --force to overwrite them", conflicts.len());
  }
  if output_is_directory {
    fs::create_dir_all(output)
      .with_context(|| format!("could not create output directory {}", output.display()))?;
  } else if let Some(parent) = output.parent() {
    fs::create_dir_all(parent)?;
  }

  let mut options = request;
  if options.target == TargetPlatform::Steam
    && options.target_curve_index.is_none()
    && let (Some(reference), Some(steamid64)) =
      (options.target_reference.as_deref(), options.target_steamid64)
  {
    let reference_file = reference_core_file(reference)?;
    options.target_curve_index =
      Some(find_curve_index(&reference_file, steamid64).with_context(|| {
        format!("could not detect target Curve Index from {}", reference_file.display())
      })?);
  }
  let mut written = Vec::with_capacity(files.len());
  for file in files {
    if file.kind == SaveFileKind::Core
      && file.platform == Platform::Steam
      && options.source_curve_index.is_none()
    {
      let steamid64 =
        options.source_steamid64.context("Steam source requires --source-steamid64")?;
      options.source_curve_index =
        Some(find_curve_index(&file.path, steamid64).with_context(|| {
          format!("could not detect source Curve Index from {}", file.path.display())
        })?);
    }
    let output_path = if output_is_directory {
      output.join(file.path.file_name().expect("discovered file has a name"))
    } else {
      output.to_path_buf()
    };
    let data =
      fs::read(&file.path).with_context(|| format!("could not read {}", file.path.display()))?;
    if !matches!(checksum_status(&data)?, ChecksumStatus::Valid) {
      bail!("source file {} has an invalid checksum; refusing to convert it", file.path.display());
    }
    let converted = match file.kind {
      SaveFileKind::Core => {
        let target_template = options
          .target_reference
          .as_deref()
          .map(|reference| read_matching_reference(reference, &file.path))
          .transpose()?
          .flatten();
        convert_bytes_with_template(&data, target_template.as_deref(), options.clone())
      }
      SaveFileKind::Auxiliary => {
        convert_auxiliary_bytes(&data, options.target, options.target_steamid64)
      }
    }
    .with_context(|| format!("could not convert {}", file.path.display()))?;
    write_converted_file(&output_path, &converted, options.force)
      .with_context(|| format!("could not write {}", output_path.display()))?;
    written.push(output_path);
    on_progress(ConversionProgress {
      completed: written.len(),
      total,
      current_file: file.path,
      kind: file.kind,
    });
  }
  Ok(written)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreflightSeverity {
  Warning,
  Error,
}

fn write_converted_file(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
  if !overwrite {
    fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(bytes)?;
    return Ok(());
  }
  // Replace the directory entry, not the contents of a possibly hard-linked existing file.
  let parent = path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
  let staging = parent.join(format!(".mhrise-convert-{:016x}.tmp", rand::random::<u64>()));
  let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&staging)?;
  let result = file.write_all(bytes);
  drop(file);
  let result = result.and_then(|()| fs::rename(&staging, path));
  if result.is_err() {
    let _ = fs::remove_file(&staging);
  }
  result.context("could not replace converted output file")
}

fn find_output_conflicts(
  files: &[SaveFile],
  input: &Path,
  output: &Path,
  reference: Option<&Path>,
) -> Result<Vec<PathBuf>> {
  let directory = input.is_dir();
  if output.exists() && directory && !output.is_dir() {
    bail!("output must be a directory");
  }
  let mut conflicts = Vec::new();
  for file in files {
    let destination = if directory {
      output.join(file.path.file_name().context("source file has no name")?)
    } else {
      output.to_path_buf()
    };
    match fs::symlink_metadata(&destination) {
      Ok(metadata) => {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
          bail!("output {} is not a regular file", destination.display());
        }
        let canonical = fs::canonicalize(&destination)?;
        for source in files {
          if canonical == fs::canonicalize(&source.path)? {
            bail!("output must not overwrite source files");
          }
        }
        if let Some(reference) = reference {
          let template = if reference.is_dir() {
            reference.join(file.path.file_name().context("source file has no name")?)
          } else {
            reference.to_path_buf()
          };
          if template.exists() && canonical == fs::canonicalize(template)? {
            bail!("output must not overwrite target template files");
          }
        }
        conflicts.push(destination);
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error).context("could not inspect output file"),
    }
  }
  Ok(conflicts)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightIssue {
  pub severity: PreflightSeverity,
  pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
  pub source_platform: Option<Platform>,
  pub file_count: usize,
  pub core_file_count: usize,
  pub auxiliary_file_count: usize,
  pub target_reference_available: bool,
  pub output_conflicts: Vec<PathBuf>,
  pub issues: Vec<PreflightIssue>,
}

impl PreflightReport {
  pub fn errors(&self) -> impl Iterator<Item = &str> {
    self.issues.iter().filter_map(|issue| {
      (issue.severity == PreflightSeverity::Error).then_some(issue.message.as_str())
    })
  }

  pub fn warnings(&self) -> impl Iterator<Item = &str> {
    self.issues.iter().filter_map(|issue| {
      (issue.severity == PreflightSeverity::Warning).then_some(issue.message.as_str())
    })
  }

  pub fn can_convert(&self) -> bool {
    !self.issues.iter().any(|issue| issue.severity == PreflightSeverity::Error)
  }
}

pub fn preflight_path(
  input: &Path,
  output: &Path,
  request: &ConversionRequest,
) -> Result<PreflightReport> {
  let files = discover_save_files(input)?;
  let source_platform =
    files.iter().find(|file| file.kind == SaveFileKind::Core).map(|file| file.platform);
  let core_file_count = files.iter().filter(|file| file.kind == SaveFileKind::Core).count();
  let auxiliary_file_count = files.len() - core_file_count;
  let mut issues = Vec::new();

  if source_platform.is_none() {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "no core save files were found".to_owned(),
    });
  }
  if files.iter().any(|file| file.checksum != "valid") {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "one or more save files have an invalid outer checksum".to_owned(),
    });
  }
  if output == input {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "output must be different from the source".to_owned(),
    });
  }
  let output_conflicts =
    find_output_conflicts(&files, input, output, request.target_reference.as_deref())?;
  if !output_conflicts.is_empty() && !request.force {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: format!(
        "{} output file(s) already exist; confirm overwrite first",
        output_conflicts.len()
      ),
    });
  }

  if source_platform == Some(Platform::Steam) && request.source_steamid64.is_none() {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "Steam source requires a SteamID64".to_owned(),
    });
  }
  if request.target == TargetPlatform::Steam {
    if request.target_steamid64.is_none() {
      issues.push(PreflightIssue {
        severity: PreflightSeverity::Error,
        message: "Steam target requires a SteamID64".to_owned(),
      });
    }
    if request.target_reference.is_none() && request.target_curve_index.is_none() {
      issues.push(PreflightIssue {
        severity: PreflightSeverity::Error,
        message: "Steam target requires a target template or Curve Index".to_owned(),
      });
    }
    if request.target_reference.is_none() {
      issues.push(PreflightIssue {
        severity: PreflightSeverity::Warning,
        message: "no Steam template selected; built-in Steam defaults will be used".to_owned(),
      });
    }
  }
  if request.target == TargetPlatform::NintendoSwitch
    && source_platform == Some(Platform::Steam)
    && request.target_reference.is_none()
  {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "Steam-to-Switch conversion requires a Switch target template".to_owned(),
    });
  }

  let target_reference_available =
    request.target_reference.as_ref().is_some_and(|path| path.exists());
  if request.target_reference.is_some() && !target_reference_available {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "selected target template does not exist".to_owned(),
    });
  }
  if core_file_count == 0 {
    issues.push(PreflightIssue {
      severity: PreflightSeverity::Error,
      message: "the input contains no convertible core save".to_owned(),
    });
  }

  Ok(PreflightReport {
    output_conflicts,
    source_platform,
    file_count: files.len(),
    core_file_count,
    auxiliary_file_count,
    target_reference_available,
    issues,
  })
}

fn read_matching_reference(reference: &Path, source_file: &Path) -> Result<Option<Vec<u8>>> {
  let reference_file = if reference.is_dir() {
    reference.join(source_file.file_name().context("source save file has no name")?)
  } else {
    reference.to_path_buf()
  };
  if !reference_file.exists() {
    return Ok(None);
  }
  fs::read(&reference_file)
    .with_context(|| format!("could not read target template {}", reference_file.display()))
    .map(Some)
}

fn reference_core_file(reference: &Path) -> Result<PathBuf> {
  if reference.is_file() {
    return Ok(reference.to_path_buf());
  }
  let files = discover_save_files(reference)?;
  files
    .iter()
    .filter(|file| file.kind == SaveFileKind::Core)
    .find(|file| file.path.file_name().is_some_and(|name| name == "data001Slot.bin"))
    .or_else(|| files.iter().find(|file| file.kind == SaveFileKind::Core))
    .map(|file| file.path.clone())
    .context("target reference contains no usable core save file")
}

pub fn convert_bytes(data: &[u8], options: ConversionOptions) -> Result<Vec<u8>> {
  convert_bytes_with_template(data, None, options)
}

pub fn convert_bytes_with_template(
  data: &[u8],
  target_template: Option<&[u8]>,
  options: ConversionOptions,
) -> Result<Vec<u8>> {
  let header = parse_header(data).context("invalid source DSSS header")?;
  if !matches!(checksum_status(data)?, ChecksumStatus::Valid) {
    bail!("source save has an invalid checksum; refusing to convert it");
  }
  let source_platform = header.platform();
  let mut payload =
    unpack_payload(data, header, options.source_steamid64, options.source_curve_index)?;
  let requested_platform = target_platform(options.target);
  if header.platform() != requested_platform {
    let source = SavePayload::parse_at_offset(&payload, class_stream_offset(header.platform()))
      .context("invalid source class stream")?;
    let target = match target_template {
      Some(template) => {
        let target_header =
          parse_header(template).context("invalid target-template DSSS header")?;
        if target_header.platform() != requested_platform {
          bail!(
            "target template is {}, but requested output is {}",
            target_header.platform(),
            requested_platform
          );
        }
        let target_payload = unpack_payload(
          template,
          target_header,
          options.target_steamid64,
          options.target_curve_index,
        )?;
        SavePayload::parse_at_offset(&target_payload, class_stream_offset(target_header.platform()))
          .context("invalid target-template class stream")?
      }
      None if requested_platform == Platform::Steam => steam_template_from_source(&source)
        .context("could not construct the built-in Steam schema")?,
      None => bail!("cross-platform conversion to Switch currently requires --target-reference"),
    };
    payload = merge_onto_template(&source, &target)
      .0
      .encode_at_offset(class_stream_offset(requested_platform))
      .context("could not encode translated class stream")?;
  }
  if source_platform == Platform::Steam && requested_platform == Platform::Steam {
    resign_owner_identities(
      &mut payload,
      options.source_steamid64.context("Steam source requires --source-steamid64")?,
      options.target_steamid64.context("Steam target requires --target-steamid64")?,
    )
    .context("could not update the hunter's own Steam identities")?;
  }
  pack_payload(&payload, options.target, options.target_steamid64, options.target_curve_index)
    .with_context(|| format!("cannot pack {} as target format", source_platform))
}

fn target_platform(target: TargetPlatform) -> Platform {
  match target {
    TargetPlatform::NintendoSwitch => Platform::NintendoSwitch,
    TargetPlatform::Steam => Platform::Steam,
  }
}

pub(crate) fn class_stream_offset(platform: Platform) -> usize {
  match platform {
    Platform::NintendoSwitch => DSSS_HEADER_LEN,
    Platform::Steam => align_up(DSSS_HEADER_LEN, 16),
    Platform::Auxiliary | Platform::Unknown => 0,
  }
}

pub fn convert_auxiliary_bytes(
  data: &[u8],
  target: TargetPlatform,
  target_steamid64: Option<u64>,
) -> Result<Vec<u8>> {
  let header = parse_header(data).context("invalid auxiliary DSSS header")?;
  if !matches!(checksum_status(data)?, ChecksumStatus::Valid) {
    bail!("auxiliary save has an invalid checksum; refusing to convert it");
  }

  let source_flags = header.raw_flags;
  let source_payload_offset = match source_flags {
    0 => DSSS_HEADER_LEN,
    value if value == SaveFlags::HAS_ID.bits() => align_up(DSSS_HEADER_LEN, 8) + 8,
    value => bail!("unsupported auxiliary DSSS flags 0x{value:08x}"),
  };
  let payload_end = data.len().checked_sub(FILE_HASH_LEN).context("auxiliary save is too small")?;
  if payload_end < source_payload_offset {
    bail!("auxiliary save payload exceeds file bounds");
  }

  let target_payload_offset = match target {
    TargetPlatform::NintendoSwitch => DSSS_HEADER_LEN,
    TargetPlatform::Steam => align_up(DSSS_HEADER_LEN, 8) + 8,
  };
  let parsed =
    SavePayload::parse_at_offset(&data[source_payload_offset..payload_end], source_payload_offset)
      .context("invalid auxiliary class stream")?;
  let payload = if source_payload_offset == target_payload_offset {
    data[source_payload_offset..payload_end].to_vec()
  } else {
    parsed
      .encode_at_offset(target_payload_offset)
      .context("could not realign auxiliary class stream")?
  };

  let mut output = Vec::with_capacity(data.len() + 12);
  output.extend_from_slice(b"DSSS");
  output.extend_from_slice(&2u32.to_le_bytes());
  match target {
    TargetPlatform::NintendoSwitch => {
      output.extend_from_slice(&SaveFlags::empty().bits().to_le_bytes());
    }
    TargetPlatform::Steam => {
      output.extend_from_slice(&SaveFlags::HAS_ID.bits().to_le_bytes());
      output.resize(align_up(output.len(), 8), 0);
      let id = target_steamid64.context("Steam target requires --target-steamid64")?;
      output.extend_from_slice(&(id & u32::MAX as u64).to_le_bytes());
    }
  }
  debug_assert_eq!(output.len(), target_payload_offset);
  output.extend_from_slice(&payload);
  finish_file(output)
}

pub fn find_curve_index(path: &Path, steamid64: u64) -> Result<usize> {
  let data = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
  let header = parse_header(&data).context("invalid target DSSS header")?;
  if header.platform() != Platform::Steam {
    bail!("target reference is not a Steam Citrus save");
  }
  let (encrypted, decrypted_len) = citrus_payload(&data)?;
  let citrus = Citrus::new(steamid64, None);
  let params = citrus
    .brute_force_find_params(encrypted, decrypted_len)
    .context("could not determine Citrus Curve Index")?;
  Ok(params.index as usize)
}

pub(crate) fn unpack_payload(
  data: &[u8],
  header: DsssHeader,
  steamid64: Option<u64>,
  curve_index: Option<usize>,
) -> Result<Vec<u8>> {
  match header.platform() {
    Platform::NintendoSwitch => unpack_deflate(data),
    Platform::Steam => {
      let id = steamid64.context("Steam source requires --source-steamid64")?;
      let (encrypted, decrypted_len) = citrus_payload(data)?;
      Citrus::new(id, curve_index)
        .decrypt(encrypted, decrypted_len)
        .context("Citrus decryption failed; check the source SteamID64 and curve index")
    }
    other => bail!("{} is not a supported core save format", other),
  }
}

fn unpack_deflate(data: &[u8]) -> Result<Vec<u8>> {
  let metadata_offset = align_up(DSSS_HEADER_LEN, 8);
  if data.len() < metadata_offset + 24 + FILE_HASH_LEN {
    bail!("Switch save is missing its DEFLATE metadata");
  }

  let compressed_size = read_u32(data, metadata_offset + 12)? as usize;
  let decompressed_size = read_u64(data, metadata_offset + 16)? as usize;
  let compressed_offset = metadata_offset + 24;
  let compressed_end = compressed_offset
    .checked_add(compressed_size)
    .context("Switch compressed payload length overflow")?;
  if compressed_end > data.len() - FILE_HASH_LEN {
    bail!("Switch compressed payload exceeds file bounds");
  }

  let mut decoder = DeflateDecoder::new(&data[compressed_offset..compressed_end]);
  let mut payload = Vec::with_capacity(decompressed_size);
  decoder.read_to_end(&mut payload)?;
  if payload.len() != decompressed_size {
    bail!("Switch DEFLATE size mismatch: expected {decompressed_size}, got {}", payload.len());
  }
  Ok(payload)
}

fn citrus_payload(data: &[u8]) -> Result<(&[u8], usize)> {
  let payload_offset = align_up(DSSS_HEADER_LEN, 16);
  if data.len() < payload_offset + CITRUS_SIZE_FIELD_LEN + FILE_HASH_LEN {
    bail!("Steam save is missing its Citrus payload");
  }
  let size_offset = data.len() - FILE_HASH_LEN - CITRUS_SIZE_FIELD_LEN;
  let decrypted_len = read_u64(data, size_offset)? as usize;
  Ok((&data[payload_offset..size_offset], decrypted_len))
}

pub(crate) fn pack_payload(
  payload: &[u8],
  target: TargetPlatform,
  steamid64: Option<u64>,
  curve_index: Option<usize>,
) -> Result<Vec<u8>> {
  let mut output = Vec::new();
  match target {
    TargetPlatform::NintendoSwitch => {
      output.extend_from_slice(b"DSSS");
      output.extend_from_slice(&2u32.to_le_bytes());
      output.extend_from_slice(&SaveFlags::DEFLATE.bits().to_le_bytes());
      output.resize(align_up(output.len(), 8), 0);

      let compressed = compress_deflate(payload)?;
      output.extend_from_slice(&((compressed.len() as u64) + 0x10).to_le_bytes());
      output.extend_from_slice(&1u32.to_le_bytes());
      output.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
      output.extend_from_slice(&(payload.len() as u64).to_le_bytes());
      output.extend_from_slice(&compressed);
    }
    TargetPlatform::Steam => {
      let id = steamid64.context("Steam target requires --target-steamid64")?;
      let curve =
        curve_index.context("Steam target requires --target-curve-index or --target-reference")?;
      output.extend_from_slice(b"DSSS");
      output.extend_from_slice(&2u32.to_le_bytes());
      output.extend_from_slice(&SaveFlags::CITRUS.bits().to_le_bytes());
      output.resize(align_up(output.len(), 16), 0);
      let encrypted = Citrus::new(id, Some(curve))
        .encrypt(payload)
        .context("Citrus encryption failed; check the target Curve Index")?;
      output.extend_from_slice(&encrypted);
      output.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    }
  }

  finish_file(output)
}

fn finish_file(mut output: Vec<u8>) -> Result<Vec<u8>> {
  output.resize(align_up(output.len(), 4), 0);
  let hash = murmur3_32(&mut std::io::Cursor::new(&output), 0xffff_ffff)?;
  output.extend_from_slice(&hash.to_le_bytes());
  Ok(output)
}

fn compress_deflate(payload: &[u8]) -> Result<Vec<u8>> {
  use flate2::{Compression, write::DeflateEncoder};
  use std::io::Write;

  let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(5));
  encoder.write_all(payload)?;
  Ok(encoder.finish()?)
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
  let bytes = data.get(offset..offset + 4).context("unexpected end of file while reading u32")?;
  Ok(u32::from_le_bytes(bytes.try_into().expect("fixed-size slice")))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64> {
  let bytes = data.get(offset..offset + 8).context("unexpected end of file while reading u64")?;
  Ok(u64::from_le_bytes(bytes.try_into().expect("fixed-size slice")))
}

fn align_up(value: usize, alignment: usize) -> usize {
  value.div_ceil(alignment) * alignment
}

pub fn verify_file(path: &Path) -> Result<bool> {
  let data = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
  let _ = parse_header(&data)?;
  Ok(matches!(checksum_status(&data)?, crate::format::ChecksumStatus::Valid))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::payload::{Class, Field, FieldValue, NativeClass, SavePayload};

  const TEST_STEAM_ID: u64 = 76_561_198_382_766_028;

  fn switch_request(force: bool) -> ConversionRequest {
    ConversionRequest {
      target: TargetPlatform::NintendoSwitch,
      source_steamid64: None,
      target_steamid64: None,
      source_curve_index: None,
      target_curve_index: None,
      target_reference: None,
      force,
    }
  }

  #[test]
  fn output_writes_do_not_truncate_linked_originals_or_unconfirmed_files() {
    let root =
      std::env::temp_dir().join(format!("mhrise-write-test-{:016x}", rand::random::<u64>()));
    fs::create_dir(&root).unwrap();
    let original = root.join("original.bin");
    let destination = root.join("output.bin");
    fs::write(&original, b"original save").unwrap();
    fs::hard_link(&original, &destination).unwrap();
    assert!(write_converted_file(&destination, b"new save", false).is_err());
    assert_eq!(fs::read(&destination).unwrap(), b"original save");
    write_converted_file(&destination, b"new save", true).unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"new save");
    assert_eq!(fs::read(&original).unwrap(), b"original save");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 2, "no temporary files remain");
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn nonempty_output_allows_unrelated_files_but_requires_force_for_collisions() {
    let root =
      std::env::temp_dir().join(format!("mhrise-output-test-{:016x}", rand::random::<u64>()));
    let source = root.join("source");
    let output = root.join("output");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&output).unwrap();
    let data = pack_payload(b"test save", TargetPlatform::NintendoSwitch, None, None).unwrap();
    for name in ["data00-1.bin", "data001Slot.bin"] {
      fs::write(source.join(name), &data).unwrap();
    }
    fs::write(output.join("notes.txt"), b"keep this").unwrap();
    let report = preflight_path(&source, &output, &switch_request(false)).unwrap();
    assert!(report.can_convert());
    assert!(report.output_conflicts.is_empty());
    assert_eq!(convert_path(&source, &output, switch_request(false)).unwrap().len(), 2);
    assert_eq!(fs::read(output.join("notes.txt")).unwrap(), b"keep this");

    fs::remove_file(output.join("data00-1.bin")).unwrap();
    fs::write(output.join("data001Slot.bin"), b"existing save").unwrap();
    let report = preflight_path(&source, &output, &switch_request(false)).unwrap();
    assert!(!report.can_convert());
    assert_eq!(report.output_conflicts, vec![output.join("data001Slot.bin")]);
    assert!(convert_path(&source, &output, switch_request(false)).is_err());
    assert!(
      !output.join("data00-1.bin").exists(),
      "reject every collision before writing any file"
    );
    assert_eq!(fs::read(output.join("data001Slot.bin")).unwrap(), b"existing save");
    assert_eq!(convert_path(&source, &output, switch_request(true)).unwrap().len(), 2);
    assert_eq!(fs::read(output.join("data001Slot.bin")).unwrap(), data);
    assert_eq!(fs::read(output.join("notes.txt")).unwrap(), b"keep this");
    assert_eq!(fs::read(source.join("data001Slot.bin")).unwrap(), data);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn forced_conversion_never_overwrites_source_or_target_template() {
    let root =
      std::env::temp_dir().join(format!("mhrise-readonly-test-{:016x}", rand::random::<u64>()));
    let source = root.join("source");
    let template = root.join("template");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&template).unwrap();
    let data = pack_payload(b"test save", TargetPlatform::NintendoSwitch, None, None).unwrap();
    fs::write(source.join("data00-1.bin"), &data).unwrap();
    fs::write(template.join("data00-1.bin"), &data).unwrap();
    assert!(convert_path(&source, &source, switch_request(true)).is_err());
    let request =
      ConversionRequest { target_reference: Some(template.clone()), ..switch_request(true) };
    assert!(convert_path(&source, &template, request).is_err());
    assert_eq!(fs::read(source.join("data00-1.bin")).unwrap(), data);
    assert_eq!(fs::read(template.join("data00-1.bin")).unwrap(), data);
    fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn output_symlinks_are_not_overwritten_even_with_force() {
    let root =
      std::env::temp_dir().join(format!("mhrise-link-test-{:016x}", rand::random::<u64>()));
    let source = root.join("source");
    let output = root.join("output");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&output).unwrap();
    let data = pack_payload(b"test save", TargetPlatform::NintendoSwitch, None, None).unwrap();
    fs::write(source.join("data00-1.bin"), &data).unwrap();
    std::os::unix::fs::symlink(source.join("data00-1.bin"), output.join("data00-1.bin")).unwrap();
    assert!(convert_path(&source, &output, switch_request(true)).is_err());
    assert_eq!(fs::read(source.join("data00-1.bin")).unwrap(), data);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn platform_containers_roundtrip_preserve_payload() {
    let payload = b"MHRise save payload used for a conversion round trip";
    let switch = pack_payload(payload, TargetPlatform::NintendoSwitch, None, None)
      .expect("Switch packing should succeed");
    let steam = pack_payload(payload, TargetPlatform::Steam, Some(TEST_STEAM_ID), Some(0))
      .expect("Steam packing should succeed");

    assert_eq!(unpack_deflate(&switch).expect("Switch payload should deflate"), payload);
    let steam_header = parse_header(&steam).expect("Steam header should parse");
    assert_eq!(
      unpack_payload(&steam, steam_header, Some(TEST_STEAM_ID), Some(0))
        .expect("Steam payload should decrypt"),
      payload
    );
    assert!(matches!(checksum_status(&switch), Ok(ChecksumStatus::Valid)));
    assert!(matches!(checksum_status(&steam), Ok(ChecksumStatus::Valid)));
  }

  #[test]
  fn conversion_rejects_invalid_outer_checksum() {
    let mut switch = pack_payload(b"payload", TargetPlatform::NintendoSwitch, None, None)
      .expect("Switch packing should succeed");
    switch[20] ^= 0xff;

    let error = convert_bytes(
      &switch,
      ConversionOptions {
        target: TargetPlatform::NintendoSwitch,
        source_steamid64: None,
        target_steamid64: None,
        source_curve_index: None,
        target_curve_index: None,
        target_reference: None,
        force: false,
      },
    )
    .expect_err("invalid source checksum should be rejected");

    assert!(error.to_string().contains("invalid checksum"));
  }

  #[test]
  fn auxiliary_wrapper_roundtrip_realigns_class_stream() {
    let payload = SavePayload {
      entries: vec![NativeClass {
        native_hash: 0x470b_c310,
        class: Class {
          hash: 0x6ca3_b0ec,
          fields: vec![Field {
            hash: 0x77ea_ad8a,
            field_type: 8,
            value: FieldValue::Scalar {
              size: 8,
              bytes: 0x1122_3344_5566_7788u64.to_le_bytes().to_vec(),
            },
          }],
        },
      }],
    };
    let mut switch = Vec::from(&b"DSSS"[..]);
    switch.extend_from_slice(&2u32.to_le_bytes());
    switch.extend_from_slice(&SaveFlags::empty().bits().to_le_bytes());
    switch.extend_from_slice(
      &payload.encode_at_offset(DSSS_HEADER_LEN).expect("Switch auxiliary payload should encode"),
    );
    let switch = finish_file(switch).expect("Switch auxiliary fixture should be valid");
    let steam_id = 76_561_198_382_766_028u64;

    let steam = convert_auxiliary_bytes(&switch, TargetPlatform::Steam, Some(steam_id))
      .expect("Switch auxiliary conversion should succeed");
    assert_eq!(&steam[8..12], &SaveFlags::HAS_ID.bits().to_le_bytes());
    assert_eq!(&steam[16..24], &(steam_id & u32::MAX as u64).to_le_bytes());
    assert_eq!(
      SavePayload::parse_at_offset(&steam[24..steam.len() - FILE_HASH_LEN], 24)
        .expect("Steam auxiliary payload should parse"),
      payload
    );

    let roundtrip = convert_auxiliary_bytes(&steam, TargetPlatform::NintendoSwitch, None)
      .expect("Steam auxiliary conversion should succeed");
    assert_eq!(roundtrip, switch);
  }

  #[test]
  fn auxiliary_resigning_preserves_raw_payload_and_padding() {
    let payload = SavePayload {
      entries: vec![NativeClass {
        native_hash: 1,
        class: Class {
          hash: 2,
          fields: vec![Field {
            hash: 3,
            field_type: 15,
            value: FieldValue::String(vec![b'X' as u16]),
          }],
        },
      }],
    };
    let mut source = b"DSSS".to_vec();
    source.extend_from_slice(&2u32.to_le_bytes());
    source.extend_from_slice(&SaveFlags::HAS_ID.bits().to_le_bytes());
    source.resize(16, 0);
    source.extend_from_slice(&4660u64.to_le_bytes());
    source.extend_from_slice(&payload.encode_at_offset(24).unwrap());
    let len = source.len();
    source[len - 2..].copy_from_slice(&[0xa5, 0x5a]);
    let source = finish_file(source).unwrap();
    let output =
      convert_auxiliary_bytes(&source, TargetPlatform::Steam, Some(TEST_STEAM_ID)).unwrap();
    assert_eq!(&output[24..output.len() - 4], &source[24..source.len() - 4]);
    assert_eq!(&output[16..24], &(TEST_STEAM_ID & u32::MAX as u64).to_le_bytes());
    assert_eq!(checksum_status(&output).unwrap(), ChecksumStatus::Valid);
  }

  #[test]
  fn steam_to_switch_keeps_template_confirm_button_and_source_progress() {
    let fixture = |decide: u32, progress: u32| SavePayload {
      entries: vec![
        NativeClass {
          native_hash: 0xb0ca_70c9,
          class: Class {
            hash: 0x34c5_d174,
            fields: vec![Field {
              hash: 0x9428_4f65,
              field_type: 7,
              value: FieldValue::Scalar { size: 4, bytes: decide.to_le_bytes().to_vec() },
            }],
          },
        },
        NativeClass {
          native_hash: 1,
          class: Class {
            hash: 10,
            fields: vec![Field {
              hash: 100,
              field_type: 8,
              value: FieldValue::Scalar { size: 4, bytes: progress.to_le_bytes().to_vec() },
            }],
          },
        },
      ],
    };
    let source = fixture(1, 7);
    let template = fixture(0, 9);
    let steam = pack_payload(
      &source.encode_at_offset(16).unwrap(),
      TargetPlatform::Steam,
      Some(TEST_STEAM_ID),
      Some(0),
    )
    .unwrap();
    let switch = pack_payload(
      &template.encode_at_offset(12).unwrap(),
      TargetPlatform::NintendoSwitch,
      None,
      None,
    )
    .unwrap();
    let output = convert_bytes_with_template(
      &steam,
      Some(&switch),
      ConversionRequest {
        target: TargetPlatform::NintendoSwitch,
        source_steamid64: Some(TEST_STEAM_ID),
        target_steamid64: None,
        source_curve_index: Some(0),
        target_curve_index: None,
        target_reference: None,
        force: false,
      },
    )
    .unwrap();
    let converted = SavePayload::parse_at_offset(&unpack_deflate(&output).unwrap(), 12).unwrap();
    assert_eq!(converted.entries[0], template.entries[0]);
    assert_eq!(converted.entries[1], source.entries[1]);
    assert_eq!(parse_header(&output).unwrap().platform(), Platform::NintendoSwitch);
    assert_eq!(checksum_status(&output).unwrap(), ChecksumStatus::Valid);
  }

  #[test]
  fn cross_platform_conversion_uses_target_template_schema() {
    let source_payload = SavePayload {
      entries: vec![NativeClass {
        native_hash: 1,
        class: Class {
          hash: 10,
          fields: vec![Field {
            hash: 100,
            field_type: 8,
            value: FieldValue::Scalar { size: 4, bytes: 7u32.to_le_bytes().to_vec() },
          }],
        },
      }],
    }
    .encode_at_offset(DSSS_HEADER_LEN)
    .expect("source payload should encode");
    let target_payload = SavePayload {
      entries: vec![NativeClass {
        native_hash: 1,
        class: Class {
          hash: 10,
          fields: vec![
            Field {
              hash: 100,
              field_type: 8,
              value: FieldValue::Scalar { size: 4, bytes: 9u32.to_le_bytes().to_vec() },
            },
            Field {
              hash: 101,
              field_type: 8,
              value: FieldValue::Scalar { size: 4, bytes: 11u32.to_le_bytes().to_vec() },
            },
          ],
        },
      }],
    }
    .encode_at_offset(align_up(DSSS_HEADER_LEN, 16))
    .expect("target payload should encode");
    let source = pack_payload(&source_payload, TargetPlatform::NintendoSwitch, None, None)
      .expect("Switch source should pack");
    let target = pack_payload(&target_payload, TargetPlatform::Steam, Some(TEST_STEAM_ID), Some(0))
      .expect("Steam template should pack");

    let converted = convert_bytes_with_template(
      &source,
      Some(&target),
      ConversionOptions {
        target: TargetPlatform::Steam,
        source_steamid64: None,
        target_steamid64: Some(TEST_STEAM_ID),
        source_curve_index: None,
        target_curve_index: Some(0),
        target_reference: None,
        force: false,
      },
    )
    .expect("template conversion should succeed");
    let converted_header = parse_header(&converted).expect("converted header should parse");
    let converted_payload =
      unpack_payload(&converted, converted_header, Some(TEST_STEAM_ID), Some(0))
        .expect("converted payload should decrypt");
    let converted_payload =
      SavePayload::parse(&converted_payload).expect("converted payload should parse");

    assert_eq!(converted_payload.entries[0].class.fields.len(), 2);
    assert_eq!(
      converted_payload.entries[0].class.fields[0].value,
      FieldValue::Scalar { size: 4, bytes: 7u32.to_le_bytes().to_vec() }
    );
    assert_eq!(
      converted_payload.entries[0].class.fields[1].value,
      FieldValue::Scalar { size: 4, bytes: 11u32.to_le_bytes().to_vec() }
    );
  }
}
