#!/usr/bin/env python3
"""Package the already-built CLI and GUI binaries for a release artifact."""

import argparse
import hashlib
import re
import shutil
import tarfile
import tempfile
import zipfile
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]


def project_version() -> str:
  cargo_toml = (PROJECT_ROOT / "Cargo.toml").read_text(encoding="utf-8")
  match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.MULTILINE)
  if match is None:
    raise RuntimeError("could not find package version in Cargo.toml")
  return match.group(1)


def parse_args() -> argparse.Namespace:
  parser = argparse.ArgumentParser(description=__doc__)
  parser.add_argument("--target-dir", type=Path, required=True, help="Cargo release directory")
  parser.add_argument("--target", required=True, help="Rust target triple")
  parser.add_argument("--output", type=Path, default=Path("dist"), help="Artifact directory")
  parser.add_argument("--version", help="Release version; defaults to Cargo.toml")
  return parser.parse_args()


def copy_required_files(target_dir: Path, package_dir: Path, target: str) -> None:
  executable_suffix = ".exe" if "windows" in target else ""
  binaries = (
    ("mhrise-save-converter", "mhrise-save"),
    ("mhrise-save-converter-gui", "mhrise-save-converter-gui"),
  )
  for source_name, package_name in binaries:
    source = target_dir / f"{source_name}{executable_suffix}"
    if not source.is_file():
      raise RuntimeError(f"missing release binary: {source}")
    destination = package_dir / f"{package_name}{executable_suffix}"
    shutil.copy2(source, destination)

  for filename in ("README.md", "LICENSE"):
    source = PROJECT_ROOT / filename
    if source.is_file():
      shutil.copy2(source, package_dir / filename)

  shutil.copytree(PROJECT_ROOT / "docs", package_dir / "docs")


def create_archive(package_dir: Path, output_dir: Path, target: str, version: str) -> Path:
  archive_stem = f"mhrise-save-converter-v{version}-{target}"
  if "windows" in target:
    archive_path = output_dir / f"{archive_stem}.zip"
    with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
      for path in sorted(package_dir.rglob("*")):
        if path.is_file():
          archive.write(path, Path(package_dir.name) / path.relative_to(package_dir))
  else:
    archive_path = output_dir / f"{archive_stem}.tar.gz"
    with tarfile.open(archive_path, "w:gz") as archive:
      archive.add(package_dir, arcname=package_dir.name)
  return archive_path


def write_checksum(archive_path: Path) -> Path:
  digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
  checksum_path = archive_path.with_suffix(archive_path.suffix + ".sha256")
  checksum_path.write_text(f"{digest}  {archive_path.name}\n", encoding="utf-8")
  return checksum_path


def main() -> None:
  args = parse_args()
  version = args.version or project_version()
  target_dir = args.target_dir.resolve()
  output_dir = args.output.resolve()
  if not target_dir.is_dir():
    raise SystemExit(f"release directory does not exist: {target_dir}")
  output_dir.mkdir(parents=True, exist_ok=True)

  package_name = f"mhrise-save-converter-v{version}-{args.target}"
  with tempfile.TemporaryDirectory(prefix="mhrise-package-") as temporary_dir:
    package_dir = Path(temporary_dir) / package_name
    package_dir.mkdir()
    copy_required_files(target_dir, package_dir, args.target)
    archive_path = create_archive(package_dir, output_dir, args.target, version)

  checksum_path = write_checksum(archive_path)
  print(archive_path)
  print(checksum_path)


if __name__ == "__main__":
  main()
