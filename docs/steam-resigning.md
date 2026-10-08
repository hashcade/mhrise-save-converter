# Steam resigning findings

## Fixed conversion defects

- **Invalid Citrus points:** the previous encoder set `x = value * 100` and used a square-root formula without checking that a root existed. This could produce off-curve ElGamal points even when the project's own encrypt/decrypt round trip passed. Encoding now searches `x = value * 100 + remainder` for `remainder` in `0..100`, preserving the decoded value while selecting an actual curve point. Tonelli-Shanks handles primes congruent to 1 modulo 4 as well as the fast path for primes congruent to 3. Decryption rejects off-curve, out-of-range, and oversized key segments.
- **Heuristic curve detection:** the previous detector assumed the decrypted first block contained mostly zero bytes. Detection now requires successful point validation and Citrus block integrity checks, without depending on the gameplay payload's zero count.
- **Stale owner identities:** same-platform conversion previously changed the encryption account while leaving the hunter's own network-record and guild-card identities bound to the source account. Resigning now updates only those two known fields, including the unfinalized IEEE CRC32 stored in big-endian order. Parsed field paths locate the byte ranges; no global SteamID search-and-replace or full payload reserialization is used. Missing, empty, and zero-initialized identities remain unchanged. Unexpected identity formats or account mismatches are rejected.
- **Missing albums:** scanning only `SS1_*`, `SS4_*`, and `SS7_*` omitted 85 `SS2_*` files in the sample. Discovery now recognizes positive numeric `SS<group>_` prefixes followed by a supported core filename. Same-platform auxiliary conversion preserves the payload and its alignment padding byte for byte while updating the account wrapper and outer checksum.

These changes apply to the shared conversion library used by both the GUI and CLI. Normal conversion does not automatically reorder character slots. Manual reordering and confirmed deletion are separate operations in the GUI's **Manage slots** tab; they are not an automatic fix for loading errors.

## Verification

Regression tests cover valid encrypted points and recovered key values on all 128 built-in curves, curve detection on a full nonzero block, invalid point rejection, owner-only identity changes, idempotence, exact reverse restoration, mismatched/duplicate identity rejection, and preservation of nonzero auxiliary padding. Synthetic fixtures contain unrelated guild-card fields with identical identity bytes to check that they are not changed.

On 2026-10-04, the updated CLI converted the previously examined fixed-account sample's complete 91-file bundle: three core saves and 88 album files. Destination-account decryption validated every Citrus point and block checksum; every outer file checksum passed. Core plaintexts matched the previously verified owner-identity-patched package exactly, before any slot swap. All album payloads matched the source byte for byte and used the destination AccountID wrapper. Private saves and destination account identifiers are not included in the repository.

## What the loading result does not prove

In this case, the short-playtime character loaded after resigning, while the longer-playtime character continued to fail after valid-point encryption and owner-identity updates. A subsequent one-off package exchanged the first two character slots, coordinating the core filenames, internal file-slot number/label, system hunter summaries, and album filename prefixes. The user then reported that the recipient could load the game.

That report is evidence for the recovery package, not proof that stale owner identity, invalid ECC, DLC, statistics, or the second slot alone caused the original `[MHRS-0012-0000]` error. The original role/summary GUID associations and slot numbers had matched before the swap. The updated converter without the extra swap has not yet been confirmed in-game for that character.

Normal conversion therefore preserves slot order, GUIDs, statistics, equipment, and progression. It does not transplant a fresh character's telemetry or clear gameplay data. If a slot-specific failure needs investigation, simply renaming a role file is insufficient: all associated metadata must remain consistent. Keep both original and destination saves backed up, and treat an integrity-checked output as unconfirmed until the intended character can actually load and save in the purchased game.
