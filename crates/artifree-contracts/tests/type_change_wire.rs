//! Behavioral checks for the `type` of a change event (decision 0059 rule 8):
//! the member is always present, `null` for an untyped artifact, and a row
//! without it is refused as the published schema and the TypeScript client
//! refuse it.

use artifree_contracts::api::ArtifactChange;
use serde_json::{Value, json};

/// A `created` change event as the server writes it, with `artifact_type` as
/// its `type` member, or no member at all for `None`.
fn created_with(artifact_type: Option<Value>) -> Value {
    let mut body = json!({
        "cursor": "c.1", "artifact_id": "art_0", "change": "created",
        "revision": 1, "metadata_revision": 1, "committed_at_ms": 1
    });
    if let Some(value) = artifact_type {
        body["type"] = value;
    }
    body
}

#[test]
fn a_change_without_a_type_member_is_refused() {
    let Err(error) = serde_json::from_value::<ArtifactChange>(created_with(None)) else {
        panic!("a change event without `type` was accepted");
    };
    assert!(error.is_data(), "expected a data error, got {error}");
    assert!(
        error.to_string().contains("missing field `type`"),
        "expected the missing member to be named, got {error}"
    );
}

#[test]
fn a_typed_and_an_untyped_change_are_read_and_written_back() -> Result<(), serde_json::Error> {
    for artifact_type in [Value::Null, json!({ "id": "react", "release": 1 })] {
        let body = created_with(Some(artifact_type));
        let change: ArtifactChange = serde_json::from_value(body.clone())?;
        assert_eq!(serde_json::to_value(&change)?, body);
    }
    Ok(())
}
