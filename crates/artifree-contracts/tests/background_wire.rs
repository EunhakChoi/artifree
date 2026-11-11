//! Behavioral checks for the background status contract (decision 0055, rule
//! 5): the published schema, the values the server writes, and the values it
//! refuses.

use artifree_contracts::api::{
    BackgroundStatus, WorkerExit, WorkerExitOutcome, WorkerMode, WorkerState, WorkerStatus,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde_json::Value;

/// A change made to a valid body before it is read.
type Edit = fn(&mut Value);

/// The schema Schemars derives for `T` under `settings`, as JSON.
fn schema_json<T: JsonSchema>(settings: SchemaSettings) -> Result<Value, serde_json::Error> {
    serde_json::to_value(settings.into_generator().into_root_schema_for::<T>())
}

/// The status of a supervised worker that failed once and is being restarted.
fn sample_status() -> BackgroundStatus {
    BackgroundStatus {
        worker: WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Restarting,
            restarts: 1,
            last_exit: Some(WorkerExit {
                at_ms: 1_700_000_000_000,
                outcome: WorkerExitOutcome::Failed,
            }),
            dropped_lines: 2,
        },
    }
}

/// `sample_status` as the server writes it, written out by hand.
fn sample_body() -> Value {
    serde_json::json!({
        "worker": {
            "mode": "supervised",
            "state": "restarting",
            "restarts": 1,
            "last_exit": { "at_ms": 1_700_000_000_000_i64, "outcome": "failed" },
            "dropped_lines": 2
        }
    })
}

/// The values a closed enum definition allows, sorted, whether the schema
/// lists them under `enum` or as `const` alternatives.
fn allowed_values(definition: &Value) -> Vec<&str> {
    let listed = definition
        .get("enum")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    let alternatives = ["oneOf", "anyOf"]
        .into_iter()
        .filter_map(|keyword| definition.get(keyword).and_then(Value::as_array))
        .flatten()
        .filter_map(|alternative| alternative.get("const"));
    let mut values: Vec<&str> = listed
        .chain(alternatives)
        .filter_map(Value::as_str)
        .collect();
    values.sort_unstable();
    values
}

/// Asserts that each value is written as its expected JSON string.
fn assert_written_as<T: Serialize + std::fmt::Debug>(
    cases: &[(T, &str)],
) -> Result<(), serde_json::Error> {
    for (value, text) in cases {
        assert_eq!(
            serde_json::to_value(value)?,
            Value::from(*text),
            "{value:?} must be written as {text:?}"
        );
    }
    Ok(())
}

/// Asserts that the server refuses `body` with a data error naming `message`.
fn assert_refused(body: Value, message: &str) {
    let Err(error) = serde_json::from_value::<BackgroundStatus>(body) else {
        panic!("an invalid background status was accepted; expected {message}");
    };
    assert!(error.is_data(), "expected a data validation error: {error}");
    assert!(
        error.to_string().contains(message),
        "expected {message:?}, got {error}"
    );
}

/// Clients read the published schema, so it must describe what the server
/// writes: the same fields, each required as the server always writes it.
#[test]
fn published_schema_matches_what_the_server_writes() -> Result<(), serde_json::Error> {
    let published = schema_json::<BackgroundStatus>(SchemaSettings::draft2020_12())?;
    let written = schema_json::<BackgroundStatus>(SchemaSettings::draft2020_12().for_serialize())?;
    assert_eq!(
        published, written,
        "the published schema of `BackgroundStatus` must describe what the server writes"
    );
    Ok(())
}

/// The published schema is closed where the server is: each enum lists exactly
/// the values of rule 5, and each object refuses a field it does not define.
#[test]
fn published_schema_closes_enums_and_objects() -> Result<(), serde_json::Error> {
    let published = schema_json::<BackgroundStatus>(SchemaSettings::draft2020_12())?;
    let definitions = &published["$defs"];
    assert_eq!(
        allowed_values(&definitions["WorkerMode"]),
        ["none", "supervised"]
    );
    assert_eq!(
        allowed_values(&definitions["WorkerState"]),
        ["restarting", "running", "stopped"]
    );
    assert_eq!(
        allowed_values(&definitions["WorkerExitOutcome"]),
        ["clean", "failed", "not_started"]
    );
    let objects = [
        ("BackgroundStatus", &published),
        ("WorkerStatus", &definitions["WorkerStatus"]),
        ("WorkerExit", &definitions["WorkerExit"]),
    ];
    for (name, object) in objects {
        assert_eq!(
            object["additionalProperties"],
            Value::Bool(false),
            "`{name}` must refuse a field it does not define"
        );
    }
    Ok(())
}

/// Each closed value is written as its `snake_case` text in rule 5.
#[test]
fn enum_values_are_written_as_rule_five_text() -> Result<(), serde_json::Error> {
    assert_written_as(&[
        (WorkerMode::Supervised, "supervised"),
        (WorkerMode::None, "none"),
    ])?;
    assert_written_as(&[
        (WorkerState::Running, "running"),
        (WorkerState::Restarting, "restarting"),
        (WorkerState::Stopped, "stopped"),
    ])?;
    assert_written_as(&[
        (WorkerExitOutcome::Clean, "clean"),
        (WorkerExitOutcome::Failed, "failed"),
        (WorkerExitOutcome::NotStarted, "not_started"),
    ])?;
    Ok(())
}

/// Rule 5 names the fields the status is written with. An absent `last_exit`
/// is an omitted key, not `null`, and a status without it reads back unchanged.
#[test]
fn writes_rule_five_fields_and_omits_an_absent_last_exit() -> Result<(), serde_json::Error> {
    assert_eq!(
        serde_json::to_value(sample_status())?,
        sample_body(),
        "the status must be written with the field names of rule 5"
    );

    let first_run = BackgroundStatus {
        worker: WorkerStatus {
            mode: WorkerMode::Supervised,
            state: WorkerState::Running,
            restarts: 0,
            last_exit: None,
            dropped_lines: 0,
        },
    };
    let written = serde_json::to_value(&first_run)?;
    assert_eq!(
        written,
        serde_json::json!({
            "worker": {
                "mode": "supervised",
                "state": "running",
                "restarts": 0,
                "dropped_lines": 0
            }
        }),
        "an absent `last_exit` must be an omitted key, not null"
    );
    assert_eq!(
        serde_json::from_value::<BackgroundStatus>(written)?,
        first_run
    );
    Ok(())
}

/// Rule 5's values are closed: the server refuses a field or a value it does
/// not define, in the worker, in its mode or state, and in the last exit.
#[test]
fn refuses_unknown_fields_and_values() -> Result<(), serde_json::Error> {
    assert_eq!(
        serde_json::from_value::<BackgroundStatus>(sample_body())?,
        sample_status(),
        "the sample body must be valid, so each refusal below comes from its edit alone"
    );
    let edits: [(Edit, &str); 5] = [
        (
            |body| body["worker"]["surprise"] = Value::Bool(true),
            "unknown field `surprise`",
        ),
        (
            |body| body["worker"]["last_exit"]["surprise"] = Value::Bool(true),
            "unknown field `surprise`",
        ),
        (
            |body| body["worker"]["last_exit"]["outcome"] = Value::from("crashed"),
            "unknown variant `crashed`",
        ),
        (
            |body| body["worker"]["mode"] = Value::from("paused"),
            "unknown variant `paused`",
        ),
        (
            |body| body["worker"]["state"] = Value::from("paused"),
            "unknown variant `paused`",
        ),
    ];
    for (edit, message) in edits {
        let mut body = sample_body();
        edit(&mut body);
        assert_refused(body, message);
    }
    Ok(())
}
