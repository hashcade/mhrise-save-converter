# Save structure

Monster Hunter Rise saves are directories containing several DSSS container files. The converter currently handles the core gameplay files and known album/photo containers:

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
