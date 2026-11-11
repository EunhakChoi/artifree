use serde_json::json;

use super::{DATA_READ, Lifecycle, METADATA_READ, RuntimePolicy};

const ADOPT_MANIFEST: &str =
    r#"{"sdk":"artifree.app.v1","lifecycle":"adopt","capabilities":["data.read"]}"#;

#[test]
fn lifecycle_is_restart_unless_the_manifest_says_adopt() -> Result<(), Box<dyn std::error::Error>> {
    let cases: [(bool, Option<&str>, Lifecycle, &[&str]); 6] = [
        (false, None, Lifecycle::Restart, &[]),
        (true, None, Lifecycle::Restart, &[METADATA_READ]),
        (false, Some(ADOPT_MANIFEST), Lifecycle::Restart, &[]),
        (
            true,
            Some(r#"{"sdk":"artifree.app.v1"}"#),
            Lifecycle::Restart,
            &[],
        ),
        (
            true,
            Some(r#"{"sdk":"artifree.app.v1","lifecycle":"restart"}"#),
            Lifecycle::Restart,
            &[],
        ),
        (true, Some(ADOPT_MANIFEST), Lifecycle::Adopt, &[DATA_READ]),
    ];
    for (enabled, source, lifecycle, declared) in cases {
        let policy = RuntimePolicy::from_source(enabled, source.map(str::as_bytes))?;
        assert_eq!(
            policy.lifecycle(),
            lifecycle,
            "enabled {enabled}, source {source:?}"
        );
        assert_eq!(
            policy.declared(),
            declared,
            "enabled {enabled}, source {source:?}"
        );
    }
    Ok(())
}

#[test]
fn any_other_lifecycle_makes_the_manifest_malformed() {
    let values = [
        json!("Adopt"),
        json!("ADOPT"),
        json!(""),
        json!("reload"),
        json!("adopt "),
        json!(null),
        json!(true),
        json!(1),
        json!(["adopt"]),
        json!({"adopt": null}),
        json!({"Adopt": null}),
    ];
    for value in values {
        let source = json!({"sdk": "artifree.app.v1", "lifecycle": value}).to_string();
        assert_eq!(
            RuntimePolicy::from_source(true, Some(source.as_bytes())),
            Err("invalid runtime declaration"),
            "lifecycle {value} in {source} must make the manifest malformed"
        );
    }
}
