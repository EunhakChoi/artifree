//! Behavioral checks for the `job_id` of a change event (decision 0055, rule
//! 8): a `job_changed` event names its job, every other kind refuses the
//! member, and the published schema states both rules.

use artifree_contracts::api::ArtifactChange;
use schemars::generate::SchemaSettings;
use serde_json::Value;

/// A well-formed job identifier: `job_` and 32 lowercase hex digits.
const JOB_ID: &str = "job_0123456789abcdef0123456789abcdef";

/// A change event of `kind` as the server writes it: the fields every kind has,
/// with `members` added.
fn change_with(kind: &str, members: &[(&str, Value)]) -> Value {
    let mut body = serde_json::json!({
        "cursor": "c.1", "artifact_id": "art_0", "change": kind,
        "revision": 1, "metadata_revision": 1, "committed_at_ms": 1
    });
    for (name, value) in members {
        body[*name] = value.clone();
    }
    body
}

/// A valid change event of each kind that carries no job, as the server writes
/// it. Only `data_changed` has a data version (decision 0040).
fn bodies_without_a_job() -> Vec<(&'static str, Value)> {
    let mut bodies: Vec<(&'static str, Value)> = [
        "created",
        "revised",
        "metadata_updated",
        "deleted",
        "recovered",
        "purged",
    ]
    .into_iter()
    .map(|kind| (kind, change_with(kind, &[])))
    .collect();
    let data = change_with("data_changed", &[("data_version", Value::from(3_u64))]);
    bodies.push(("data_changed", data));
    bodies
}

/// The schema Schemars derives for `ArtifactChange`, as clients read it.
fn published_schema() -> Result<Value, serde_json::Error> {
    serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<ArtifactChange>(),
    )
}

/// Asserts that the server refuses `body` as a change event, with a data error
/// that names `message`. `case` names the row that failed.
fn assert_refused(case: &str, body: Value, message: &str) {
    let Err(error) = serde_json::from_value::<ArtifactChange>(body) else {
        panic!("{case}: a change event that breaks rule 8 was accepted; expected {message:?}");
    };
    assert!(
        error.is_data(),
        "{case}: expected a data validation error, got {error}"
    );
    assert!(
        error.to_string().contains(message),
        "{case}: expected {message:?}, got {error}"
    );
}

/// A `job_changed` event names its job, and the server reads and writes the
/// identifier back unchanged.
#[test]
fn a_job_change_names_its_job_in_both_directions() -> Result<(), serde_json::Error> {
    let body = change_with("job_changed", &[("job_id", Value::from(JOB_ID))]);
    let change: ArtifactChange = serde_json::from_value(body.clone())?;
    assert_eq!(change.job_id.as_deref(), Some(JOB_ID));
    assert_eq!(
        serde_json::to_value(&change)?,
        body,
        "a job change must be written back with the job_id it was read with"
    );
    Ok(())
}

/// A `job_changed` event without a well-formed `job_id` is refused: absent,
/// `null`, uppercase hex, 31 or 33 digits, or another prefix.
#[test]
fn a_job_change_refuses_a_missing_or_malformed_job_id() {
    let needs_job_id = "a job_changed change needs a job_id";
    let uppercase = format!("job_{}", "A".repeat(32));
    let short = format!("job_{}", "a".repeat(31));
    let long = format!("job_{}", "a".repeat(33));
    let other_prefix = format!("run_{}", "a".repeat(32));
    assert_refused("absent", change_with("job_changed", &[]), needs_job_id);
    for (case, job_id) in [
        ("null", Value::Null),
        ("uppercase hex", Value::from(uppercase)),
        ("31 digits", Value::from(short)),
        ("33 digits", Value::from(long)),
        ("another prefix", Value::from(other_prefix)),
    ] {
        let body = change_with("job_changed", &[("job_id", job_id)]);
        assert_refused(case, body, needs_job_id);
    }
}

/// A `job_changed` event carries no data version, even alongside its job.
#[test]
fn a_job_change_refuses_a_data_version() {
    for (case, data_version) in [
        ("data_version 1", Value::from(1_u64)),
        ("data_version null", Value::Null),
    ] {
        let body = change_with(
            "job_changed",
            &[
                ("job_id", Value::from(JOB_ID)),
                ("data_version", data_version),
            ],
        );
        assert_refused(
            case,
            body,
            "only a data_changed change carries a data_version",
        );
    }
}

/// `job_id` is present exactly for `job_changed` (rule 8). Every other kind
/// refuses the member, valid or `null`, and is written without it.
#[test]
fn other_kinds_refuse_a_job_id_and_are_written_without_one() -> Result<(), serde_json::Error> {
    for (kind, body) in bodies_without_a_job() {
        let change: ArtifactChange = serde_json::from_value(body.clone())?;
        assert_eq!(change.job_id, None, "`{kind}` must carry no job");
        assert_eq!(
            serde_json::to_value(&change)?,
            body,
            "`{kind}` must be written without a job_id member"
        );
        for job_id in [Value::from(JOB_ID), Value::Null] {
            let case = format!("{kind} with job_id {job_id}");
            let mut refused = body.clone();
            refused["job_id"] = job_id;
            assert_refused(&case, refused, "only a job_changed change carries a job_id");
        }
    }
    Ok(())
}

/// The published schema states the job rule: `job_id` is a string of the shape
/// rule 8 gives, required only by `job_changed` and refused by every other kind.
#[test]
fn published_schema_states_the_job_id_rule() -> Result<(), serde_json::Error> {
    let schema = published_schema()?;
    assert_eq!(
        schema["properties"]["job_id"]["pattern"],
        "^job_[0-9a-f]{32}$"
    );
    assert_eq!(
        schema["dependentSchemas"]["change"],
        serde_json::json!({
            "if": {
                "properties": { "change": { "const": "job_changed" } },
                "required": ["change"]
            },
            "then": {
                "properties": { "job_id": { "type": "string" } },
                "required": ["job_id"]
            },
            "else": { "not": { "required": ["job_id"] } }
        })
    );
    let Some(required) = schema["required"].as_array() else {
        panic!("the published schema lists no required fields");
    };
    assert!(
        !required.contains(&Value::from("job_id")),
        "`job_id` must not be required at the root: {required:?}"
    );
    Ok(())
}

/// The rule for `data_version` is unchanged in the published schema: required
/// for `data_changed` and forbidden for every other kind.
#[test]
fn published_schema_keeps_the_data_version_rule() -> Result<(), serde_json::Error> {
    let schema = published_schema()?;
    assert_eq!(
        schema["if"],
        serde_json::json!({
            "properties": { "change": { "const": "data_changed" } },
            "required": ["change"]
        })
    );
    assert_eq!(
        schema["then"],
        serde_json::json!({
            "properties": { "data_version": { "type": "integer" } },
            "required": ["data_version"]
        })
    );
    assert_eq!(
        schema["else"],
        serde_json::json!({ "not": { "required": ["data_version"] } })
    );
    Ok(())
}
