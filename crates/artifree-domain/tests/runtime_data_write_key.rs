//! Behavioral tests for the key of a runtime frame data write (`data.set`,
//! `data.update` and `data.remove`, decision 0053, rule 9).
//!
//! The key is built from the artifact, the view, the view's generation and the
//! frame's retry identity. No caller can parse it back from text, and it stays
//! distinct from the source and asset write keys built for the same parts.

use artifree_domain::identity::{
    ArtifactId, InvalidValue, RUNTIME_ASSET_WRITE_PREFIX, RUNTIME_DATA_WRITE_PREFIX,
    RUNTIME_WRITE_PREFIX, RequestKey,
};

/// One kind of runtime frame write key: its name for failure messages, the
/// prefix its keys start with, and the builder that makes them.
#[derive(Debug, Clone, Copy)]
struct Kind {
    name: &'static str,
    prefix: &'static str,
    build: fn(&ArtifactId, &str, u64, &str) -> Result<RequestKey, InvalidValue>,
}

/// The three runtime frame write kinds. They are built by the same rules and
/// differ only in their prefix.
const KINDS: [Kind; 3] = [
    Kind {
        name: "source write",
        prefix: RUNTIME_WRITE_PREFIX,
        build: RequestKey::runtime_write,
    },
    Kind {
        name: "asset write",
        prefix: RUNTIME_ASSET_WRITE_PREFIX,
        build: RequestKey::runtime_asset_write,
    },
    Kind {
        name: "data write",
        prefix: RUNTIME_DATA_WRITE_PREFIX,
        build: RequestKey::runtime_data_write,
    },
];

/// The artifact that every test builds keys for.
fn sample_artifact() -> ArtifactId {
    ArtifactId::from_random([0xab; 16])
}

/// The text of the key that `kind` builds for the given parts.
fn key_text(
    kind: &Kind,
    artifact: &ArtifactId,
    view: &str,
    generation: u64,
    retry_id: &str,
) -> Result<String, InvalidValue> {
    let key = (kind.build)(artifact, view, generation, retry_id)?;
    Ok(key.as_str().to_owned())
}

/// Whether the public key grammar refuses `text` as a request key. The check
/// names the refusal, so a failure in some other place does not pass for it.
fn refused_by_key_grammar(text: &str) -> bool {
    matches!(
        text.parse::<RequestKey>(),
        Err(InvalidValue {
            what: "request key",
            ..
        })
    )
}

/// Asserts that `left` and `right` build different keys for the same parts,
/// and that the two keys are equal once their prefixes are removed.
fn assert_differ_only_in_prefix(
    left: &Kind,
    right: &Kind,
    artifact: &ArtifactId,
) -> Result<(), InvalidValue> {
    let left_text = key_text(left, artifact, "view1", 3, "retry-0001")?;
    let right_text = key_text(right, artifact, "view1", 3, "retry-0001")?;
    assert!(
        left_text.starts_with(left.prefix),
        "the {} key {left_text:?} lacks its prefix",
        left.name
    );
    assert!(
        right_text.starts_with(right.prefix),
        "the {} key {right_text:?} lacks its prefix",
        right.name
    );
    assert_ne!(
        left_text, right_text,
        "a {} and a {} share a key",
        left.name, right.name
    );
    assert_eq!(
        left_text.strip_prefix(left.prefix),
        right_text.strip_prefix(right.prefix),
        "a {} and a {} differ beyond their prefixes",
        left.name,
        right.name
    );
    Ok(())
}

#[test]
fn a_data_write_key_is_the_data_prefix_then_the_scoped_parts() -> Result<(), InvalidValue> {
    let key = RequestKey::runtime_data_write(&sample_artifact(), "view1", 3, "retry-0001")?;
    // Decision 0053 rule 9 names the prefix. The parts follow it joined by ':',
    // as the source and asset write keys join theirs.
    assert_eq!(
        key.as_str(),
        format!("rtd~art_{}:view1:3:retry-0001", "ab".repeat(16))
    );
    assert!(
        key.as_str().starts_with(RUNTIME_DATA_WRITE_PREFIX),
        "{key:?} does not start with the data prefix"
    );
    Ok(())
}

#[test]
fn a_data_write_key_is_outside_the_public_key_grammar() -> Result<(), InvalidValue> {
    let key = RequestKey::runtime_data_write(&sample_artifact(), "v", 1, "retry-0001")?;
    let text = key.as_str();
    // The key is refused when echoed back in any letter case, so no caller can
    // replay or forge it by sending its text.
    for echoed in [text.to_owned(), text.to_uppercase(), text.to_lowercase()] {
        assert!(
            refused_by_key_grammar(&echoed),
            "{echoed:?} is not refused by the public key grammar"
        );
    }
    // Text spelled with the data prefix is refused as well.
    for typed in ["rtd~request-1", "rtd~anything-at-all"] {
        assert!(
            refused_by_key_grammar(typed),
            "{typed:?} is not refused by the public key grammar"
        );
    }
    Ok(())
}

#[test]
fn the_three_write_keys_for_the_same_parts_differ_only_by_prefix() -> Result<(), InvalidValue> {
    let [source, asset, data] = KINDS;
    let artifact = sample_artifact();
    assert_differ_only_in_prefix(&source, &asset, &artifact)?;
    assert_differ_only_in_prefix(&source, &data, &artifact)?;
    assert_differ_only_in_prefix(&asset, &data, &artifact)?;
    Ok(())
}

#[test]
fn the_longest_key_of_each_runtime_write_kind_is_127_characters() -> Result<(), InvalidValue> {
    let artifact = sample_artifact();
    for kind in KINDS {
        // "rtd~" (4), "art_" and 32 hex digits (36), a 32-character view, a
        // 16-digit generation, a 36-character retry identity and three ':'
        // separators: 4 + 36 + 32 + 16 + 36 + 3 = 127. The key grammar accepts
        // at most 128 characters.
        let longest = (kind.build)(
            &artifact,
            &"f".repeat(32),
            9_007_199_254_740_991,
            &"r".repeat(36),
        )?;
        assert_eq!(
            longest.as_str().len(),
            127,
            "the longest {} key is {:?}",
            kind.name,
            longest.as_str()
        );
    }
    Ok(())
}

#[test]
fn a_data_write_key_refuses_what_an_asset_write_key_refuses() {
    let artifact = sample_artifact();
    // Each row holds a view, generation and retry identity that a key must
    // refuse, with the part the refusal has to name. A retry identity of 7 or
    // 37 characters is outside the 8 to 36 range, and a ':' would blur the
    // parts of the key.
    for (view, generation, retry_id, refused_part) in [
        ("", 1, "retry-0001", "runtime view"),
        ("vi:ew", 1, "retry-0001", "runtime view"),
        (&"v".repeat(33), 1, "retry-0001", "runtime view"),
        ("view", 0, "retry-0001", "runtime generation"),
        (
            "view",
            9_007_199_254_740_992,
            "retry-0001",
            "runtime generation",
        ),
        ("view", 1, &"r".repeat(7), "retry id"),
        ("view", 1, &"r".repeat(37), "retry id"),
        ("view", 1, "retry:0001", "retry id"),
    ] {
        let data = RequestKey::runtime_data_write(&artifact, view, generation, retry_id);
        let asset = RequestKey::runtime_asset_write(&artifact, view, generation, retry_id);
        assert_eq!(
            data, asset,
            "data and asset writes refuse {view:?} {generation} {retry_id:?} differently"
        );
        assert_eq!(
            data.err().map(|error| error.what),
            Some(refused_part),
            "{view:?} {generation} {retry_id:?} is refused for the wrong part"
        );
    }
}

#[test]
fn a_data_write_key_accepts_the_boundary_parts() -> Result<(), InvalidValue> {
    let artifact = sample_artifact();
    // The shortest view and retry identity, then the longest view, the largest
    // safe generation and the longest retry identity.
    for (view, generation, retry_id) in [
        ("v", 1, &"r".repeat(8)),
        (&"f".repeat(32), 9_007_199_254_740_991, &"r".repeat(36)),
    ] {
        let key = RequestKey::runtime_data_write(&artifact, view, generation, retry_id)?;
        assert!(
            key.as_str().starts_with(RUNTIME_DATA_WRITE_PREFIX),
            "{key:?} does not start with the data prefix"
        );
    }
    Ok(())
}
