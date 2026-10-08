#!/usr/bin/env python3
"""Safely bump or publish the Cargo package version from the main branch."""

import argparse
import re
import subprocess
import sys
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
VERSION_PATTERN = re.compile(r'^(version\s*=\s*")([^"]+)("\s*)$', re.MULTILINE)


def run(*command: str, capture: bool = False) -> str:
  result = subprocess.run(
    command,
    cwd=PROJECT_ROOT,
    check=False,
    text=True,
    capture_output=capture,
  )
  if result.returncode != 0:
    if result.stderr:
      print(result.stderr.rstrip(), file=sys.stderr)
    raise SystemExit(f"command failed: {' '.join(command)}")
  return result.stdout.strip() if capture else ""


def cargo_version() -> str:
  cargo_toml = (PROJECT_ROOT / "Cargo.toml").read_text(encoding="utf-8")
  match = VERSION_PATTERN.search(cargo_toml)
  if match is None:
    raise SystemExit("could not find package version in Cargo.toml")
  return match.group(2)


def parse_version(version: str) -> tuple[int, int, int]:
  match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
  if match is None:
    raise SystemExit(f"unsupported release version: {version}")
  return tuple(int(part) for part in match.groups())


def next_version(version: str, component: str) -> str:
  major, minor, patch = parse_version(version)
  if component == "major":
    return f"{major + 1}.0.0"
  if component == "minor":
    return f"{major}.{minor + 1}.0"
  return f"{major}.{minor}.{patch + 1}"


def update_cargo_version(version: str) -> None:
  path = PROJECT_ROOT / "Cargo.toml"
  content = path.read_text(encoding="utf-8")
  updated, count = VERSION_PATTERN.subn(rf"\g<1>{version}\g<3>", content, count=1)
  if count != 1:
    raise SystemExit("could not update package version in Cargo.toml")
  path.write_text(updated, encoding="utf-8")


def confirm(prompt: str, assume_yes: bool) -> None:
  if assume_yes:
    return
  answer = input(f"{prompt} [y/N] ").strip().lower()
  if answer not in {"y", "yes"}:
    raise SystemExit("release cancelled")


def ensure_release_state() -> None:
  branch = run("git", "branch", "--show-current", capture=True)
  if branch != "main":
    raise SystemExit(f"release must run from main, currently on {branch or '(detached HEAD)'}")
  if run("git", "status", "--porcelain", capture=True):
    raise SystemExit("release requires a clean working tree")

  run("git", "fetch", "origin", "main", "--tags")
  local = run("git", "rev-parse", "HEAD", capture=True)
  remote = run("git", "rev-parse", "origin/main", capture=True)
  is_ancestor = subprocess.run(
    ("git", "merge-base", "--is-ancestor", remote, local), cwd=PROJECT_ROOT, check=False
  )
  if is_ancestor.returncode != 0:
    raise SystemExit("local main is behind or diverged from origin/main")


def ensure_tag_is_new(tag: str) -> None:
  local_tag = subprocess.run(
    ("git", "show-ref", "--verify", "--quiet", f"refs/tags/{tag}"),
    cwd=PROJECT_ROOT,
    check=False,
  )
  if local_tag.returncode == 0:
    raise SystemExit(f"tag already exists locally: {tag}")
  if run("git", "ls-remote", "--tags", "origin", f"refs/tags/{tag}", capture=True):
    raise SystemExit(f"tag already exists on origin: {tag}")


def parse_args() -> argparse.Namespace:
  parser = argparse.ArgumentParser(description=__doc__)
  group = parser.add_mutually_exclusive_group()
  group.add_argument("--current", action="store_true", help="publish the current Cargo.toml version")
  group.add_argument("--bump", choices=("major", "minor", "patch"), help="bump the version first")
  parser.add_argument("--yes", action="store_true", help="skip the final confirmation")
  return parser.parse_args()


def main() -> None:
  args = parse_args()
  ensure_release_state()
  current = cargo_version()
  version = current if args.current else next_version(current, args.bump or "patch")
  tag = f"v{version}"
  ensure_tag_is_new(tag)
  confirm(f"Publish {tag} from the current main commit?", args.yes)

  if version != current:
    update_cargo_version(version)
    run("cargo", "check", "--no-default-features", "--bin", "mhrise-save-converter")
    run("git", "add", "Cargo.toml", "Cargo.lock")
    run("git", "commit", "-m", f"chore: release {tag}")

  run("git", "push", "origin", "main")
  run("git", "tag", "-a", tag, "-m", f"Release {tag}")
  run("git", "push", "origin", tag)
  print(f"Pushed {tag}; GitHub Actions will build and publish the release artifacts.")


if __name__ == "__main__":
  main()
