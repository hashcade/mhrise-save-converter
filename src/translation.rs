use crate::payload::{Array, ArrayValue, Class, Field, FieldValue, NativeClass, SavePayload};

const SAVE_FILE_DETAIL_NATIVE_HASH: u32 = 0x85e9_04c1;
const SNAPSHOT_SAVE_DATA_NATIVE_HASH: u32 = 0x1322_883a;
const DLC_HUNTER_SAVE_NATIVE_HASH: u32 = 0x1f61_3294;
const EXTERNAL_DL_ITEM_PACK_NATIVE_HASH: u32 = 0xa812_38c6;

const FACILITY_SAVE_DATA_CLASS_HASH: u32 = 0x0918_a30e;
const FLAG_DATA_SAVE_DATA_CLASS_HASH: u32 = 0xd41c_bf69;
const GUILD_CARD_DATA_CLASS_HASH: u32 = 0x4454_1321;
const HUNTER_RECORD_CLASS_HASH: u32 = 0xaf6e_a643;
const NETWORK_SAVE_DATA_CLASS_HASH: u32 = 0xfdb8_053d;
const OPTION_SYSTEM_SAVE_DATA_CLASS_HASH: u32 = 0x34c5_d174;
const QUEST_SAVE_DATA_CLASS_HASH: u32 = 0xdc89_2e15;
const SYSTEM_SAVE_DATA_CLASS_HASH: u32 = 0x5766_f30b;

const BBQ_NEW_MARK_BIT_LIST_FIELD_HASH: u32 = 0xea58_bbfe;
const DECIDE_DATA_FIELD_HASH: u32 = 0x9428_4f65;
const GOOD_FOLLOWER_DATA_LIST_FIELD_HASH: u32 = 0xb385_b976;
const GUILD_CARD_ID_FIELD_HASH: u32 = 0x85ef_5b34;
const HUNTER_UNIQUE_ID_FIELD_HASH: u32 = 0x0a96_0102;
const NETWORK_UNIQUE_ID_FIELD_HASH: u32 = 0x0737_8f29;
const NSA_ID_FIELD_HASH: u32 = 0xcce8_505f;
const NET_ERROR_BAN_QUEST_LIST_FIELD_HASH: u32 = 0xf29d_836b;
const PL_WEAPON_CHANGE_RECIPE_NEW_MARK_TABLE_FIELD_HASH: u32 = 0x4009_b7e0;
const PLATFORM_QUEST_STATE_LIST_FIELD_HASH: u32 = 0x2a41_ba2e;
const RANDOM_SEED_MANUAL_FIELD_HASH: u32 = 0xc85a_f19a;
const UNIQUE_ID_BYTE_ARRAY_FIELD_HASH: u32 = 0x9bbc_a62b;
const UNIQUE_ID_BIN_FIELD_HASH: u32 = 0xa291_e6f1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeReport {
  pub source_top_level_classes: usize,
  pub target_top_level_classes: usize,
  pub merged_top_level_classes: usize,
  pub preserved_target_top_level_classes: usize,
  pub copied_source_fields: usize,
  pub preserved_target_fields: usize,
}

pub fn merge_onto_template(
  source: &SavePayload,
  target_template: &SavePayload,
) -> (SavePayload, MergeReport) {
  let mut report = MergeReport {
    source_top_level_classes: source.entries.len(),
    target_top_level_classes: target_template.entries.len(),
    ..MergeReport::default()
  };

  let entries = target_template
    .entries
    .iter()
    .map(|target| merge_top_level(source, target, &mut report))
    .collect();

  (SavePayload { entries }, report)
}

fn merge_top_level(
  source: &SavePayload,
  target: &NativeClass,
  report: &mut MergeReport,
) -> NativeClass {
  if matches!(
    target.native_hash,
    SAVE_FILE_DETAIL_NATIVE_HASH
      | SNAPSHOT_SAVE_DATA_NATIVE_HASH
      | DLC_HUNTER_SAVE_NATIVE_HASH
      | EXTERNAL_DL_ITEM_PACK_NATIVE_HASH
  ) {
    report.preserved_target_top_level_classes += 1;
    return target.clone();
  }

  let Some(source) = source.entries.iter().find(|source| {
    source.native_hash == target.native_hash && source.class.hash == target.class.hash
  }) else {
    report.preserved_target_top_level_classes += 1;
    return target.clone();
  };

  report.merged_top_level_classes += 1;
  NativeClass {
    native_hash: target.native_hash,
    class: merge_class(&source.class, &target.class, report),
  }
}

fn merge_class(source: &Class, target: &Class, report: &mut MergeReport) -> Class {
  if source.hash != target.hash || source.hash == 0 {
    return source.clone();
  }

  let fields = target
    .fields
    .iter()
    .map(|target_field| {
      if preserves_target_platform_state(target.hash, target_field.hash) {
        report.preserved_target_fields += 1;
        return target_field.clone();
      }

      let source_field = source.fields.iter().find(|source_field| {
        source_field.hash == target_field.hash && source_field.field_type == target_field.field_type
      });
      match source_field {
        Some(source_field) => {
          report.copied_source_fields += 1;
          Field {
            hash: target_field.hash,
            field_type: target_field.field_type,
            value: merge_value(&source_field.value, &target_field.value, report),
          }
        }
        None => {
          report.preserved_target_fields += 1;
          target_field.clone()
        }
      }
    })
    .collect();

  Class { hash: target.hash, fields }
}

fn preserves_target_platform_state(class_hash: u32, field_hash: u32) -> bool {
  matches!(
    (class_hash, field_hash),
    (FACILITY_SAVE_DATA_CLASS_HASH, RANDOM_SEED_MANUAL_FIELD_HASH)
      | (
        FLAG_DATA_SAVE_DATA_CLASS_HASH,
        BBQ_NEW_MARK_BIT_LIST_FIELD_HASH | PL_WEAPON_CHANGE_RECIPE_NEW_MARK_TABLE_FIELD_HASH
      )
      | (
        GUILD_CARD_DATA_CLASS_HASH,
        GUILD_CARD_ID_FIELD_HASH | UNIQUE_ID_BYTE_ARRAY_FIELD_HASH | NSA_ID_FIELD_HASH
      )
      | (
        HUNTER_RECORD_CLASS_HASH,
        HUNTER_UNIQUE_ID_FIELD_HASH
          | NETWORK_UNIQUE_ID_FIELD_HASH
          | NSA_ID_FIELD_HASH
          | NET_ERROR_BAN_QUEST_LIST_FIELD_HASH
      )
      | (NETWORK_SAVE_DATA_CLASS_HASH, UNIQUE_ID_BIN_FIELD_HASH)
      | (OPTION_SYSTEM_SAVE_DATA_CLASS_HASH, DECIDE_DATA_FIELD_HASH)
      | (QUEST_SAVE_DATA_CLASS_HASH, PLATFORM_QUEST_STATE_LIST_FIELD_HASH)
      | (SYSTEM_SAVE_DATA_CLASS_HASH, GOOD_FOLLOWER_DATA_LIST_FIELD_HASH)
  )
}

fn merge_value(source: &FieldValue, target: &FieldValue, report: &mut MergeReport) -> FieldValue {
  match (source, target) {
    (FieldValue::Class(source), FieldValue::Class(target)) => {
      FieldValue::Class(Box::new(merge_class(source, target, report)))
    }
    (FieldValue::Array(source), FieldValue::Array(target)) => {
      FieldValue::Array(merge_array(source, target, report))
    }
    _ => source.clone(),
  }
}

fn merge_array(source: &Array, target: &Array, report: &mut MergeReport) -> Array {
  if source.member_type != target.member_type || source.array_type != target.array_type {
    report.preserved_target_fields += 1;
    return target.clone();
  }

  let values = source
    .values
    .iter()
    .enumerate()
    .map(|(index, source_value)| match source_value {
      ArrayValue::Class(source_class) => {
        let target_class = target
          .values
          .get(index)
          .and_then(as_class)
          .filter(|class| class.hash == source_class.hash)
          .or_else(|| {
            target.values.iter().filter_map(as_class).find(|class| class.hash == source_class.hash)
          });
        match target_class {
          Some(target_class) => {
            ArrayValue::Class(Box::new(merge_class(source_class, target_class, report)))
          }
          None => source_value.clone(),
        }
      }
      _ => source_value.clone(),
    })
    .collect();

  Array {
    member_type: source.member_type,
    member_size: source.member_size,
    array_type: source.array_type,
    class_hashes: source.class_hashes.clone(),
    values,
  }
}

fn as_class(value: &ArrayValue) -> Option<&Class> {
  match value {
    ArrayValue::Class(class) => Some(class),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scalar(value: u32) -> FieldValue {
    FieldValue::Scalar { size: 4, bytes: value.to_le_bytes().to_vec() }
  }

  fn field(hash: u32, value: u32) -> Field {
    Field { hash, field_type: 8, value: scalar(value) }
  }

  #[test]
  fn follows_target_schema_and_copies_matching_source_values() {
    let source = SavePayload {
      entries: vec![NativeClass {
        native_hash: 1,
        class: Class { hash: 10, fields: vec![field(100, 1), field(101, 2)] },
      }],
    };
    let target = SavePayload {
      entries: vec![
        NativeClass {
          native_hash: 1,
          class: Class { hash: 10, fields: vec![field(100, 9), field(102, 3)] },
        },
        NativeClass { native_hash: 2, class: Class { hash: 20, fields: vec![field(200, 4)] } },
      ],
    };

    let (merged, report) = merge_onto_template(&source, &target);

    assert_eq!(merged.entries.len(), 2);
    assert_eq!(merged.entries[0].class.fields, vec![field(100, 1), field(102, 3)]);
    assert_eq!(merged.entries[1], target.entries[1]);
    assert_eq!(report.merged_top_level_classes, 1);
    assert_eq!(report.preserved_target_top_level_classes, 1);
    assert_eq!(report.copied_source_fields, 1);
    assert_eq!(report.preserved_target_fields, 1);
  }

  #[test]
  fn preserves_target_file_detail_metadata() {
    let source = SavePayload {
      entries: vec![NativeClass {
        native_hash: SAVE_FILE_DETAIL_NATIVE_HASH,
        class: Class { hash: 10, fields: vec![field(100, 1)] },
      }],
    };
    let target = SavePayload {
      entries: vec![NativeClass {
        native_hash: SAVE_FILE_DETAIL_NATIVE_HASH,
        class: Class { hash: 10, fields: vec![field(100, 9)] },
      }],
    };

    let (merged, report) = merge_onto_template(&source, &target);

    assert_eq!(merged, target);
    assert_eq!(report.preserved_target_top_level_classes, 1);
    assert_eq!(report.copied_source_fields, 0);
  }

  #[test]
  fn preserves_target_telemetry_snapshot() {
    let source = SavePayload {
      entries: vec![NativeClass {
        native_hash: SNAPSHOT_SAVE_DATA_NATIVE_HASH,
        class: Class { hash: 10, fields: vec![field(100, 1)] },
      }],
    };
    let target = SavePayload {
      entries: vec![NativeClass {
        native_hash: SNAPSHOT_SAVE_DATA_NATIVE_HASH,
        class: Class { hash: 10, fields: vec![field(100, 9)] },
      }],
    };

    let (merged, report) = merge_onto_template(&source, &target);

    assert_eq!(merged, target);
    assert_eq!(report.preserved_target_top_level_classes, 1);
    assert_eq!(report.copied_source_fields, 0);
  }

  #[test]
  fn preserves_target_account_bound_top_level_state() {
    for native_hash in [DLC_HUNTER_SAVE_NATIVE_HASH, EXTERNAL_DL_ITEM_PACK_NATIVE_HASH] {
      let source = SavePayload {
        entries: vec![NativeClass {
          native_hash,
          class: Class { hash: 10, fields: vec![field(100, 1)] },
        }],
      };
      let target = SavePayload {
        entries: vec![NativeClass {
          native_hash,
          class: Class { hash: 10, fields: vec![field(100, 9)] },
        }],
      };

      let (merged, report) = merge_onto_template(&source, &target);

      assert_eq!(merged, target);
      assert_eq!(report.preserved_target_top_level_classes, 1);
      assert_eq!(report.copied_source_fields, 0);
    }
  }

  #[test]
  fn preserves_target_confirm_button_without_resetting_other_options() {
    // SystemSaveData.DecideData; values are deliberately different, not interpreted as A/B.
    const DECIDE_DATA: u32 = 0x9428_4f65;
    let options = |decide| NativeClass {
      native_hash: 0xb0ca_70c9,
      class: Class {
        hash: 0x34c5_d174,
        fields: vec![
          Field { hash: DECIDE_DATA, field_type: 7, value: scalar(decide) },
          field(100, decide + 10),
        ],
      },
    };
    for (source_decide, target_decide) in [(0, 1), (1, 0)] {
      let source = SavePayload {
        entries: vec![
          options(source_decide),
          NativeClass {
            native_hash: 1,
            class: Class { hash: 10, fields: vec![field(DECIDE_DATA, 42)] },
          },
        ],
      };
      let target = SavePayload {
        entries: vec![
          options(target_decide),
          NativeClass {
            native_hash: 1,
            class: Class { hash: 10, fields: vec![field(DECIDE_DATA, 9)] },
          },
        ],
      };
      let (merged, report) = merge_onto_template(&source, &target);
      assert_eq!(merged.entries[0].class.fields[0], target.entries[0].class.fields[0]);
      assert_eq!(merged.entries[0].class.fields[1], source.entries[0].class.fields[1]);
      assert_eq!(merged.entries[1], source.entries[1]);
      assert_eq!(report.preserved_target_fields, 1);
      assert_eq!(report.copied_source_fields, 2);
    }
  }

  #[test]
  fn preserves_target_platform_state_fields() {
    let platform_fields = [
      (FACILITY_SAVE_DATA_CLASS_HASH, RANDOM_SEED_MANUAL_FIELD_HASH),
      (FLAG_DATA_SAVE_DATA_CLASS_HASH, BBQ_NEW_MARK_BIT_LIST_FIELD_HASH),
      (FLAG_DATA_SAVE_DATA_CLASS_HASH, PL_WEAPON_CHANGE_RECIPE_NEW_MARK_TABLE_FIELD_HASH),
      (GUILD_CARD_DATA_CLASS_HASH, GUILD_CARD_ID_FIELD_HASH),
      (GUILD_CARD_DATA_CLASS_HASH, UNIQUE_ID_BYTE_ARRAY_FIELD_HASH),
      (GUILD_CARD_DATA_CLASS_HASH, NSA_ID_FIELD_HASH),
      (HUNTER_RECORD_CLASS_HASH, HUNTER_UNIQUE_ID_FIELD_HASH),
      (HUNTER_RECORD_CLASS_HASH, NETWORK_UNIQUE_ID_FIELD_HASH),
      (HUNTER_RECORD_CLASS_HASH, NSA_ID_FIELD_HASH),
      (HUNTER_RECORD_CLASS_HASH, NET_ERROR_BAN_QUEST_LIST_FIELD_HASH),
      (NETWORK_SAVE_DATA_CLASS_HASH, UNIQUE_ID_BIN_FIELD_HASH),
      (QUEST_SAVE_DATA_CLASS_HASH, PLATFORM_QUEST_STATE_LIST_FIELD_HASH),
      (SYSTEM_SAVE_DATA_CLASS_HASH, GOOD_FOLLOWER_DATA_LIST_FIELD_HASH),
    ];

    for (class_hash, field_hash) in platform_fields {
      let source = SavePayload {
        entries: vec![NativeClass {
          native_hash: 1,
          class: Class { hash: class_hash, fields: vec![field(field_hash, 1)] },
        }],
      };
      let target = SavePayload {
        entries: vec![NativeClass {
          native_hash: 1,
          class: Class { hash: class_hash, fields: vec![field(field_hash, 9)] },
        }],
      };

      let (merged, report) = merge_onto_template(&source, &target);

      assert_eq!(merged, target);
      assert_eq!(report.copied_source_fields, 0);
      assert_eq!(report.preserved_target_fields, 1);
    }
  }
}
