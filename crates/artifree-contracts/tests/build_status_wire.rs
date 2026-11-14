//! Behavioral checks for the head status contract (decision 0058 rule 16 and
//! its R03d split note, slice R03db): the published schema, the fields the
//! server writes for each status, and the values it refuses.

use artifree_contracts::api::{
    BuildStatus, DiagnosticLocation, HeadBuildStatus, JobDiagnostic, JobFailure,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

/// A change made to a valid body before it is read.
type Edit = fn(&mut Value);

/// The schema Schemars derives for `T` under `settings`, as JSON.
fn schema_json<T: JsonSchema>(settings: SchemaSettings) -> Result<Value, serde_json::Error> {
    serde_json::to_value(settings.into_generator().into_root_schema_for::<T>())
}

/// A failed head build with one located and one general diagnostic.
fn failed() -> BuildStatus {
    BuildStatus {
        profile: "react-v1".into(),
        head_revision: 3,
        latest_build_revision: Some(2),
        status: HeadBuildStatus::Failed,
        job_id: Some("job_0123456789abcdef0123456789abcdef".into()),
        failure: Some(JobFailure::BuildFailed),
        diagnostics: vec![
            JobDiagnostic {
                location: Some(DiagnosticLocation {
                    path: "src/main.tsx".into(),
                    line: 1,
                    column: 16,
                }),
                message: "Could not resolve \"left-pad\"".into(),
            },
            JobDiagnostic {
                location: None,
                message: "The build was stopped".into(),
            },
        ],
    }
}

/// `failed` as the server writes it, written out by hand.
fn failed_body() -> Value {
    json!({
        "profile": "react-v1",
        "head_revision": 3,
        "latest_build_revision": 2,
        "status": "failed",
        "job_id": "job_0123456789abcdef0123456789abcdef",
        "failure": "build_failed",
        "diagnostics": [
            { "location": { "path": "src/main.tsx", "line": 1, "column": 16 },
              "message": "Could not resolve \"left-pad\"" },
            { "message": "The build was stopped" }
        ]
    })
}

/// Clients read the published schema, so it must describe what the server
/// writes, and it closes the status and the object as the server does.
#[test]
fn the_published_schema_is_what_the_server_writes_and_is_closed() -> Result<(), serde_json::Error> {
    let published = schema_json::<BuildStatus>(SchemaSettings::draft2020_12())?;
    let written = schema_json::<BuildStatus>(SchemaSettings::draft2020_12().for_serialize())?;
    assert_eq!(published, written);
    assert_eq!(published["additionalProperties"], Value::Bool(false));
    assert_eq!(
        published["required"],
        json!(["profile", "head_revision", "status"])
    );
    let mut statuses: Vec<&str> = published["$defs"]["HeadBuildStatus"]["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            published["$defs"]["HeadBuildStatus"]["enum"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .filter_map(|value| value.get("const").unwrap_or(value).as_str())
        .collect();
    statuses.sort_unstable();
    assert_eq!(statuses, ["built", "failed", "none", "pending"]);
    Ok(())
}

/// The note's fields: a failed status carries its job, failure and
/// diagnostics, and a status without them omits the keys rather than writing
/// `null` or an empty list.
#[test]
fn each_status_is_written_with_the_fields_of_the_note() -> Result<(), serde_json::Error> {
    assert_eq!(serde_json::to_value(failed())?, failed_body());
    assert_eq!(
        serde_json::from_value::<BuildStatus>(failed_body())?,
        failed()
    );

    let none = BuildStatus {
        profile: "react-v1".into(),
        head_revision: 1,
        latest_build_revision: None,
        status: HeadBuildStatus::None,
        job_id: None,
        failure: None,
        diagnostics: Vec::new(),
    };
    let written = serde_json::to_value(&none)?;
    assert_eq!(
        written,
        json!({ "profile": "react-v1", "head_revision": 1, "status": "none" })
    );
    assert_eq!(serde_json::from_value::<BuildStatus>(written)?, none);
    for (status, text) in [
        (HeadBuildStatus::Built, "built"),
        (HeadBuildStatus::Pending, "pending"),
    ] {
        assert_eq!(serde_json::to_value(status)?, Value::from(text));
    }
    Ok(())
}

/// The server refuses a field or a status it does not define.
#[test]
fn unknown_fields_and_statuses_are_refused() {
    let edits: [(Edit, &str); 3] = [
        (
            |body| body["surprise"] = Value::Bool(true),
            "unknown field `surprise`",
        ),
        (
            |body| body["status"] = Value::from("building"),
            "unknown variant `building`",
        ),
        (
            |body| body["diagnostics"][0]["location"]["surprise"] = Value::Bool(true),
            "unknown field `surprise`",
        ),
    ];
    for (edit, message) in edits {
        let mut body = failed_body();
        edit(&mut body);
        let Err(error) = serde_json::from_value::<BuildStatus>(body) else {
            panic!("an invalid status was accepted; expected {message}");
        };
        assert!(
            error.to_string().contains(message),
            "expected {message:?}, got {error}"
        );
    }
}
