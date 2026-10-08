# Save structure

Monster Hunter Rise saves are directories containing several DSSS container files. During platform/account conversion, the converter handles the core gameplay files and known album/photo containers:

| File or pattern | Contents | Handling |
| --- | --- | --- |
| `data00-1.bin` | System and global state | Converted with the platform schema |
| `data###Slot.bin` | Hunter progress, equipment, items, and quests | Converted with the platform schema |
| `SS<group>_data###Slot.bin` (positive numeric group), `SS<group>_data00-1.bin` | Album and screenshot data, including `SS2_*` | Rewrapped while preserving the raw payload on the same platform |
| Other files | Version- or feature-specific data | Unsupported and omitted |

The core formats share the DSSS v2 container but use different payload protection:

- Switch uses raw DEFLATE (`DEFLATE`, `0x08`).
- Steam uses Citrus encryption (`CITRUS`, `0x04`), which depends on SteamID64 and a Curve Index.
- Known album/photo files use an unencrypted wrapper on Switch and a `HAS_ID` (`0x02`) wrapper on Steam.
- Both formats contain an outer MurmurHash3 integrity value; Steam core files also contain Citrus block checks.

Steam → Steam resigning updates two initialized owner identity byte arrays and their BinaryInfo CRCs: `HunterRecordSaveData.HunterRecordNetworkUniqueId.Data` and `GuildCardSaveData.MyData.UniqueIDByteArray`. It preserves every other core payload byte and rebuilds the required container integrity values. Other hunters' guild cards and uninitialized identities are not changed. Same-platform album conversion preserves the raw payload and updates only the wrapper and checksum.

Cross-platform conversion follows the destination class schema, copies matching source fields, and preserves or constructs destination-only fields. Use a new output directory and keep the original save as a backup; files outside the supported patterns cannot currently be reconstructed.

## Character-slot editing

Slot editing is separate from conversion and preserves the existing platform and account. It reads a complete source folder and writes a verified bundle to a new, nonexistent output folder outside the source. The source is never modified; subdirectories, symlinks, and incomplete or inconsistent bundles are rejected.

System hunter summaries, character filenames, internal slot metadata, and album prefixes move together. Every file in an original character's exact `SS1_`, `SS2_`, or `SS3_` namespace follows that character, even when the filename is not a supported conversion pattern. Its contents remain unchanged. Unrelated regular files are copied unchanged; for example, `SS10_` is not treated as `SS1_`.

Deleting a character excludes its role file and slot-associated namespace from the new bundle and replaces its system summary with a complete empty summary. Other characters do not automatically shift. A source empty summary is reused when available; otherwise, only recognized layouts compatible with the built-in 16.0.2.0 empty summary are accepted. Unrelated parsed system fields are verified unchanged, although alignment padding may be regenerated.

Container tests and real Steam file checks cover slot editing, but deletion has not been tested in-game. See [Character-slot management](../README.md#character-slot-management) for the GUI workflow and [Character-slot safety and verification](slot-management.md) for safety limits.
