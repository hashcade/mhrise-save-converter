# CLI usage

The CLI is maintained for advanced users and development. Most users should use the native GUI or download a packaged release from the [GitHub Releases](https://github.com/jinghaihan/mhrise-save-converter/releases) page.

## Inspect and verify

Inspect save containers without modifying them:

```bash
mhrise-save inspect /path/to/save
```

Verify outer checksums for all supported files:

```bash
mhrise-save verify /path/to/save
```

During development, run these commands from the repository with `cargo run --` instead of `mhrise-save`.

## Switch → Steam

Use an existing Steam save as the target template when possible. It provides the destination schema, platform settings, metadata, and Curve Index:

```bash
mhrise-save convert /path/to/monster-hunter-rise-ns /path/to/new-steam-save \
  --to steam \
  --target-steamid64 <TARGET_STEAMID64> \
  --target-reference /path/to/existing/win64_save
```

Without a Steam template, provide the target Curve Index explicitly. Built-in defaults are used for known Steam-only fields:

```bash
mhrise-save convert /path/to/monster-hunter-rise-ns /path/to/new-steam-save \
  --to steam \
  --target-steamid64 <TARGET_STEAMID64> \
  --target-curve-index <TARGET_CURVE_INDEX>
```

## Steam → Switch

This path requires a Switch target template because some Switch-only DLC fields cannot be inferred reliably from Steam:

```bash
mhrise-save convert /path/to/win64_save /path/to/new-switch-save \
  --to switch \
  --source-steamid64 <SOURCE_STEAMID64> \
  --target-reference /path/to/existing/switch-save
```

Cross-platform conversion keeps the destination template's confirmation-button setting (`OptionSystemSave.SystemSaveData.DecideData`) instead of copying the source platform's setting. Use a template whose confirm/cancel controls already work as intended. Other matching user settings and gameplay progress continue to come from the source; this does not reset all controller settings or hardcode an A/B mapping.

## Steam → Steam

Provide the source account ID and destination account ID. The source Curve Index is detected automatically when omitted:

```bash
mhrise-save convert /path/to/source/win64_save /path/to/new-steam-save \
  --to steam \
  --source-steamid64 <SOURCE_STEAMID64> \
  --target-steamid64 <TARGET_STEAMID64> \
  --target-reference /path/to/target/win64_save
```

Add `--source-curve-index` or `--target-curve-index` only when automatic detection is unavailable. Add `--force` only when intentionally reusing a non-empty output path.

## Steam IDs and Curve Index

`SteamID64` is the 17-digit numeric Steam account identifier. It is not a display name, email address, or custom `/id/...` profile name. Get it from the numeric `steamcommunity.com/profiles/<STEAMID64>` URL of the account; if the profile uses a custom URL, use a SteamID lookup tool or copy the canonical numeric profile URL.

- Switch → Steam: `target-steamid64` is the account that will own the converted save.
- Steam → Switch: `source-steamid64` is the account that owns the source save.
- Steam → Steam: provide both source and target IDs.
- `Curve Index` is a Citrus encryption parameter, not an account identifier. The tool detects it from a Steam source save, and detects the destination value from a Steam `--target-reference` template.

On Windows, Steam saves are commonly under `Steam/userdata/<STEAMID64>/1446780/remote/win64_save`. The `1446780` directory is Monster Hunter Rise's Steam app ID.

## Safety

The input and template directories are read-only. Always write to a new output directory, keep backups, and disable Steam Cloud while testing so it cannot overwrite converted files.
