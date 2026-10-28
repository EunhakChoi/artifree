//! The domain judges every case of the shared path fixture as it states,
//! so the web's copy of the rules can be checked against the same cases.
use std::error::Error;
use std::str::FromStr;

use artifree_domain::package::PackagePath;

#[test]
fn the_domain_judges_every_shared_path_case() -> Result<(), Box<dyn Error>> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../contracts/fixtures/package-paths.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("cases is not a list")?;
    assert!(cases.len() >= 40, "The fixture lost cases: {}", cases.len());
    for case in cases {
        let path = case["path"].as_str().ok_or("a path is not a string")?;
        let actual = PackagePath::from_str(path)
            .err()
            .map(|error| format!("{error:?}"));
        assert_eq!(
            actual.as_deref(),
            case["error"].as_str(),
            "{path:?} was judged differently"
        );
    }
    Ok(())
}
