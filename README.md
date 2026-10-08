# MHRise Save Converter

Native GUI and library tooling for converting *Monster Hunter Rise* saves between Nintendo Switch and Steam formats.

> [!WARNING]
> **Test status:** Nintendo Switch → Steam conversion has been tested and confirmed working. The updated Steam → Steam converter has passed real-file decryption, curve-point, integrity, and payload-preservation checks. In one migration, the user reported successful loading after an additional coordinated character-slot swap; that workaround is not part of normal conversion, and the updated converter's output without it has not been confirmed in-game. Steam → Nintendo Switch has not been tested yet. Always back up your saves first.

## GUI

Download the latest package from [Releases](https://github.com/jinghaihan/mhrise-save-converter/releases), or run it from source:

```bash
cargo run --release --bin mhrise-save-converter-gui
```

Choose a source save, an output directory, and the target platform. For Steam conversion, enter the relevant SteamID64 and preferably select an existing target save as the template; the app can read the destination Curve Index automatically. The source and template are never modified. The GUI performs a preflight check, converts in the background, shows per-file progress, and can open the output folder.

Required SteamID64 fields are visible in the main form; Switch inputs do not need an ID. Advanced options contain only Curve Index overrides. Conversion can write into an existing non-empty directory without changing unrelated files. If generated filenames already exist, the GUI lists the conflicts and asks for confirmation before writing. Cancelling leaves the existing files unchanged.

## Character-slot swap

Open the **Swap slots** tab to manage slots independently of conversion. Choose the complete source save folder and a new, nonexistent output folder. For Steam, enter the current account's SteamID64; the Curve Index is detected automatically. Choose **Read save** to display all three slots. In the table, select the character in **Move character** and its target position in **Destination**. An empty destination produces a **Move to slot N** action and leaves the original position empty; an occupied destination exchanges the two characters instead.

The operation coordinates system summaries, character filenames and internal slot metadata, and album prefixes. It keeps the same account and platform, copies other regular files unchanged, and does not edit the original save. Existing output directories, incomplete or inconsistent bundles, and subdirectories/symlinks are rejected. Clicking the output Browse button selects a parent directory and proposes a new `swapped-save` subfolder.

Steam and Switch container tests cover exchanges, empty destinations, round trips, and integrity checks. The generalized implementation has also been checked with a real Steam bundle. This is not a guarantee that swapping fixes every loading error; new outputs still need an in-game test. Slot deletion is not implemented.

## Steam inputs

SteamID64 is the account's 17-digit numeric Steam identifier, not a display name or custom profile name. A numeric Steam profile URL has the form `steamcommunity.com/profiles/<STEAMID64>`. For details about SteamID64, Curve Index, save locations, and the advanced CLI, see [docs/cli.md](docs/cli.md).

The supported file layout and container details are documented in [docs/save-structure.md](docs/save-structure.md).

### Fixed-account PC save → purchased Steam account

For pirated PC copies using TENOKE, account configuration is in `<game directory>/tenoke.ini`; saves may be under `<game directory>/SteamData/win64_save`. Check the actual configuration rather than assuming a fixed SteamID64.

| Source setting | Verified value |
| --- | --- |
| SteamID64 | `76561197960270388` |
| AccountID (low 32 bits) | `4660` / `0x1234` |
| Citrus Curve Index | `93` |

These values were confirmed by successfully decrypting `data00-1.bin`, `data001Slot.bin`, and `data002Slot.bin`, checking every Citrus block, and parsing their class streams. They apply to this verified sample; other packages or modified account configurations may use different values. Album headers can provide candidate account IDs, but this sample also contained album files with a different ID, so successful core-file decryption is the deciding check.

In the GUI, select the source `SteamData/win64_save` folder, choose a new output directory, set **Target** to **Steam**, and enter:

- **Source SteamID64:** `76561197960270388`
- **Source Curve Index:** `93` (optional; detected automatically if blank)
- **Target SteamID64:** the SteamID64 of the purchased-game account
- **Template:** that account's own `win64_save` folder, created by launching and saving in the purchased game
- **Target Curve Index:** leave blank to detect it from the template; do not reuse the source value `93`

Equivalent CLI command:

```bash
mhrise-save convert /path/to/SteamData/win64_save /path/to/new-steam-save \
  --to steam \
  --source-steamid64 76561197960270388 \
  --source-curve-index 93 \
  --target-steamid64 <TARGET_STEAMID64> \
  --target-reference /path/to/purchased-account/win64_save
```

Exit the game, back up the destination save, and disable Steam Cloud while installing and testing the output. The destination path normally uses the account's **32-bit AccountID**, not its full SteamID64: `<Steam directory>/userdata/<AccountID>/1446780/remote/win64_save`.

**Albums:** the GUI/CLI includes numeric `SS<group>_` auxiliary files, including this sample's 85 `SS2_*` files and three `SS1_*` files. Steam → Steam album conversion updates the AccountID wrapper and outer checksum without reserializing the payload or its padding. Cross-platform album conversion still realigns the class stream for the destination wrapper. Copying Steam album files unchanged does not update their account ID.

**Owner identity:** Steam → Steam conversion also updates the hunter's own network-record and guild-card Steam identity blobs, including their BinaryInfo CRCs. Empty identities stay empty; other hunters' guild cards are untouched. All other core payload bytes, including slot metadata, GUIDs, statistics, progress, and padding, remain unchanged. Unsupported or mismatched owner identities are rejected rather than guessed. See [Steam resigning findings](docs/steam-resigning.md) for the fixes and remaining limitations.

## Credits

Format research and implementation references:

- [kvasszn/ree-save-editor](https://github.com/kvasszn/ree-save-editor), especially its MH Rise DSSS/Citrus research and account-transfer notes.

This project is an independent implementation and does not include the referenced project's GUI or source tree.

## License

[MIT](./LICENSE) License © [jinghaihan](https://github.com/jinghaihan)
