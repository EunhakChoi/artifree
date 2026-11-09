//! Behavioral checks for the public contracts' Serde boundary.
use artifree_contracts::ServiceDescription;
use artifree_contracts::api::{
    AppData, Artifact, ArtifactChange, ArtifactList, CommitResponse, CreateArtifactRequest,
    DataMembers, DuplicateArtifactResponse, EditFilesRequest, ErrorBody, ErrorResponse,
    FeedPosition, MAX_DOWNLOAD_NAME, MAX_RUNTIME_ASSET_PART_BYTES, MAX_RUNTIME_SOURCE_PART_BYTES,
    MAX_RUNTIME_SOURCE_WRITE_BASE64, MAX_RUNTIME_SOURCE_WRITE_BYTES, Preview,
    PurgeArtifactResponse, ReadDataMembersQuery, RemoveUnreferencedContentResponse, Revision,
    RevisionList, RuntimeAssetListRequest, RuntimeAssetPart, RuntimeAssetReadRequest,
    RuntimeConnection, RuntimeDownloadOffer, RuntimeGrant, RuntimeSourceList, RuntimeSourcePart,
    RuntimeSourceReadRequest, RuntimeSourceWriteReceipt, RuntimeSourceWriteRequest, RuntimeView,
    SaveRevisionRequest, SourceExportManifest, UnreferencedContent, UpdateArtifactResponse,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;

fn descriptor() -> Value {
    serde_json::json!({
        "name": "Artifree", "version": "0.1.0", "contract_version": "0.1.0",
        "phase": "early-development", "server_framework": null, "async_runtime": null,
        "http_api": false, "mcp_api": false,
        "components": [{ "id": "web", "name": "Web", "layer": "presentation",
            "language": "TypeScript", "path": "apps/web", "status": "scaffold", "summary": "Development shell." }],
        "package_rules": { "entry_path": "index.html", "max_files": 2, "max_file_bytes": 10,
            "max_package_bytes": 20, "max_path_bytes": 40, "max_path_segments": 2,
            "max_segment_bytes": 20, "media_types": { "html": "text/html; charset=utf-8" } }
    })
}

fn assert_rejected(value: Value, message: &str) {
    let Err(error) = serde_json::from_value::<ServiceDescription>(value) else {
        panic!("Invalid wire input was accepted; expected {message}");
    };
    assert!(error.is_data(), "Expected a data validation error: {error}");
    assert!(
        error.to_string().contains(message),
        "Expected {message:?}, got {error}"
    );
}

#[test]
fn round_trip_preserves_explicit_nulls_and_snake_case() -> Result<(), serde_json::Error> {
    let expected = descriptor();
    let parsed: ServiceDescription = serde_json::from_value(expected.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, expected);
    Ok(())
}

#[test]
fn rejects_unknown_top_level_field() {
    let mut invalid = descriptor();
    invalid["surprise"] = Value::Bool(true);
    assert_rejected(invalid, "unknown field `surprise`");
}

#[test]
fn rejects_unknown_nested_field() {
    let mut invalid = descriptor();
    invalid["components"][0]["surprise"] = Value::Bool(true);
    assert_rejected(invalid, "unknown field `surprise`");
}

#[test]
fn rejects_unknown_package_rule() {
    let mut invalid = descriptor();
    invalid["package_rules"]["max_folders"] = Value::from(1);
    assert_rejected(invalid, "unknown field `max_folders`");
}

#[test]
fn rejects_unsupported_layer() {
    let mut invalid = descriptor();
    invalid["components"][0]["layer"] = Value::String("database".into());
    assert_rejected(invalid, "unknown variant `database`");
}

fn save_request() -> Value {
    serde_json::json!({
        "base_revision": 3,
        "changes": [
            { "op": "put", "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" },
            { "op": "delete", "path": "old.css" }
        ]
    })
}

fn save_error(value: Value) -> String {
    match serde_json::from_value::<SaveRevisionRequest>(value) {
        Ok(parsed) => panic!("Invalid change was accepted: {parsed:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn save_request_round_trips_tagged_changes() -> Result<(), serde_json::Error> {
    let expected = save_request();
    let parsed: SaveRevisionRequest = serde_json::from_value(expected.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, expected);
    Ok(())
}

#[test]
fn save_request_rejects_unknown_operations_and_fields() {
    let mut unknown_op = save_request();
    unknown_op["changes"][1]["op"] = Value::String("rename".into());
    assert!(save_error(unknown_op).contains("unknown variant `rename`"));

    let mut extra = save_request();
    extra["changes"][1]["content"] = Value::String("ignored?".into());
    assert!(save_error(extra).contains("unknown field `content`"));

    let mut missing = save_request();
    missing["changes"][0]
        .as_object_mut()
        .map(|change| change.remove("encoding"));
    assert!(save_error(missing).contains("missing field `encoding`"));

    let mut negative = save_request();
    negative["base_revision"] = serde_json::json!(-1);
    assert!(save_error(negative).contains("invalid value"));
}

#[test]
fn create_request_rejects_unknown_encodings_and_fields() {
    let request = |files: Value| serde_json::json!({ "title": "T", "files": files });
    for (files, message) in [
        (
            serde_json::json!([{ "path": "index.html", "encoding": "hex", "content": "" }]),
            "unknown variant `hex`",
        ),
        (
            serde_json::json!([{ "path": "index.html", "encoding": "utf8", "content": "", "mode": 755 }]),
            "unknown field `mode`",
        ),
    ] {
        let Err(error) = serde_json::from_value::<CreateArtifactRequest>(request(files)) else {
            panic!("Invalid upload accepted; expected {message}");
        };
        assert!(
            error.to_string().contains(message),
            "Expected {message:?}, got {error}"
        );
    }
}

/// Whether the schema Schemars derives for reading `T`, the published one,
/// differs from the one it derives for writing it.
fn published_unlike_written<T: JsonSchema>() -> bool {
    let schema = |settings: SchemaSettings| settings.into_generator().into_root_schema_for::<T>();
    schema(SchemaSettings::draft2020_12()) != schema(SchemaSettings::draft2020_12().for_serialize())
}

/// Clients read responses with the published schema, where an `Option`
/// would be optional. Each response must be published as the server writes
/// it: every field it always writes required, with its type.
#[test]
fn response_schemas_describe_what_the_server_writes() {
    let differing: Vec<&str> = [
        ("Artifact", published_unlike_written::<Artifact>()),
        ("ArtifactList", published_unlike_written::<ArtifactList>()),
        (
            "CommitResponse",
            published_unlike_written::<CommitResponse>(),
        ),
        (
            "DuplicateArtifactResponse",
            published_unlike_written::<DuplicateArtifactResponse>(),
        ),
        (
            "UpdateArtifactResponse",
            published_unlike_written::<UpdateArtifactResponse>(),
        ),
        (
            "PurgeArtifactResponse",
            published_unlike_written::<PurgeArtifactResponse>(),
        ),
        (
            "RemoveUnreferencedContentResponse",
            published_unlike_written::<RemoveUnreferencedContentResponse>(),
        ),
        (
            "UnreferencedContent",
            published_unlike_written::<UnreferencedContent>(),
        ),
        ("Revision", published_unlike_written::<Revision>()),
        ("RevisionList", published_unlike_written::<RevisionList>()),
        ("Preview", published_unlike_written::<Preview>()),
        (
            "RuntimeConnection",
            published_unlike_written::<RuntimeConnection>(),
        ),
        ("RuntimeGrant", published_unlike_written::<RuntimeGrant>()),
        ("RuntimeView", published_unlike_written::<RuntimeView>()),
        (
            "RuntimeSourceList",
            published_unlike_written::<RuntimeSourceList>(),
        ),
        (
            "RuntimeSourcePart",
            published_unlike_written::<RuntimeSourcePart>(),
        ),
        (
            "RuntimeAssetPart",
            published_unlike_written::<RuntimeAssetPart>(),
        ),
        (
            "RuntimeDownloadOffer",
            published_unlike_written::<RuntimeDownloadOffer>(),
        ),
        (
            "RuntimeSourceWriteReceipt",
            published_unlike_written::<RuntimeSourceWriteReceipt>(),
        ),
        ("ErrorResponse", published_unlike_written::<ErrorResponse>()),
        (
            "ArtifactChange",
            published_unlike_written::<ArtifactChange>(),
        ),
        ("FeedPosition", published_unlike_written::<FeedPosition>()),
        (
            "SourceExportManifest",
            published_unlike_written::<SourceExportManifest>(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect();
    assert_eq!(differing, Vec::<&str>::new());
}

/// A value that was stored as `null` is not an absent value (decision 0031):
/// both read back through serde as they were written.
#[test]
fn app_data_tells_a_stored_null_from_no_value() {
    let read = |text: &str| serde_json::from_str::<AppData>(text);
    let head = r#""artifact_id":"art_1","path":"a","data_version":3"#;
    let stored_null = read(&format!(r#"{{{head},"exists":true,"value":null}}"#))
        .unwrap_or_else(|error| panic!("a stored null must read: {error}"));
    assert_eq!(
        stored_null
            .value
            .as_ref()
            .map(artifree_contracts::api::JsonValue::get),
        Some("null")
    );
    let absent = read(&format!(r#"{{{head},"exists":false}}"#))
        .unwrap_or_else(|error| panic!("an absent value must read: {error}"));
    assert_eq!(absent.value, None);
    // And each writes back as it was read.
    let text = |data: &AppData| serde_json::to_string(data).unwrap_or_default();
    assert!(
        text(&stored_null).contains(r#""value":null"#),
        "{}",
        text(&stored_null)
    );
    assert!(!text(&absent).contains("value"), "{}", text(&absent));
}

/// The bounds a frame's parsers apply are the ones the schema publishes
/// (decision 0032): a part is at most `MAX_RUNTIME_SOURCE_PART_BYTES`, a
/// download name at most `MAX_DOWNLOAD_NAME` characters.
#[test]
fn runtime_source_bounds_are_the_published_ones() {
    let schema = |settings: SchemaSettings| {
        let generator = settings.into_generator();
        (
            serde_json::to_value(
                generator
                    .clone()
                    .into_root_schema_for::<RuntimeSourceReadRequest>(),
            ),
            serde_json::to_value(generator.into_root_schema_for::<RuntimeDownloadOffer>()),
        )
    };
    let (read, offer) = schema(SchemaSettings::draft2020_12());
    let read = read.unwrap_or_else(|error| panic!("read schema: {error}"));
    let offer = offer.unwrap_or_else(|error| panic!("offer schema: {error}"));
    assert_eq!(
        read["properties"]["length"]["maximum"],
        MAX_RUNTIME_SOURCE_PART_BYTES
    );
    assert_eq!(read["properties"]["length"]["minimum"], 1);
    assert_eq!(
        offer["properties"]["file_name"]["maxLength"],
        MAX_DOWNLOAD_NAME
    );
}

/// A frame write is bounded by the schema's text length (decision 0033): the
/// base64 bound is exactly the encoding of the byte bound.
#[test]
fn runtime_source_write_bounds_are_the_published_ones() {
    let schema = serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<RuntimeSourceWriteRequest>(),
    )
    .unwrap_or_else(|error| panic!("write schema: {error}"));
    assert_eq!(
        schema["properties"]["content"]["maxLength"],
        MAX_RUNTIME_SOURCE_WRITE_BASE64
    );
    // The retry identity is 8 to 36 characters; the server refuses 7 and 37.
    assert_eq!(schema["properties"]["retry_id"]["minLength"], 8);
    assert_eq!(schema["properties"]["retry_id"]["maxLength"], 36);
    // 1 MiB is 349,525 whole groups of three bytes and one byte over, which
    // base64 writes as 349,526 groups of four characters with padding.
    assert_eq!(MAX_RUNTIME_SOURCE_WRITE_BYTES, 1_048_576);
    assert_eq!(MAX_RUNTIME_SOURCE_WRITE_BASE64, 349_526 * 4);
}

/// The bounds the asset reader applies are the ones the schema publishes
/// (decision 0045 rule 12): a part is at least one byte and at most
/// `MAX_RUNTIME_ASSET_PART_BYTES`, which is 256 KiB. The published range refuses
/// a length of 0 and a length one byte past the largest part.
#[test]
fn runtime_asset_part_bounds_are_the_published_ones() -> Result<(), serde_json::Error> {
    let schema = serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<RuntimeAssetReadRequest>(),
    )?;
    let length = &schema["properties"]["length"];
    assert_eq!(MAX_RUNTIME_ASSET_PART_BYTES, 262_144);
    assert_eq!(length["minimum"], 1);
    assert_eq!(length["maximum"], MAX_RUNTIME_ASSET_PART_BYTES);
    // The published bounds are inclusive, so the ends decide: 0 and one byte past
    // the largest part fail validation, and the two ends are admitted.
    let minimum = length["minimum"].as_u64().unwrap_or_default();
    let maximum = length["maximum"].as_u64().unwrap_or_default();
    for (value, admitted) in [(0, false), (1, true), (262_144, true), (262_145, false)] {
        assert_eq!(
            (minimum..=maximum).contains(&value),
            admitted,
            "a length of {value} against the published bounds"
        );
    }
    Ok(())
}

/// Asserts that `T` reads `body`, and that the same body with a field nobody
/// defines is refused, naming that field.
fn assert_refuses_unknown_field<T>(body: Value)
where
    T: serde::de::DeserializeOwned + std::fmt::Debug,
{
    let _valid: T = serde_json::from_value(body.clone())
        .unwrap_or_else(|error| panic!("a body without unknown fields was refused: {error}"));
    let mut surprised = body;
    surprised["surprise"] = Value::Bool(true);
    match serde_json::from_value::<T>(surprised) {
        Ok(parsed) => panic!("a body with an unknown field was accepted: {parsed:?}"),
        Err(error) => assert!(
            error.to_string().contains("unknown field `surprise`"),
            "the unknown field must be named, got {error}"
        ),
    }
}

/// The asset requests and the part refuse a field they do not define, rather
/// than ignoring it, as the other wire bodies do.
#[test]
fn runtime_asset_bodies_refuse_unknown_fields() {
    assert_refuses_unknown_field::<RuntimeAssetListRequest>(serde_json::json!({
        "initialization": "secret"
    }));
    assert_refuses_unknown_field::<RuntimeAssetReadRequest>(serde_json::json!({
        "initialization": "secret", "asset_id": "a".repeat(64), "offset": 0, "length": 1
    }));
    assert_refuses_unknown_field::<RuntimeAssetPart>(serde_json::json!({
        "asset_id": "a".repeat(64), "content_type": "text/plain", "offset": 8,
        "next_offset": null, "size": 10, "sha256": "0".repeat(64), "content": "BAU="
    }));
}

/// One part of a 10-byte asset, as a runtime view receives it.
fn asset_part(offset: u64, next_offset: Option<u64>, content: &str) -> RuntimeAssetPart {
    RuntimeAssetPart {
        asset_id: "a".repeat(64),
        content_type: "text/plain".into(),
        offset,
        next_offset,
        size: 10,
        sha256: "0".repeat(64),
        content: content.into(),
    }
}

/// A part writes `next_offset` as a number while more of the asset follows, and
/// as an explicit `null` on the part that reaches its end (decision 0045 rule
/// 12). The end part reads back as it was written.
#[test]
fn runtime_asset_part_writes_next_offset_as_a_number_or_null() -> Result<(), serde_json::Error> {
    let middle = asset_part(0, Some(4), "AAECAw==");
    assert_eq!(
        serde_json::to_value(&middle)?,
        serde_json::json!({
            "asset_id": "a".repeat(64), "content_type": "text/plain", "offset": 0,
            "next_offset": 4, "size": 10, "sha256": "0".repeat(64), "content": "AAECAw=="
        })
    );
    let last = asset_part(8, None, "BAU=");
    assert_eq!(
        serde_json::to_value(&last)?,
        serde_json::json!({
            "asset_id": "a".repeat(64), "content_type": "text/plain", "offset": 8,
            "next_offset": null, "size": 10, "sha256": "0".repeat(64), "content": "BAU="
        })
    );
    assert_eq!(
        serde_json::from_value::<RuntimeAssetPart>(serde_json::to_value(&last)?)?,
        last
    );
    Ok(())
}

fn edit_request() -> Value {
    serde_json::json!({
        "base_revision": 3,
        "edits": [
            {
                "op": "put", "path": "index.html", "encoding": "utf8",
                "content": "<p>x</p>", "expect": "0".repeat(64)
            },
            { "op": "put", "path": "new.css", "encoding": "utf8", "content": "", "expect": "absent" },
            { "op": "delete", "path": "old.css", "expect": "f".repeat(64) }
        ]
    })
}

/// An edit carries the condition on its own file (decision 0035): there is no
/// default, and the shape is the same as a save's change plus `expect`.
#[test]
fn edit_request_round_trips_and_every_edit_states_its_condition() -> Result<(), serde_json::Error> {
    let expected = edit_request();
    let parsed: EditFilesRequest = serde_json::from_value(expected.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, expected);

    for index in 0..3 {
        let mut request = edit_request();
        request["edits"][index]
            .as_object_mut()
            .map(|edit| edit.remove("expect"));
        let refused = serde_json::from_value::<EditFilesRequest>(request);
        assert!(
            refused.is_err_and(|error| error.to_string().contains("missing field `expect`")),
            "edit {index} without a condition"
        );
    }
    let mut renamed = edit_request();
    renamed["edits"][2]["op"] = Value::String("rename".into());
    assert!(serde_json::from_value::<EditFilesRequest>(renamed).is_err());
    Ok(())
}

/// The numbers the prose and the schema state are the server's (decision
/// 0035): at most 512 edits, 32 listed conflicts, and conditions that are
/// `absent` or 64 lowercase hexadecimal digits.
#[test]
fn edit_bounds_and_conditions_are_the_published_ones() {
    let schema = |value: Result<Value, serde_json::Error>| {
        value.unwrap_or_else(|error| panic!("schema: {error}"))
    };
    let generator = SchemaSettings::draft2020_12().into_generator();
    let request = schema(serde_json::to_value(
        generator.clone().into_root_schema_for::<EditFilesRequest>(),
    ));
    let error = schema(serde_json::to_value(
        generator.into_root_schema_for::<ErrorBody>(),
    ));
    assert_eq!(request["properties"]["edits"]["maxItems"], 512);
    assert_eq!(error["properties"]["conflicting_files"]["maxItems"], 32);
    let put = &request["$defs"]["FileEdit"]["oneOf"][0]["properties"]["expect"];
    let delete = &request["$defs"]["FileEdit"]["oneOf"][1]["properties"]["expect"];
    assert_eq!(put["pattern"], "^(absent|[0-9a-f]{64})$");
    assert_eq!(delete["pattern"], "^[0-9a-f]{64}$");
}

/// A listing page omits the token when nothing follows, and the query reads
/// the whole numbers its schema bounds (decision 0040, rules 8 and 9).
#[test]
fn data_members_omit_an_absent_token_and_the_query_bounds_are_published() {
    let page = DataMembers {
        artifact_id: "art_1".into(),
        path: "a".into(),
        data_version: 2,
        exists: true,
        names: vec!["x".into()],
        next_page_token: None,
    };
    let text = serde_json::to_string(&page).unwrap_or_default();
    assert!(!text.contains("next_page_token"), "{text}");
    let with_token = DataMembers {
        next_page_token: Some("t".into()),
        ..page
    };
    assert!(
        serde_json::to_string(&with_token)
            .unwrap_or_default()
            .ends_with(r#""next_page_token":"t"}"#)
    );

    let query = |text: &str| serde_json::from_str::<ReadDataMembersQuery>(text);
    assert!(query(r#"{"limit":100,"at_data_version":9007199254740991}"#).is_ok());
    assert!(query(r#"{"at_data_version":9007199254740992}"#).is_err());
    assert!(query(r#"{"unknown":1}"#).is_err());
    let schema = serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<ReadDataMembersQuery>(),
    )
    .unwrap_or_default();
    assert_eq!(schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(schema["properties"]["limit"]["maximum"], 100);
    assert_eq!(
        schema["properties"]["at_data_version"]["maximum"],
        9_007_199_254_740_991_u64
    );
}

/// A change event names a data version of at least 1: a space nobody wrote is
/// at version 0 and no write leaves it there, so the published schema must not
/// admit 0. Other kinds serialize without the field (decision 0040).
#[test]
fn a_change_event_publishes_its_data_version_bounds() -> Result<(), serde_json::Error> {
    let schema = serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<ArtifactChange>(),
    )?;
    assert_eq!(schema["properties"]["data_version"]["minimum"], 1);
    assert_eq!(
        schema["properties"]["data_version"]["maximum"],
        9_007_199_254_740_991_u64
    );
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        !required.contains(&"data_version"),
        "the field is optional: {required:?}"
    );

    let revised: ArtifactChange = serde_json::from_value(serde_json::json!({
        "cursor": "c.1", "artifact_id": "art_0", "change": "revised",
        "revision": 1, "metadata_revision": 1, "committed_at_ms": 1
    }))?;
    assert!(
        serde_json::to_value(&revised)?
            .get("data_version")
            .is_none(),
        "a kind without a data version serializes without the field"
    );
    Ok(())
}

fn change_with(kind: &str, extra: &serde_json::Value) -> Result<ArtifactChange, serde_json::Error> {
    let mut body = serde_json::json!({
        "cursor": "c.1", "artifact_id": "art_0", "change": kind,
        "revision": 1, "metadata_revision": 1, "committed_at_ms": 1
    });
    if let (Some(fields), Some(more)) = (body.as_object_mut(), extra.as_object()) {
        fields.extend(
            more.iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
    }
    serde_json::from_value(body)
}

/// A change event states a data version exactly when it is a data change
/// (decision 0040 rule 12), and a body that breaks the rule is not read at
/// all: neither a data change without a version, nor another kind with one.
#[test]
fn a_change_event_reads_a_data_version_only_for_a_data_change() -> Result<(), serde_json::Error> {
    let data = change_with("data_changed", &serde_json::json!({ "data_version": 3 }))?;
    assert_eq!(data.data_version, Some(3));
    let largest = change_with(
        "data_changed",
        &serde_json::json!({ "data_version": 9_007_199_254_740_991_u64 }),
    )?;
    assert_eq!(largest.data_version, Some(9_007_199_254_740_991));
    assert_eq!(
        change_with("revised", &serde_json::json!({}))?.data_version,
        None
    );

    for refused in [
        change_with("data_changed", &serde_json::json!({})),
        change_with("data_changed", &serde_json::json!({ "data_version": null })),
        change_with("data_changed", &serde_json::json!({ "data_version": 0 })),
        change_with(
            "data_changed",
            &serde_json::json!({ "data_version": 9_007_199_254_740_992_u64 }),
        ),
        change_with("revised", &serde_json::json!({ "data_version": 1 })),
        change_with("purged", &serde_json::json!({ "data_version": 1 })),
        change_with("revised", &serde_json::json!({ "data_version": null })),
    ] {
        assert!(refused.is_err(), "a change breaking the rule was read");
    }
    Ok(())
}

/// The published schema says what deserializing does: a data version is
/// required for `data_changed` and forbidden otherwise, and the schema keeps
/// its name and body so generated types do not change.
#[test]
fn a_change_event_publishes_the_kind_dependent_data_version() -> Result<(), serde_json::Error> {
    let schema = serde_json::to_value(
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<ArtifactChange>(),
    )?;
    assert_eq!(
        schema["if"],
        serde_json::json!({
            "properties": { "change": { "const": "data_changed" } },
            "required": ["change"]
        })
    );
    assert_eq!(
        schema["then"]["required"],
        serde_json::json!(["data_version"])
    );
    assert_eq!(
        schema["else"],
        serde_json::json!({ "not": { "required": ["data_version"] } })
    );
    assert_eq!(schema["title"], "ArtifactChange");
    assert_eq!(schema["additionalProperties"], false);
    Ok(())
}
