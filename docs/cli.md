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

## Character slots

List the three positions (the Steam account ID is unnecessary for Switch saves):

```bash
mhrise-save slots /path/to/win64_save --steamid64 <CURRENT_STEAMID64>
```

Exchange slots 1 and 2 while preserving the platform and account:

```bash
mhrise-save swap-slots /path/to/win64_save /path/to/new-swapped-save \
  --first 1 --second 2 --steamid64 <CURRENT_STEAMID64>
```

Slot numbers are 1–3. An occupied/empty exchange moves the character; two empty slots or the same slot are rejected. `--curve-index` optionally overrides automatic Steam curve detection. A target template is not required.

The output directory must not already exist, its parent must exist, and it must be outside the source directory. There is no `--force` option for slot swaps. Each character's `SSN_` album/metadata namespace moves with its owner without changing file bytes; unrelated regular files are copied unchanged. The bundle is verified before publishing the output. Back up the original save and test the output with Steam Cloud disabled.

The CLI currently supports inspection and pairwise swaps only. For repeated reordering or confirmed character deletion, use the GUI's **Manage slots** tab. Deletion marks a position empty without automatically shifting the remaining characters and applies only when **Save slots** writes a new output bundle. See [Character-slot safety and verification](slot-management.md) for validation and safety limits.

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

Add `--source-curve-index` or `--target-curve-index` only when automatic detection is unavailable. Non-empty conversion output directories are allowed; unrelated files are kept. Add `--force` only to overwrite generated files with the same names. Source and target template files must not be used as output files, even with `--force`. Slot swaps still require a new output directory.

Resigning updates the supported identity fields owned by the hunter, not other hunters' guild cards. It does not move characters between slots, replace statistics, or copy the target account's gameplay progress. For known fixes and the limits of the reported in-game slot-swap recovery, see [Steam resigning findings](steam-resigning.md).

## Steam IDs and Curve Index

`SteamID64` is the 17-digit numeric Steam account identifier. It is not a display name, email address, or custom `/id/...` profile name. Get it from the numeric `steamcommunity.com/profiles/<STEAMID64>` URL of the account; if the profile uses a custom URL, use a SteamID lookup tool or copy the canonical numeric profile URL.

- Switch → Steam: `target-steamid64` is the account that will own the converted save.
- Steam → Switch: `source-steamid64` is the account that owns the source save.
- Steam → Steam: provide both source and target IDs.
- `Curve Index` is a Citrus encryption parameter, not an account identifier. The tool detects it from a Steam source save, and detects the destination value from a Steam `--target-reference` template.

On Windows, Steam saves are commonly under `Steam/userdata/<AccountID>/1446780/remote/win64_save`. This directory uses the account's **32-bit AccountID** (the low 32 bits of SteamID64), not the full SteamID64 required by the converter. The `1446780` directory is Monster Hunter Rise's Steam app ID.

### Fixed-account PC saves

For pirated PC copies using TENOKE, account configuration is in `<game directory>/tenoke.ini`; saves may be under `<game directory>/SteamData/win64_save`. Check the actual configuration rather than assuming a fixed SteamID64.

## Safety

The input and template directories are read-only. Always write to a new output directory, keep backups, and disable Steam Cloud while testing so it cannot overwrite converted files.
