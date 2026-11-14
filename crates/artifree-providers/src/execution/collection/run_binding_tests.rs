//! Behavioral tests of the run binding of the output manifest (decision 0057 rule 17; decision
//! 0056, amendment of 2026-11-13, input and output). A manifest that binds its run takes the
//! `artifree-output-manifest-v2` tag and seven run lines after the `input` line, one field per
//! line, with `none` for an absent generation. A manifest without a run keeps the v1 text and its
//! digest.
//!
//! The expected texts are written out by hand from the rule, not computed by the code under test.
//! The file line's digest is the SHA-256 of the one-file package's bytes, taken from the literal
//! that the collection tests use.

use std::error::Error;

use artifree_domain::package::{FilePackage, PackagePath};
use artifree_ports::execution::BoundRun;
use sha2::{Digest, Sha256};

use super::{ManifestBinding, OutputManifest};

/// The result of a test body: a setup failure ends the test with its error.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The entry document of the one-file package. Its eight bytes appear in every expected text.
const INDEX_HTML: &[u8] = b"<p>i</p>";

/// The binding's lines in their fixed order, without a version tag. The digests are the literal
/// values that `manifest_of` sets.
const BINDING_LINES: &str = concat!(
    "profile\tartifree-fixture-inspection-v1\n",
    "profile-manifest\t0101010101010101010101010101010101010101010101010101010101010101\n",
    "tool-root\tabababababababababababababababababababababababababababababababab\n",
    "filter\tcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\n",
    "input\t0202020202020202020202020202020202020202020202020202020202020202\n",
);

/// The file line of `index.html`: its length and the SHA-256 of `INDEX_HTML`.
const INDEX_LINE: &str =
    "file\tindex.html\t8\t804a2503db0467dbff55deb3ac76d5cc3115702299af82510697647d0b1260d4\n";

/// The attempt identity of the bound run: 32 lowercase hexadecimal digits.
const ATTEMPT: &str = "00112233445566778899aabbccddeeff";

/// The run that the bound manifests carry, with `generation` as its generation.
fn bound_run(generation: Option<u64>) -> BoundRun {
    BoundRun {
        actor: "owner".to_owned(),
        generation,
        artifact: "a-1".to_owned(),
        revision: 1,
        job: "j-1".to_owned(),
        fence: 2,
        attempt: ATTEMPT.to_owned(),
    }
}

/// The manifest of the one-file package under a binding that carries `run`.
fn manifest_of(run: Option<BoundRun>) -> TestResult<OutputManifest> {
    let path: PackagePath = "index.html".parse()?;
    let package = FilePackage::new(vec![(path, INDEX_HTML.to_vec())])?;
    let binding = ManifestBinding {
        profile: "artifree-fixture-inspection-v1",
        profile_manifest_sha256: [1; 32],
        tool_root_sha256: "ab".repeat(32),
        filter_sha256: "cd".repeat(32),
        input_digest: [2; 32],
        run,
    };
    Ok(OutputManifest::of(&package, &binding))
}

/// The canonical text of the one-file package under a bound run whose generation line is
/// `generation_line`, written out field by field.
fn bound_text(generation_line: &str) -> String {
    format!(
        "artifree-output-manifest-v2\n\
         {BINDING_LINES}\
         actor\towner\n\
         {generation_line}\
         artifact\ta-1\n\
         revision\t1\n\
         job\tj-1\n\
         fence\t2\n\
         attempt\t{ATTEMPT}\n\
         {INDEX_LINE}"
    )
}

#[test]
fn a_bound_manifest_has_the_v2_tag_and_its_run_lines() -> TestResult {
    let manifest = manifest_of(Some(bound_run(Some(3))))?;
    let text = manifest.canonical_text();
    let first_line = text.lines().next();
    assert!(
        text.starts_with("artifree-output-manifest-v2\n"),
        "a manifest that binds its run starts with the v2 tag, got first line {first_line:?}"
    );
    let expected = bound_text("generation\t3\n");
    assert_eq!(
        text, expected,
        "the run lines follow the input line, one per field, and precede the file lines"
    );
    let expected_digest: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
    assert_eq!(
        manifest.digest(),
        expected_digest,
        "the digest is the SHA-256 of the canonical text with the run lines"
    );
    Ok(())
}

#[test]
fn an_absent_generation_is_written_none() -> TestResult {
    let manifest = manifest_of(Some(bound_run(None)))?;
    assert_eq!(
        manifest.canonical_text(),
        bound_text("generation\tnone\n"),
        "a run without a generation writes the line `generation` with the value `none`"
    );
    Ok(())
}

#[test]
fn without_a_run_the_text_is_r02afs() -> TestResult {
    let bound = manifest_of(Some(bound_run(Some(3))))?;
    let unbound = manifest_of(None)?;
    let text = unbound.canonical_text();
    let expected = format!("artifree-output-manifest-v1\n{BINDING_LINES}{INDEX_LINE}");
    assert_eq!(
        text, expected,
        "a manifest without a run keeps the v1 tag, the binding lines and the file lines"
    );
    assert!(
        !text.contains("actor\t"),
        "a manifest without a run writes no run lines, got {text:?}"
    );
    assert_ne!(
        unbound.digest(),
        bound.digest(),
        "the v1 digest of a manifest without a run differs from the v2 digest of the bound one"
    );
    Ok(())
}
