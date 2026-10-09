use anyhow::{Result, ensure};
use hex_literal::hex;

use crate::{
  format::Platform,
  payload::{ArrayValue, Class, FieldValue, SavePayload},
};

// Canonically encoded, owner-free empty summary captured from MHRise 16.0.2.0.
// Used only when the source has no empty slot and its schema matches this snapshot.
pub(super) fn builtin(platform: Platform) -> Result<Class> {
  let bytes = hex!(
    "
    0000000021000000dfe80d52f63a9ff70f000000060000002800ee4e29000d54
    2171573091c053560700000004000000ffffffff8586424e0700000004000000
    ffffffff90ffb8340700000004000000000000004a5fd0260700000004000000
    ffffff7ff9ca62db0b0000000400000000000000bf61c5740c00000008000000
    00000000000000009460b59e0100000004000000010000000c4359ed01000000
    04000000000000008318478a01000000040000000000000065afdef401000000
    04000000020000000f812ad5ffffffff07000000040000001200000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    00000000000000002f0e48d70200000001000000000000004999653710000000
    1000000000000000000000000000000000000000000000000000000000000000
    85dd95f31100000000000000000000005199d468070000000400000000000000
    ba926169ffffffff110000000800000002000000010000000000000000000000
    00000000000000009fd854a8ffffffff11000000080000000200000001000000
    00000000000000000000000000000000c33b3f96070000000400000000000000
    40301da80700000004000000000000004d660a4a070000000400000000000000
    4685363c070000000400000000000000730e2cc0070000000400000000000000
    b9a99fb307000000040000000000000068bc2756070000000400000000000000
    f7e7001707000000040000000000000048664c26020000000100000000000000
    cdc00fe410000000100000000000000000000000000000000000000000000000
    c22589c9ffffffff020000000100000005000000000000000000000000000000
    e22b444cffffffff020000000100000005000000000000000000000000000000
    a9e35604ffffffff100000001000000005000000000000000000000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    000000000000000000000000000000003a23f25dffffffff1000000010000000
    0500000000000000000000000000000000000000000000000000000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    0000000000000000000000000000000000000000000000000000000000000000
    f6797902ffffffff07000000040000000300000000000000ffffffffffffffff
    ffffffff
  "
  );
  let mut payload = SavePayload::parse(&bytes)?;
  ensure!(payload.entries.len() == 1, "invalid built-in empty-slot snapshot");
  let mut summary = payload.entries.remove(0).class;
  if platform == Platform::NintendoSwitch {
    // The matching Switch schema has no Steam-only consistency field.
    summary.fields.retain(|field| field.hash != super::CONSISTENCY);
  }
  Ok(summary)
}

pub(super) fn compatible(source: &Class, empty: &Class) -> bool {
  source.hash == empty.hash
    && source.fields.len() == empty.fields.len()
    && source.fields.iter().zip(&empty.fields).all(|(source, empty)| {
      source.hash == empty.hash
        && source.field_type == empty.field_type
        && same_shape(
          &source.value,
          &empty.value,
          match source.hash {
            // These three appearance fields use null classes in an empty slot.
            0xf395_dd85 => Some(0xeec7_904b),
            0x6961_92ba => Some(0x1f54_d43a),
            0xa854_d89f => Some(0x0b27_4b03),
            _ => None,
          },
        )
    })
}

fn same_shape(source: &FieldValue, empty: &FieldValue, nullable_class: Option<u32>) -> bool {
  match (source, empty) {
    (FieldValue::String(_), FieldValue::String(_)) => true,
    (FieldValue::Scalar { size: a, .. }, FieldValue::Scalar { size: b, .. }) => a == b,
    (FieldValue::Class(a), FieldValue::Class(b)) => same_class(a, b, nullable_class),
    (FieldValue::Array(a), FieldValue::Array(b)) => {
      a.member_type == b.member_type
        && a.member_size == b.member_size
        && a.array_type == b.array_type
        && a.class_hashes == b.class_hashes
        && a.values.len() == b.values.len()
        && a.values.iter().zip(&b.values).all(|(a, b)| match (a, b) {
          (ArrayValue::Class(a), ArrayValue::Class(b)) => same_class(a, b, nullable_class),
          (ArrayValue::Scalar(a), ArrayValue::Scalar(b)) => a.len() == b.len(),
          (ArrayValue::String(_), ArrayValue::String(_)) => true,
          _ => false,
        })
    }
    _ => false,
  }
}

fn same_class(source: &Class, empty: &Class, nullable: Option<u32>) -> bool {
  if empty.hash == 0 && empty.fields.is_empty() && nullable == Some(source.hash) {
    true
  } else {
    compatible(source, empty)
  }
}

#[cfg(test)]
mod tests {
  use super::super::CONSISTENCY;
  use super::*;
  use crate::payload::Field;

  #[test]
  fn switch_snapshot_omits_only_the_steam_consistency_field() {
    let steam = builtin(Platform::Steam).unwrap();
    let switch = builtin(Platform::NintendoSwitch).unwrap();
    assert_eq!(steam.fields.len(), 33);
    assert_eq!(switch.fields.len(), 32);
    assert!(!switch.fields.iter().any(|field| field.hash == CONSISTENCY));
    let mut expected = steam.clone();
    expected.fields.retain(|field| field.hash != CONSISTENCY);
    assert_eq!(switch, expected);
    assert!(!compatible(&switch, &steam));
    assert!(!compatible(&steam, &switch));
  }

  #[test]
  fn snapshot_supports_populated_previews_but_rejects_unknown_layouts() {
    let empty = builtin(Platform::Steam).unwrap();
    assert_eq!(empty.hash, 0x520d_e8df);
    assert_eq!(empty.fields.len(), 33);
    let mut occupied = empty.clone();
    for (hash, class_hash) in
      [(0xf395_dd85, 0xeec7_904b), (0x6961_92ba, 0x1f54_d43a), (0xa854_d89f, 0x0b27_4b03)]
    {
      let field = occupied.fields.iter_mut().find(|field| field.hash == hash).unwrap();
      let populated = Box::new(Class {
        hash: class_hash,
        fields: vec![Field {
          hash: 99,
          field_type: 7,
          value: FieldValue::Scalar { size: 4, bytes: vec![0; 4] },
        }],
      });
      match &mut field.value {
        FieldValue::Class(class) => *class = populated,
        FieldValue::Array(array) => array.values[0] = ArrayValue::Class(populated),
        _ => panic!(),
      }
    }
    assert!(compatible(&occupied, &empty));
    let mut unknown = occupied.clone();
    let FieldValue::Class(preview) =
      &mut unknown.fields.iter_mut().find(|field| field.hash == 0xf395_dd85).unwrap().value
    else {
      panic!()
    };
    preview.hash = 0xdead_beef;
    assert!(!compatible(&unknown, &empty));
    unknown = occupied.clone();
    unknown.fields[0].field_type = 7;
    assert!(!compatible(&unknown, &empty));
    unknown = occupied.clone();
    unknown.fields.pop();
    assert!(!compatible(&unknown, &empty));
    unknown = occupied.clone();
    let FieldValue::Array(array) =
      &mut unknown.fields.iter_mut().find(|field| field.hash == 0x6961_92ba).unwrap().value
    else {
      panic!()
    };
    array.values.push(array.values[0].clone());
    assert!(!compatible(&unknown, &empty));
  }
}
