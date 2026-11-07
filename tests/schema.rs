//! Schema contract tests: validity, constraint presence, result validation, corpus agreement,
//! golden tool definition, and refusal of external references.

use std::path::PathBuf;

use serde_json::{Value, json};
use token_saver::probe;

fn obj(v: &std::sync::Arc<rmcp::model::JsonObject>) -> Value {
    Value::Object((**v).clone())
}

#[test]
fn schemas_are_valid_json_schema_2020_12() {
    jsonschema::validator_for(&obj(&probe::input_schema())).expect("input schema valid");
    jsonschema::validator_for(&obj(&probe::output_schema())).expect("output schema valid");
}

#[test]
fn schema_constraints_are_present() {
    let input = obj(&probe::input_schema());
    assert_eq!(input["additionalProperties"], false);
    assert_eq!(input["required"], json!(["nonce"]));
    assert_eq!(input["properties"]["nonce"]["minLength"], 1);
    assert_eq!(input["properties"]["nonce"]["maxLength"], 64);
    assert_eq!(input["properties"]["nonce"]["pattern"], "^[A-Za-z0-9]+$");
    let output = obj(&probe::output_schema());
    assert_eq!(output["additionalProperties"], false);
    assert_eq!(
        output["required"],
        json!(["nonce", "instance_id", "server_version", "status"])
    );
    assert_eq!(
        output["properties"]["instance_id"]["pattern"],
        "^[0-9a-f]{32}$"
    );
    assert_eq!(output["properties"]["status"]["const"], "ok");
    assert_eq!(output["properties"]["server_version"]["maxLength"], 16);
    assert!(probe::VERSION.len() <= 16);
}

#[test]
fn success_results_validate_against_output_schema() {
    let validator = jsonschema::validator_for(&obj(&probe::output_schema())).unwrap();
    let id = probe::new_instance_id();
    for nonce in ["a", "Nonce123", &"z".repeat(64)] {
        let args = json!({"nonce": nonce}).as_object().cloned().unwrap();
        let result = probe::evaluate(&id, Some(&args)).expect("not oversized");
        let sc = result.structured_content.clone().expect("structured");
        assert!(
            validator.is_valid(&sc),
            "structured content violates output schema: {sc}"
        );
        let mirror: Value =
            serde_json::from_str(result.content[0].as_text().unwrap().text.as_str()).unwrap();
        assert_eq!(mirror, sc);
        assert_eq!(result.is_error, Some(false));
        let size = serde_json::to_vec(&result).unwrap().len();
        assert!(size <= 1024, "{size}");
    }
}

#[test]
fn validator_and_handler_agree_on_argument_corpus() {
    let validator = jsonschema::validator_for(&obj(&probe::input_schema())).unwrap();
    let id = probe::new_instance_id();
    let corpus: Vec<(Value, bool)> = vec![
        (json!({"nonce":"a"}), true),
        (json!({"nonce":"A1"}), true),
        (json!({"nonce":"x".repeat(64)}), true),
        (json!({"nonce":""}), false),
        (json!({"nonce":"x".repeat(65)}), false),
        (json!({"nonce":"a b"}), false),
        (json!({"nonce":"caf\u{e9}"}), false),
        (json!({"nonce":"a-b"}), false),
        (json!({"nonce":"a_b"}), false),
        (json!({"nonce":5}), false),
        (json!({"nonce":null}), false),
        (json!({}), false),
        (json!({"nonce":"ok","extra":1}), false),
    ];
    for (args, valid) in corpus {
        let schema_says = validator.is_valid(&args);
        let result = probe::evaluate(&id, args.as_object()).unwrap();
        let handler_says = result.is_error != Some(true);
        assert_eq!(schema_says, valid, "schema on {args}");
        assert_eq!(handler_says, valid, "handler on {args}");
    }
    // Absent arguments: handler rejects; schema requires an object so there is nothing to validate.
    assert_eq!(probe::evaluate(&id, None).unwrap().is_error, Some(true));
}

#[test]
fn external_references_are_rejected_by_the_pinned_validator() {
    let schema = json!({"$ref": "https://example.invalid/schema.json"});
    assert!(
        jsonschema::validator_for(&schema).is_err(),
        "remote $ref must not resolve"
    );
    let file_schema = json!({"$ref": "file:///etc/hosts"});
    assert!(
        jsonschema::validator_for(&file_schema).is_err(),
        "file $ref must not resolve"
    );
}

#[test]
fn tool_definition_matches_golden() {
    let tool = serde_json::to_value(probe::tool()).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool.golden.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&tool).unwrap() + "\n").unwrap();
    }
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("golden file present"))
            .unwrap();
    assert_eq!(
        tool, golden,
        "tool definition changed; review and set UPDATE_GOLDEN=1 to accept"
    );
    assert_eq!(tool["name"], "connection_probe");
    assert!(tool["name"].as_str().unwrap().len() <= 64);
    assert!(tool["description"].as_str().unwrap().len() <= 1024);
    assert_eq!(
        tool["annotations"],
        json!({"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})
    );
}

#[test]
fn adapter_and_handler_advertise_the_same_versions() {
    assert_eq!(
        probe::supported_version_strings(),
        token_saver::limits::SUPPORTED_PROTOCOL_VERSIONS.to_vec()
    );
}
