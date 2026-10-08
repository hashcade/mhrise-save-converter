# Development

## Running from source

```bash
cargo run --release --bin mhrise-save-converter-gui
```

## Platform and build requirements

The GPUI frontend in v0.4.0 and later targets macOS 15+ (Apple Silicon and Intel), Windows 10+, and Linux with a Vulkan-capable graphics driver. Packages through v0.3.0 use egui and have different GUI requirements. The build and release workflows retain all four native targets.

Source builds use the Rust toolchain pinned in `rust-toolchain.toml`. macOS needs Xcode Command Line Tools; Windows needs the MSVC C++ toolchain and CMake. On Ubuntu, install the native GUI dependencies:

```bash
sudo apt-get install build-essential clang cmake pkg-config libfontconfig-dev \
  libwayland-dev libwebkit2gtk-4.1-dev libxkbcommon-x11-dev libx11-xcb-dev \
  libssl-dev libzstd-dev libvulkan1
```

To build only the CLI without GUI dependencies:

```bash
cargo build --locked --release --no-default-features --bin mhrise-save-converter
```

Run the full suite with `cargo test --locked --all-targets --all-features`. The `gui-render` integration test renders the production frontend through GPUI's Metal renderer and exercises input, tabs, advanced options, slot moves, and deletion confirmation/cancellation on macOS. It explicitly skips GPU rendering on other platforms; backend/controller tests still run there. Set `MHR_GUI_SCREENSHOTS` to an existing directory to save those real rendered frames. Optional `MHR_GUI_TEST_SAVE` and `MHR_GUI_TEST_STEAMID64` load a local read-only save fixture for captures; no paths or account IDs are built into the application.

## Publishing

Pushes to `main` and pull requests run CI and four-platform packaging; documentation-only changes skip both. New runs cancel outdated CI/Build runs for the same ref. Version tags run only Release, which shares each platform's Rust dependency cache with Build. The release script's `chore: release v...` version-only commit still runs CI but skips Build, leaving packaging to Release. Releases are not cancelled by ordinary commits.

Publish from a clean, up-to-date `main` checkout with Python 3.9+, Git, and Rust:

```bash
python3 scripts/release.py --bump patch --yes
python3 scripts/release.py --bump minor --yes
```

Run only one command for the intended version increment. With no arguments, the script proposes a patch bump and asks for confirmation. `--current` publishes the existing package version without bumping it; existing tags are rejected. The script checks the CLI, updates `Cargo.toml`/`Cargo.lock`, commits the version bump, pushes `main`, and pushes an annotated tag. GitHub Actions builds and verifies the four release packages. The publishing script uses only Python's standard library; CI packaging also uses `scripts/package.py`.
