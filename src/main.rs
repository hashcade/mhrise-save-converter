use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use mhrise_save_converter::{
  conversion::{ConversionRequest, TargetPlatform, convert_path, find_curve_index, verify_file},
  discover::{discover_core_files, discover_save_files},
  slots::{SlotOptions, inspect_slots, swap_slots},
};

#[derive(Debug, Parser)]
#[command(name = "mhrise-save", version, about = "Inspect and convert Monster Hunter Rise saves")]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
  /// List the three character slots without changing files.
  Slots {
    input: PathBuf,
    #[arg(long)]
    steamid64: Option<u64>,
    #[arg(long)]
    curve_index: Option<usize>,
  },
  /// Exchange two character slots and their albums into a new save directory.
  SwapSlots {
    input: PathBuf,
    output: PathBuf,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
    first: u8,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=3))]
    second: u8,
    #[arg(long)]
    steamid64: Option<u64>,
    #[arg(long)]
    curve_index: Option<usize>,
  },
  /// Inspect DSSS headers and integrity checks without changing files.
  Inspect {
    /// A save file or a win64_save directory.
    path: PathBuf,
  },
  /// Convert the complete supported save bundle into another platform format.
  Convert {
    /// A save file or a win64_save directory.
    input: PathBuf,
    /// A new output file or directory.
    output: PathBuf,
    /// Target platform format.
    #[arg(long, value_enum)]
    to: Target,
    /// SteamID64 used to decrypt Steam source files.
    #[arg(long)]
    source_steamid64: Option<u64>,
    /// SteamID64 used to encrypt Steam target files.
    #[arg(long)]
    target_steamid64: Option<u64>,
    /// Source Citrus curve index. If omitted, it is detected from the source save.
    #[arg(long)]
    source_curve_index: Option<usize>,
    /// Target Citrus curve index.
    #[arg(long)]
    target_curve_index: Option<usize>,
    /// Existing target save used as a schema/default template and, for Steam, to detect its curve.
    #[arg(long)]
    target_reference: Option<PathBuf>,
    /// Overwrite existing output files with the same names; unrelated files are kept.
    #[arg(long)]
    force: bool,
  },
  /// Verify outer file checksums for all supported save files.
  Verify {
    /// A save file or a win64_save directory.
    path: PathBuf,
  },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Target {
  Switch,
  Steam,
}

#[derive(Debug)]
struct ConvertRequest {
  input: PathBuf,
  output: PathBuf,
  target: Target,
  source_steamid64: Option<u64>,
  target_steamid64: Option<u64>,
  source_curve_index: Option<usize>,
  target_curve_index: Option<usize>,
  target_reference: Option<PathBuf>,
  force: bool,
}

fn main() -> Result<()> {
  let cli = Cli::parse();

  match cli.command {
    Command::Slots { input, steamid64, curve_index } => {
      let report = inspect_slots(&input, SlotOptions { steamid64, curve_index })?;
      println!("Platform: {}", report.platform);
      for slot in report.slots {
        println!(
          "Slot {}: {} | HR {} | MR {} | {}",
          slot.number,
          slot.name.as_deref().unwrap_or("Empty"),
          slot.hunter_rank,
          slot.master_rank,
          slot.playtime()
        );
      }
      Ok(())
    }
    Command::SwapSlots { input, output, first, second, steamid64, curve_index } => {
      let written =
        swap_slots(&input, &output, first, second, SlotOptions { steamid64, curve_index })?;
      println!(
        "Swapped slots {first} and {second}: {} files written to {}",
        written.len(),
        output.display()
      );
      Ok(())
    }
    Command::Inspect { path } => inspect(&path),
    Command::Convert {
      input,
      output,
      to,
      source_steamid64,
      target_steamid64,
      source_curve_index,
      target_curve_index,
      target_reference,
      force,
    } => convert(ConvertRequest {
      input,
      output,
      target: to,
      source_steamid64,
      target_steamid64,
      source_curve_index,
      target_curve_index,
      target_reference,
      force,
    }),
    Command::Verify { path } => verify(&path),
  }
}

fn inspect(path: &Path) -> Result<()> {
  let files = discover_save_files(path)?;
  println!("Input: {}", path.display());
  println!("Save files: {}", files.len());

  for file in files {
    let name = file.path.file_name().and_then(|value| value.to_str()).unwrap_or("<unknown>");
    println!(
      "- {name}: kind={}, platform={}, checksum={}",
      file.kind, file.platform, file.checksum
    );
  }

  Ok(())
}

fn convert(request: ConvertRequest) -> Result<()> {
  let ConvertRequest {
    input,
    output,
    target,
    source_steamid64,
    target_steamid64,
    source_curve_index,
    target_curve_index,
    target_reference,
    force,
  } = request;
  let target = match target {
    Target::Switch => TargetPlatform::NintendoSwitch,
    Target::Steam => TargetPlatform::Steam,
  };
  let target_curve_index =
    match (target, target_curve_index, target_reference.as_deref(), target_steamid64) {
      (TargetPlatform::Steam, Some(index), _, _) => Some(index),
      (TargetPlatform::Steam, None, Some(reference), Some(id)) => {
        let reference_file = reference_file(reference)?;
        Some(find_curve_index(&reference_file, id).with_context(|| {
          format!("could not read target Curve Index from {}", reference_file.display())
        })?)
      }
      (TargetPlatform::Steam, None, _, _) => None,
      (TargetPlatform::NintendoSwitch, index, _, _) => index,
    };

  let written = convert_path(
    &input,
    &output,
    ConversionRequest {
      target,
      source_steamid64,
      target_steamid64,
      source_curve_index,
      target_curve_index,
      target_reference: target_reference.clone(),
      force,
    },
  )?;
  println!("Converted {} file(s):", written.len());
  for path in written {
    println!("- {}", path.display());
  }
  Ok(())
}

fn reference_file(reference: &Path) -> Result<PathBuf> {
  if reference.is_file() {
    return Ok(reference.to_path_buf());
  }
  let files = discover_core_files(reference)?;
  files
    .iter()
    .find(|file| file.path.file_name().is_some_and(|name| name == "data001Slot.bin"))
    .or_else(|| files.first())
    .map(|file| file.path.clone())
    .context("target reference contains no usable core save file")
}

fn verify(path: &Path) -> Result<()> {
  let files = discover_save_files(path)?;
  let mut failed = false;
  for file in files {
    let valid = verify_file(&file.path)?;
    println!("{}: {}", file.path.display(), if valid { "valid" } else { "INVALID" });
    failed |= !valid;
  }
  if failed {
    bail!("one or more file checksums are invalid");
  }
  Ok(())
}
