//! The skeleton's protocol behavior through its `handle` export.
use memoire_catalog::handle;
use serde_json::{Value, json};

fn call(request: Value) -> Value {
    serde_json::from_str(&handle(&request.to_string()).expect("the ABI call succeeds")).unwrap()
}

#[test]
fn describe_reports_protocol_2() {
    let response = call(json!({"version": 2, "body": {"op": "describe"}}));
    assert_eq!(response["version"], 2);
    assert_eq!(response["outcome"]["status"], "ok");
    assert_eq!(response["outcome"]["reply"]["protocol"], 2);
}

#[test]
fn known_commands_are_not_implemented_yet() {
    let response = call(json!({
        "version": 2,
        "body": {"op": "validate", "kind": "command", "tag": "CreateDatabase", "payload": {}},
    }));
    assert_eq!(response["outcome"]["status"], "err");
    assert_eq!(response["outcome"]["error"]["code"], "not_implemented");
}

#[test]
fn unknown_commands_are_refused() {
    let response = call(json!({
        "version": 2,
        "body": {"op": "validate", "kind": "command", "tag": "DropEverything", "payload": {}},
    }));
    assert_eq!(response["outcome"]["error"]["code"], "unknown_command");
}
