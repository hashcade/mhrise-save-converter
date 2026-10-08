#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: ./tools/release.sh [--bump major|minor|patch | --current] [--yes]' \
    'Default: bump the patch version, confirm, push main and an annotated release tag.'
}

mode=patch
mode_selected=false
assume_yes=false
while (($#)); do
  case "$1" in
    --current|--bump)
      if $mode_selected; then
        fail 'choose either --current or one --bump option'
      fi
      mode_selected=true
      if [[ $1 == --current ]]; then
        mode=current
      else
        (($# >= 2)) || fail '--bump requires major, minor, or patch'
        shift
        case "$1" in
          major|minor|patch) mode=$1 ;;
          *) fail "unsupported bump: $1" ;;
        esac
      fi
      ;;
    --yes) assume_yes=true ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; fail "unknown option: $1" ;;
  esac
  shift
done

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$project_root"
branch=$(git branch --show-current)
[[ $branch == main ]] || fail "release must run from main, currently on ${branch:-'(detached HEAD)'}"
[[ -z $(git status --porcelain) ]] || fail 'release requires a clean working tree'
git fetch origin main --tags
git merge-base --is-ancestor origin/main HEAD || fail 'local main is behind or diverged from origin/main'

current=$(awk '
  /^\[package\]/ { package = 1; next }
  /^\[/ { package = 0 }
  package && /^version[[:space:]]*=/ {
    split($0, parts, "\"")
    print parts[2]
    exit
  }
' Cargo.toml)
[[ $current =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || fail "unsupported release version: $current"
major=$((10#${BASH_REMATCH[1]}))
minor=$((10#${BASH_REMATCH[2]}))
patch=$((10#${BASH_REMATCH[3]}))
case "$mode" in
  current) version=$current ;;
  major) version="$((major + 1)).0.0" ;;
  minor) version="$major.$((minor + 1)).0" ;;
  patch) version="$major.$minor.$((patch + 1))" ;;
esac
tag="v$version"
if git show-ref --verify --quiet "refs/tags/$tag"; then
  fail "tag already exists locally: $tag"
fi
remote_tag=$(git ls-remote --tags origin "refs/tags/$tag")
[[ -z $remote_tag ]] || fail "tag already exists on origin: $tag"

if ! $assume_yes; then
  printf 'Publish %s from the current main commit? [y/N] ' "$tag"
  IFS= read -r answer || fail 'release cancelled'
  case "$answer" in
    y|Y|yes|YES|Yes) ;;
    *) fail 'release cancelled' ;;
  esac
fi

if [[ $version != "$current" ]]; then
  version_file=$(mktemp "$project_root/.release-version.XXXXXX")
  trap 'rm -f -- "$version_file"' EXIT
  cp -p Cargo.toml "$version_file"
  awk -v version="$version" '
    /^\[package\]/ { package = 1 }
    /^\[/ && $0 != "[package]" { package = 0 }
    package && /^version[[:space:]]*=/ { $0 = "version = \"" version "\"" }
    { print }
  ' Cargo.toml > "$version_file"
  mv -- "$version_file" Cargo.toml
  trap - EXIT
  cargo check --no-default-features --bin mhrise-save-converter
  git add Cargo.toml Cargo.lock
  git commit -m "chore: release $tag"
fi

git push origin main
git tag -a "$tag" -m "Release $tag"
git push origin "$tag"
printf 'Pushed %s; GitHub Actions will build and publish the release artifacts.\n' "$tag"
