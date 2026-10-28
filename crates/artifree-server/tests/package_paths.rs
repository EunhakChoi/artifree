//! The shared path cases of `contracts/fixtures/package-paths.json`, judged
//! by the domain's parser; the web's copy of the rules checks the same cases
//! (decision 0017).

use std::error::Error;
use std::str::FromStr;

use artifree_domain::package::PackagePath;

#[test]
fn every_shared_path_case_is_judged_as_the_fixture_says() -> Result<(), Box<dyn Error>> {
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/fixtures/package-paths.json"
    ))?;
    let fixture: serde_json::Value = serde_json::from_str(&fixture)?;
    let cases = fixture["cases"].as_array().ok_or("cases")?;
    assert!(cases.len() > 30, "the fixture lost its cases");
    for case in cases {
        let path = case["path"].as_str().ok_or("path")?;
        let judged = PackagePath::from_str(path)
            .err()
            .map(|error| format!("{error:?}"));
        assert_eq!(judged.as_deref(), case["error"].as_str(), "{path:?}");
    }
    Ok(())
}
