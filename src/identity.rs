//! Account-bound fields owned by this hunter, not other hunters' guild cards.

use anyhow::{Result, bail};

use crate::payload::byte_array_ranges;

const OWNER_PATHS: &[&[u32]] = &[
  // HunterRecordSaveData.HunterRecordNetworkUniqueId.Data
  &[0x355c_8c4f, 0x0737_8f29, 0xef50_95c4],
  // GuildCardSaveData.MyData.UniqueIDByteArray
  &[0x8c6f_b4c6, 0xd51b_2c9f, 0x9bbc_a62b],
];

pub(crate) fn resign_owner_identities(
  payload: &mut [u8],
  source_steamid64: u64,
  target_steamid64: u64,
) -> Result<()> {
  if source_steamid64 == target_steamid64 {
    return Ok(());
  }
  let ranges = byte_array_ranges(payload, 16, OWNER_PATHS)?;
  let source = network_identity(source_steamid64);
  let target = network_identity(target_steamid64);
  let mut seen = [false; 2];
  // Validate everything before changing any bytes. Empty identities are normal on new saves.
  for (index, range) in &ranges {
    if seen[*index] {
      bail!("duplicate owner identity field; refusing to resign an ambiguous payload");
    }
    seen[*index] = true;
    let bytes = &payload[range.clone()];
    if !bytes.is_empty() && bytes != source && bytes != target && bytes != [0u8; 16] {
      bail!("owner identity does not match the source account or the supported BinaryInfo format");
    }
  }
  for (_, range) in ranges {
    if payload[range.clone()] == source {
      payload[range].copy_from_slice(&target);
    }
  }
  Ok(())
}

fn network_identity(steamid64: u64) -> [u8; 16] {
  // BinaryInfo uses the unfinalized IEEE CRC32 in big-endian order.
  let mut crc = u32::MAX;
  for byte in steamid64.to_le_bytes() {
    crc ^= u32::from(byte);
    for _ in 0..8 {
      crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
    }
  }
  let mut identity = [0u8; 16];
  identity[..4].copy_from_slice(&[2, 8, 0, 0]);
  identity[4..8].copy_from_slice(&crc.to_be_bytes());
  identity[8..].copy_from_slice(&steamid64.to_le_bytes());
  identity
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::payload::{Array, ArrayValue, Class, Field, FieldValue, NativeClass, SavePayload};

  const SOURCE: u64 = 76_561_197_960_270_388;
  const TARGET: u64 = 76_561_198_382_766_028;

  fn identity_field(hash: u32, bytes: &[u8]) -> Field {
    Field {
      hash,
      field_type: -1,
      value: FieldValue::Array(Array {
        member_type: 4,
        member_size: 1,
        array_type: 0,
        class_hashes: None,
        values: bytes.iter().map(|byte| ArrayValue::Scalar(vec![*byte])).collect(),
      }),
    }
  }

  fn fixture(identity: &[u8]) -> Vec<u8> {
    let mut entries = Vec::new();
    for path in OWNER_PATHS {
      let child = Class { hash: 2, fields: vec![identity_field(path[2], identity)] };
      entries.push(NativeClass {
        native_hash: path[0],
        class: Class {
          hash: 1,
          fields: vec![
            Field {
              hash: path[1],
              field_type: 17,
              value: FieldValue::Class(Box::new(child.clone())),
            },
            // An unrelated guild card contains identical bytes. It must not be patched.
            Field { hash: 99, field_type: 17, value: FieldValue::Class(Box::new(child)) },
            Field { hash: 100, field_type: 15, value: FieldValue::String(vec![42]) },
          ],
        },
      });
    }
    let mut bytes = SavePayload { entries }.encode_at_offset(16).unwrap();
    let len = bytes.len();
    bytes[len - 2..].copy_from_slice(&[0xa5, 0x5a]);
    bytes
  }

  #[test]
  fn matches_verified_binary_info_encoding() {
    assert_eq!(
      network_identity(SOURCE),
      [2, 8, 0, 0, 0xee, 0x62, 0xb5, 0x54, 0x34, 0x12, 0, 0, 1, 0, 0x10, 1,]
    );
  }

  #[test]
  fn updates_only_owned_identity_bytes_and_restores_exactly() {
    let original = fixture(&network_identity(SOURCE));
    let mut expected = original.clone();
    for (_, range) in byte_array_ranges(&original, 16, OWNER_PATHS).unwrap() {
      expected[range].copy_from_slice(&network_identity(TARGET));
    }
    let mut patched = original.clone();
    resign_owner_identities(&mut patched, SOURCE, TARGET).unwrap();
    assert_eq!(patched, expected);
    let parsed = SavePayload::parse_at_offset(&patched, 16).unwrap();
    for (entry, path) in parsed.entries.iter().zip(OWNER_PATHS) {
      for field in entry.class.fields.iter().filter(|field| field.field_type == 17) {
        let FieldValue::Class(child) = &field.value else { unreachable!() };
        let FieldValue::Array(array) = &child.fields[0].value else { unreachable!() };
        let bytes: Vec<_> = array
          .values
          .iter()
          .map(|value| match value {
            ArrayValue::Scalar(bytes) => bytes[0],
            _ => unreachable!(),
          })
          .collect();
        let account = if field.hash == path[1] { TARGET } else { SOURCE };
        assert_eq!(bytes, network_identity(account));
      }
    }
    resign_owner_identities(&mut patched, SOURCE, TARGET).unwrap();
    assert_eq!(patched, expected, "resigning twice must be idempotent");
    resign_owner_identities(&mut patched, TARGET, SOURCE).unwrap();
    assert_eq!(patched, original, "all non-owner bytes and padding must remain exact");
  }

  #[test]
  fn leaves_empty_and_uninitialized_identities_unchanged() {
    for identity in [vec![], vec![0; 16]] {
      let mut bytes = fixture(&identity);
      let original = bytes.clone();
      resign_owner_identities(&mut bytes, SOURCE, TARGET).unwrap();
      assert_eq!(bytes, original);
    }
  }

  #[test]
  fn rejects_unexpected_identities_without_mutating_payload() {
    for identity in [vec![1; 16], network_identity(123).to_vec()] {
      let mut bytes = fixture(&identity);
      let original = bytes.clone();
      assert!(resign_owner_identities(&mut bytes, SOURCE, TARGET).is_err());
      assert_eq!(bytes, original);
    }
  }

  #[test]
  fn rejects_duplicate_owner_fields_without_mutating_payload() {
    let original = fixture(&network_identity(SOURCE));
    let mut parsed = SavePayload::parse_at_offset(&original, 16).unwrap();
    parsed.entries.push(parsed.entries[0].clone());
    let mut bytes = parsed.encode_at_offset(16).unwrap();
    let original = bytes.clone();
    assert!(resign_owner_identities(&mut bytes, SOURCE, TARGET).is_err());
    assert_eq!(bytes, original);
  }

  #[test]
  fn steam_conversion_updates_owner_ids_without_reserializing_payload() {
    use crate::{
      conversion::{ConversionRequest, TargetPlatform, convert_bytes},
      crypto::Citrus,
      format::{ChecksumStatus, checksum_status},
    };
    let plain = fixture(&network_identity(SOURCE));
    let mut source = b"DSSS".to_vec();
    source.extend_from_slice(&2u32.to_le_bytes());
    source.extend_from_slice(&4u32.to_le_bytes());
    source.resize(16, 0);
    source.extend_from_slice(&Citrus::new(SOURCE, Some(93)).encrypt(&plain).unwrap());
    source.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    let checksum = murmur3::murmur3_32(&mut std::io::Cursor::new(&source), u32::MAX).unwrap();
    source.extend_from_slice(&checksum.to_le_bytes());
    let output = convert_bytes(
      &source,
      ConversionRequest {
        target: TargetPlatform::Steam,
        source_steamid64: Some(SOURCE),
        target_steamid64: Some(TARGET),
        source_curve_index: Some(93),
        target_curve_index: Some(67),
        target_reference: None,
        force: false,
      },
    )
    .unwrap();
    assert_eq!(checksum_status(&output).unwrap(), ChecksumStatus::Valid);
    let mut converted =
      Citrus::new(TARGET, Some(67)).decrypt(&output[16..output.len() - 12], plain.len()).unwrap();
    let mut expected = plain.clone();
    resign_owner_identities(&mut expected, SOURCE, TARGET).unwrap();
    assert_eq!(converted, expected);
    resign_owner_identities(&mut converted, TARGET, SOURCE).unwrap();
    assert_eq!(converted, plain);
  }
}
