use std::{collections::BTreeSet, fs, path::Path};

const TOOLS: &[&str] = &[
    "acquire_control",
    "authorization_status",
    "close_splint",
    "close_dojo",
    "create_lair",
    "decide_control_transfer",
    "input",
    "inspect_audit",
    "inspect_splint",
    "inspect_topology",
    "kill_splint",
    "list_lairs",
    "new_dojo",
    "ping",
    "read_scrollback",
    "read_terminal",
    "relaunch_splint",
    "release_control",
    "rename_lair",
    "rename_splint",
    "rename_dojo",
    "report_agent_status",
    "request_access",
    "request_control_transfer",
    "request_lair_access",
    "resize",
    "restore_lair",
    "restore_splint",
    "restore_dojo",
    "revoke_access",
    "search_scrollback",
    "set_split_ratio",
    "set_dojo_default_focus",
    "split_splint",
];

const ERROR_CODES: &[&str] = &[
    "authentication_failed",
    "handshake_required",
    "incompatible_version",
    "invalid_request",
    "unsupported_schema",
    "consent_unavailable",
    "consent_denied",
    "unauthorized",
    "confirmation_required",
    "controller_unavailable",
    "control_transfer_unavailable",
    "stale_topology",
    "not_found",
    "stale_incarnation",
    "invalid_argument",
    "resource_limit",
    "cancelled",
    "timeout",
    "internal",
];

const REVIEWED_SCHEMA_FNV64: &[(&str, u64)] = &[
    ("common.schema.json", 0x48fa_8a13_da28_baa8),
    ("error.schema.json", 0x9dd2_e2af_f101_25fe),
    ("resources/control.schema.json", 0xd4f3_1fbc_06b2_5c2b),
    ("resources/terminal.schema.json", 0x20c0_6b33_9e25_c903),
    ("resources/topology.schema.json", 0x9997_f7e0_b8cc_8fec),
    (
        "tools/acquire_control.input.schema.json",
        0x2ca3_3fb5_40b3_2b83,
    ),
    (
        "tools/acquire_control.output.schema.json",
        0xd76a_acdc_0311_6830,
    ),
    (
        "tools/authorization_status.input.schema.json",
        0xbc38_208b_d34f_5b43,
    ),
    (
        "tools/authorization_status.output.schema.json",
        0xe4f3_d913_1efb_cca2,
    ),
    ("tools/close_dojo.input.schema.json", 0x8a94_83a8_db18_ddda),
    ("tools/close_dojo.output.schema.json", 0xa13a_7239_1643_8d21),
    (
        "tools/close_splint.input.schema.json",
        0x6ba8_fd4c_9e76_cab2,
    ),
    (
        "tools/close_splint.output.schema.json",
        0xf067_0bf7_49eb_d415,
    ),
    ("tools/create_lair.input.schema.json", 0x0916_f9d5_2ed4_3c8f),
    (
        "tools/create_lair.output.schema.json",
        0x9c69_4126_cdd4_97e5,
    ),
    (
        "tools/decide_control_transfer.input.schema.json",
        0x8b14_f73d_25f5_3a33,
    ),
    (
        "tools/decide_control_transfer.output.schema.json",
        0xaff5_14f4_0f32_0867,
    ),
    ("tools/input.input.schema.json", 0xe79c_c053_2e93_6d2a),
    ("tools/input.output.schema.json", 0x6f42_6194_56e8_137c),
    (
        "tools/inspect_audit.input.schema.json",
        0x4098_738c_dde4_d3b3,
    ),
    (
        "tools/inspect_audit.output.schema.json",
        0x32d1_667b_845e_60ff,
    ),
    (
        "tools/inspect_splint.input.schema.json",
        0x8e0b_c2fa_1001_22cf,
    ),
    (
        "tools/inspect_splint.output.schema.json",
        0xb17a_faa3_6121_fc69,
    ),
    (
        "tools/inspect_topology.input.schema.json",
        0x49cf_480c_1bec_611a,
    ),
    (
        "tools/inspect_topology.output.schema.json",
        0xf7ce_3c0b_1d85_b023,
    ),
    ("tools/kill_splint.input.schema.json", 0x3ec9_951a_7d7a_8daa),
    (
        "tools/kill_splint.output.schema.json",
        0xe316_2dda_acca_4f2f,
    ),
    ("tools/list_lairs.input.schema.json", 0xb83a_6e8c_b692_a7b6),
    ("tools/list_lairs.output.schema.json", 0xa683_c49b_7cbb_f6e1),
    ("tools/new_dojo.input.schema.json", 0x76c4_e647_bbd6_8ed9),
    ("tools/new_dojo.output.schema.json", 0xfaa0_8cab_df12_f342),
    ("tools/ping.input.schema.json", 0x330d_9f93_c9bc_3dea),
    ("tools/ping.output.schema.json", 0xaf91_1919_d5ed_5480),
    (
        "tools/read_scrollback.input.schema.json",
        0x0f87_0822_8907_a0ce,
    ),
    (
        "tools/read_scrollback.output.schema.json",
        0x2180_bc84_7c5a_3a9b,
    ),
    (
        "tools/read_terminal.input.schema.json",
        0x1800_0280_51cd_5cdf,
    ),
    (
        "tools/read_terminal.output.schema.json",
        0x6bc1_c075_647e_0829,
    ),
    (
        "tools/relaunch_splint.input.schema.json",
        0x49ae_a46d_9d5a_df99,
    ),
    (
        "tools/relaunch_splint.output.schema.json",
        0xa6be_a9f9_86c7_02fd,
    ),
    (
        "tools/release_control.input.schema.json",
        0x3d30_bd8b_2bdc_fa95,
    ),
    (
        "tools/release_control.output.schema.json",
        0x6517_d088_c2c4_22c9,
    ),
    ("tools/rename_dojo.input.schema.json", 0xa351_c1fd_b43d_e1a8),
    (
        "tools/rename_dojo.output.schema.json",
        0xdab3_8d9f_db5c_757a,
    ),
    ("tools/rename_lair.input.schema.json", 0xfa87_d6b1_1bc0_ff80),
    (
        "tools/rename_lair.output.schema.json",
        0x92a8_044b_3dc0_c8e6,
    ),
    (
        "tools/rename_splint.input.schema.json",
        0x52cf_0e0e_0101_557a,
    ),
    (
        "tools/rename_splint.output.schema.json",
        0x39ae_3deb_38d9_8bb6,
    ),
    (
        "tools/report_agent_status.input.schema.json",
        0x2585_acb6_fc43_7989,
    ),
    (
        "tools/report_agent_status.output.schema.json",
        0x2643_4744_05cf_0175,
    ),
    (
        "tools/request_access.input.schema.json",
        0xb4af_1008_7282_c553,
    ),
    (
        "tools/request_access.output.schema.json",
        0x3e63_a65e_b3a0_5e83,
    ),
    (
        "tools/request_lair_access.input.schema.json",
        0x22e5_3fcb_c996_793c,
    ),
    (
        "tools/request_lair_access.output.schema.json",
        0x7491_1aca_d46d_3c0a,
    ),
    (
        "tools/request_control_transfer.input.schema.json",
        0xd5ee_8269_2bb2_e35d,
    ),
    (
        "tools/request_control_transfer.output.schema.json",
        0x170a_ac8c_e6e5_59a3,
    ),
    ("tools/resize.input.schema.json", 0x1a8c_b25e_4606_a63f),
    ("tools/resize.output.schema.json", 0x0e4a_3a70_7d47_3460),
    (
        "tools/restore_dojo.input.schema.json",
        0xf338_b917_05b9_35af,
    ),
    (
        "tools/restore_dojo.output.schema.json",
        0x7b04_1551_7b32_9a6f,
    ),
    (
        "tools/restore_lair.input.schema.json",
        0x436d_bcf5_ce37_83c3,
    ),
    (
        "tools/restore_lair.output.schema.json",
        0x777f_ea53_ba8b_cedb,
    ),
    (
        "tools/restore_splint.input.schema.json",
        0xf2e6_93df_bf17_2557,
    ),
    (
        "tools/restore_splint.output.schema.json",
        0x932d_87e8_476c_411e,
    ),
    (
        "tools/revoke_access.input.schema.json",
        0xfe63_60e0_216e_d22d,
    ),
    (
        "tools/revoke_access.output.schema.json",
        0xd1e0_3c0f_daee_fffe,
    ),
    (
        "tools/search_scrollback.input.schema.json",
        0x0d87_7d02_396e_f9f9,
    ),
    (
        "tools/search_scrollback.output.schema.json",
        0xa615_efec_c692_3ae0,
    ),
    (
        "tools/set_dojo_default_focus.input.schema.json",
        0x598f_8fb2_7aac_6402,
    ),
    (
        "tools/set_dojo_default_focus.output.schema.json",
        0x8567_1ff1_faf8_4b33,
    ),
    (
        "tools/set_split_ratio.input.schema.json",
        0xe0bf_e828_9a61_b189,
    ),
    (
        "tools/set_split_ratio.output.schema.json",
        0xb9d0_80fd_3ce1_cc7b,
    ),
    (
        "tools/split_splint.input.schema.json",
        0xfc84_1a6c_24b3_2346,
    ),
    (
        "tools/split_splint.output.schema.json",
        0x79e4_1044_60a9_7ee3,
    ),
];

fn file_stems(directory: &Path, suffix: &str) -> BTreeSet<String> {
    fs::read_dir(directory)
        .expect("schema directory must exist")
        .filter_map(|entry| {
            let entry = entry.expect("schema directory entry must be readable");
            if !entry
                .file_type()
                .expect("schema entry type must be readable")
                .is_file()
            {
                return None;
            }
            let name = entry
                .file_name()
                .into_string()
                .expect("schema filenames must be UTF-8");
            name.strip_suffix(suffix).map(str::to_owned)
        })
        .collect()
}

fn canonical_json(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Null => output.push_str("null"),
        serde_json::Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(value) => output.push_str(&value.to_string()),
        serde_json::Value::String(value) => {
            output.push_str(&serde_json::to_string(value).expect("string must serialize"));
        }
        serde_json::Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical_json(value, output);
            }
            output.push(']');
        }
        serde_json::Value::Object(values) => {
            output.push('{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).expect("object key must serialize"));
                output.push(':');
                canonical_json(value, output);
            }
            output.push('}');
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn schema_inventory_is_exactly_34_tools_and_three_resources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let schema_root = root.join("dist/schemas/mcp/v2");
    let tools = schema_root.join("tools");
    let resources = schema_root.join("resources");

    assert_eq!(TOOLS.len(), 34);
    let expected_tools = TOOLS
        .iter()
        .map(|tool| (*tool).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(file_stems(&tools, ".input.schema.json"), expected_tools);
    assert_eq!(file_stems(&tools, ".output.schema.json"), expected_tools);
    assert_eq!(
        file_stems(&resources, ".schema.json"),
        ["control", "terminal", "topology"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert_eq!(
        file_stems(&schema_root, ".schema.json"),
        ["common", "error"].into_iter().map(str::to_owned).collect()
    );

    let common: serde_json::Value = serde_json::from_slice(
        &fs::read(schema_root.join("common.schema.json")).expect("common schema must be readable"),
    )
    .expect("common schema must be valid JSON");
    let actual_error_codes = common["$defs"]["error_code"]["enum"]
        .as_array()
        .expect("error_code must be an enum")
        .iter()
        .map(|code| code.as_str().expect("error code must be a string"))
        .collect::<Vec<_>>();
    assert_eq!(actual_error_codes, ERROR_CODES);

    assert_eq!(REVIEWED_SCHEMA_FNV64.len(), 73);
    for (relative_path, expected_hash) in REVIEWED_SCHEMA_FNV64 {
        let path = schema_root.join(relative_path);
        let schema: serde_json::Value = serde_json::from_slice(
            &fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let expected_id = format!("https://splinterm.oldjobobo.com/schemas/mcp/v2/{relative_path}");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema",
            "{} has noncanonical $schema",
            path.display()
        );
        assert_eq!(
            schema["$id"],
            expected_id,
            "{} has an ID that does not match its path",
            path.display()
        );
        let mut canonical = String::new();
        canonical_json(&schema, &mut canonical);
        assert_eq!(
            fnv1a64(canonical.as_bytes()),
            *expected_hash,
            "{} differs from its reviewed structural hash",
            path.display()
        );
    }
}
