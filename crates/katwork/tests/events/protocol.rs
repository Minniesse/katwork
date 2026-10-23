use std::{fs, path::Path, sync::LazyLock};

use katwork::{events::*, types::SideEffect};
use serde_json::{Value, json};

static VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    let schema: Value = serde_json::from_str(include_str!("../fixtures/agui/schema.json")).unwrap();
    jsonschema::draft202012::new(&schema).unwrap()
});

fn spec_fixtures() -> Vec<(String, Value)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agui/spec");
    let mut files = Vec::new();
    for event in fs::read_dir(&root).unwrap() {
        for file in fs::read_dir(event.unwrap().path().join("valid")).unwrap() {
            let path = file.unwrap().path();
            files.push((
                path.strip_prefix(&root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap(),
            ));
        }
    }
    files
}

#[test]
fn events_serialize_as_agui_json() {
    assert_eq!(
        serde_json::to_value(super::delta("สวัสดี 👋🏽")).unwrap(),
        json!({"type":"TEXT_MESSAGE_CONTENT","messageId":"reply","delta":"สวัสดี 👋🏽"})
    );
}

#[test]
fn agui_events_validate_against_vendored_1_0_schema() {
    let events = [
        AgUiEvent::RunStarted {
            thread_id: "t".into(),
            run_id: "r".into(),
            protocol_version: Some("1.0".into()),
        },
        AgUiEvent::TextMessageStart {
            message_id: "m".into(),
            role: Some("assistant".into()),
        },
        super::delta(""),
        AgUiEvent::TextMessageEnd {
            message_id: "m".into(),
        },
        AgUiEvent::ToolCallStart {
            tool_call_id: "c".into(),
            tool_call_name: "read_thread".into(),
            parent_message_id: None,
        },
        AgUiEvent::ToolCallArgs {
            tool_call_id: "c".into(),
            delta: "{}".into(),
        },
        AgUiEvent::ToolCallEnd {
            tool_call_id: "c".into(),
        },
        AgUiEvent::ToolCallResult {
            message_id: "m".into(),
            tool_call_id: "c".into(),
            content: "text".into(),
        },
        AgUiEvent::RunFinished {
            thread_id: "t".into(),
            run_id: "r".into(),
            outcome: None,
        },
        AgUiEvent::RunError {
            message: "cancelled".into(),
            code: Some("cancelled".into()),
        },
        AgUiEvent::Custom {
            name: "katwork.kat_state".into(),
            value: json!({"state":"idle"}),
        },
    ];
    for event in events {
        let mut value = serde_json::to_value(event).unwrap();
        assert!(VALIDATOR.is_valid(&value), "{value}");
        value["strayField"] = json!(true);
        assert!(
            !VALIDATOR.is_valid(&value),
            "schema must reject stray fields"
        );
    }
}

#[test]
fn agui_spec_valid_fixtures_validate() {
    let fixtures = spec_fixtures();
    assert_eq!(fixtures.len(), 46);
    for (name, value) in fixtures {
        assert!(VALIDATOR.is_valid(&value), "{name}: {value}");
    }
}

#[test]
fn agui_spec_valid_fixtures_roundtrip() {
    let fixtures = spec_fixtures();
    for (name, reason) in super::SKIPPED_FIXTURES {
        assert!(!reason.is_empty());
        assert!(
            fixtures.iter().any(|(path, _)| path == name),
            "missing skip: {name}"
        );
    }
    for (name, value) in fixtures {
        let roundtrip = serde_json::from_value::<AgUiEvent>(value.clone())
            .ok()
            .map(|event| serde_json::to_value(event).unwrap());
        if super::SKIPPED_FIXTURES
            .iter()
            .any(|(path, _)| *path == name)
        {
            assert_ne!(roundtrip, Some(value), "remove obsolete skip: {name}");
        } else {
            assert_eq!(roundtrip, Some(value), "{name}");
        }
    }
}

#[test]
fn katwork_payloads_match_their_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agui/katwork");
    let fixtures: Vec<_> = fs::read_dir(root).unwrap().collect();
    assert_eq!(fixtures.len(), 8);
    for file in fixtures {
        let value: Value =
            serde_json::from_slice(&fs::read(file.unwrap().path()).unwrap()).unwrap();
        let event: KatworkEvent = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), value);
        let agui = serde_json::to_value(AgUiEvent::from(event)).unwrap();
        assert_eq!(
            agui,
            json!({"type":"CUSTOM", "name":value["name"],"value":value["value"]})
        );
        assert!(VALIDATOR.is_valid(&agui));
    }
}

#[test]
fn kat_state_for_tool_matches_effect() {
    for effect in [
        SideEffect::Read,
        SideEffect::Navigate,
        SideEffect::WorkspaceRead,
    ] {
        assert_eq!(KatState::for_tool(effect), KatState::Researching);
    }
    for effect in [
        SideEffect::Draft,
        SideEffect::WorkspaceWrite,
        SideEffect::Exec,
        SideEffect::CredentialUse,
        SideEffect::Destructive,
        SideEffect::Outward(katwork::types::OutwardKind::Send),
    ] {
        assert_eq!(KatState::for_tool(effect), KatState::Working);
    }
}
