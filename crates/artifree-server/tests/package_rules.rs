//! The package rules in the build inventory are the domain's, and the shared
//! path fixture that the web's checks also judge matches the domain.
use std::error::Error;

use artifree_domain::package::{self, PackagePath};

#[test]
fn inventory_rules_are_those_the_domain_enforces() -> Result<(), Box<dyn Error>> {
    let rules = artifree_server::describe().package_rules;
    assert_eq!(rules.entry_path, package::ENTRY_PATH);
    assert_eq!(rules.max_files, package::MAX_FILES as u64);
    assert_eq!(rules.max_file_bytes, package::MAX_FILE_BYTES as u64);
    assert_eq!(rules.max_package_bytes, package::MAX_PACKAGE_BYTES as u64);
    assert_eq!(rules.max_path_bytes, package::MAX_PATH_BYTES as u64);
    assert_eq!(rules.max_path_segments, package::MAX_PATH_SEGMENTS as u64);
    assert_eq!(rules.max_segment_bytes, package::MAX_SEGMENT_BYTES as u64);
    for (extension, content_type) in &rules.media_types {
        for name in [
            format!("a.{extension}"),
            format!("a.{}", extension.to_ascii_uppercase()),
        ] {
            let path: PackagePath = name.parse()?;
            assert_eq!(path.media_type().essence(), content_type, "{name}");
        }
    }
    // An extension the rules leave out is refused.
    assert!("a.ts".parse::<PackagePath>().is_err());
    assert!(!rules.media_types.contains_key("ts"));
    Ok(())
}

#[test]
fn the_domain_judges_the_shared_path_fixture() -> Result<(), Box<dyn Error>> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../contracts/fixtures/package-paths.json"
    ))?;
    let cases = fixture["cases"].as_array().ok_or("no cases")?;
    assert!(cases.len() >= 30, "the fixture lost its cases");
    for case in cases {
        let path = case["path"].as_str().ok_or("a case without a path")?;
        let expected = case["error"].as_str();
        let actual = path
            .parse::<PackagePath>()
            .err()
            .map(|error| format!("{error:?}"));
        assert_eq!(actual.as_deref(), expected, "{path:?}");
    }
    Ok(())
}
