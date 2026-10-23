use std::path::PathBuf;

use katwork::types::ReasonCode;
use serde_json::{Map, Value};

fn catalogue(locale: &str) -> Map<String, Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/src/i18n")
        .join(format!("{locale}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    match serde_json::from_str(&text).unwrap() {
        Value::Object(map) => map,
        other => panic!("{locale}.json is not an object: {other}"),
    }
}

#[test]
fn reason_codes_have_en_and_th_messages() {
    for locale in ["en", "th"] {
        let messages = catalogue(locale);
        for code in ReasonCode::ALL {
            let key = format!("reason.{}", code.wire());
            let message = messages.get(&key).and_then(Value::as_str);
            assert!(
                message.is_some_and(|text| !text.trim().is_empty()),
                "{locale}.json has no message for {key}"
            );
        }
    }
}
