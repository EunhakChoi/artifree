use std::fmt::Write as _;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
/// Files with their content.
type Files = Vec<(PackagePath, Vec<u8>)>;
/// A diagnostic as its location and its message.
type Shown<'a> = (Option<(&'a str, u32, u32)>, &'a str);

/// The template's entry document: it loads the bundle with the element of
/// rule 7.
const PAGE: &str = "<!doctype html>\n<div id=\"root\"></div>\n<script type=\"module\" src=\"./app.js\"></script>\n";

fn files(entries: &[(&str, &str)]) -> Result<Files, Box<dyn std::error::Error>> {
    entries
        .iter()
        .map(|(path, content)| Ok((path.parse()?, content.as_bytes().to_vec())))
        .collect()
}

fn bundle(entries: &[(&str, &str)]) -> Result<FileSet, Box<dyn std::error::Error>> {
    Ok(FileSet::new(files(entries)?)?)
}

fn source(page: &str, more: &[(&str, &str)]) -> Result<FilePackage, Box<dyn std::error::Error>> {
    let mut entries = vec![("index.html", page), ("src/main.tsx", "render()")];
    entries.extend_from_slice(more);
    Ok(FilePackage::new(files(&entries)?)?)
}

fn paths(package: &FilePackage) -> Vec<&str> {
    package.files().map(|(path, _)| path.as_str()).collect()
}

#[test]
fn a_build_is_the_bundle_with_the_files_outside_src() -> TestResult {
    let built = compose(
        bundle(&[("app.js", "js"), ("app.css", "css")])?,
        &source(PAGE, &[("logo.svg", "<svg/>"), ("src/app.css", "x")])?,
    )?;
    assert_eq!(
        paths(&built),
        ["app.css", "app.js", "index.html", "logo.svg"]
    );
    assert_eq!(built.file(&"app.js".parse()?), Some(&b"js"[..]));
    assert_eq!(built.file(&PackagePath::entry()), Some(PAGE.as_bytes()));
    let script_only = compose(bundle(&[("app.js", "js")])?, &source(PAGE, &[])?)?;
    assert_eq!(paths(&script_only), ["app.js", "index.html"]);
    // Source under `src/` in any case is never published.
    let any_case = compose(
        bundle(&[("app.js", "js")])?,
        &source(
            PAGE,
            &[("Src/secret.tsx", "x"), ("assets/src/logo.png", "png")],
        )?,
    )?;
    assert_eq!(
        paths(&any_case),
        ["app.js", "assets/src/logo.png", "index.html"]
    );
    // Another directory named `src` is no reference into the source.
    let others = format!(
        "{PAGE}<img src=\"./assets/src/logo.png\"><link href=\"https://cdn.example/x@1/src/a.css\">"
    );
    assert_eq!(
        paths(&compose(
            bundle(&[("app.js", "js")])?,
            &source(&others, &[])?
        )?),
        ["app.js", "index.html"]
    );
    Ok(())
}

#[test]
fn each_broken_rule_rejects_the_composition() -> TestResult {
    let cases: [(FileSet, FilePackage, Rejection); 10] = [
        (
            bundle(&[("app.js", "js")])?,
            source(&format!("{PAGE}<img src=src/logo.svg>"), &[])?,
            Rejection::EntryRefersToSource,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source(&format!("{PAGE}<a href='../src/main.tsx'>"), &[])?,
            Rejection::EntryRefersToSource,
        ),
        (
            bundle(&[("app.js", "js"), ("chunk.js", "js")])?,
            source(PAGE, &[])?,
            Rejection::UnexpectedOutput,
        ),
        (
            bundle(&[("app.css", "css")])?,
            source(PAGE, &[])?,
            Rejection::MissingScript,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source(PAGE, &[("app.js", "mine")])?,
            Rejection::Collision,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source("<p>no script</p>", &[])?,
            Rejection::EntryLoadsNoBundle,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source(&format!("{PAGE}{PAGE}"), &[])?,
            Rejection::EntryLoadsNoBundle,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source(&PAGE.replace("module", "text/javascript"), &[])?,
            Rejection::EntryLoadsNoBundle,
        ),
        (
            bundle(&[("app.js", "js")])?,
            source(&format!("{PAGE}<link href=\"./SRC/app.css\">"), &[])?,
            Rejection::EntryRefersToSource,
        ),
        // `App.js` and `app.js` differ only by case, which the package rules
        // refuse.
        (
            bundle(&[("app.js", "js")])?,
            source(PAGE, &[("App.js", "mine")])?,
            Rejection::Package,
        ),
    ];
    for (built, revision, rejection) in cases {
        assert_eq!(compose(built, &revision), Err(rejection));
    }
    // The element must be markup the browser runs.
    let element = "<script type=\"module\" src=\"./app.js\"></script>";
    for inert in [
        format!("<!doctype html><!-- {element} -->"),
        // A `>` inside the comment ends no comment.
        format!("<!doctype html><!-- a > b {element} -->"),
        format!("<!doctype html><template>{element}</template>"),
        format!("<!doctype html><noscript>{element}</noscript>"),
        format!("<!doctype html><textarea>{element}</textarea>"),
        format!("<!doctype html><div title='{element}'></div>"),
        format!("<!doctype html><script>let a = '{element}"),
        format!("<!doctype html><svg>{element}</svg>"),
    ] {
        assert_eq!(
            compose(bundle(&[("app.js", "js")])?, &source(&inert, &[])?),
            Err(Rejection::EntryLoadsNoBundle),
            "{inert}"
        );
    }
    let closed =
        format!("<!doctype html><title>T</title><!-- a note --><script>run()</script>{element}");
    assert!(compose(bundle(&[("app.js", "js")])?, &source(&closed, &[])?).is_ok());
    let not_text = FilePackage::new(vec![
        (PackagePath::entry(), vec![0xff, 0xfe]),
        ("src/main.tsx".parse()?, b"x".to_vec()),
    ])?;
    assert_eq!(
        compose(bundle(&[("app.js", "js")])?, &not_text),
        Err(Rejection::EntryLoadsNoBundle)
    );
    Ok(())
}

/// Messages in the bundler's form: a header, a location, the quoted excerpt,
/// a note and the closing count.
const TWO_ERRORS: &str = "\u{2718} [ERROR] Could not resolve \"left-pad\"\n\
\n    ../input/src/main.tsx:1:16:\n      1 \u{2502} import pad from \"left-pad\";\n        \u{2575}                 ~~~~~~~~~~\n\
\n  You can mark the path \"left-pad\" as external to exclude it from the bundle.\n\
\n\u{2718} [ERROR] No matching export in \"/input/src/parts.ts\" for import \"Part\"\n\
\n    ../input/src/app.tsx:3:9:\n      3 \u{2502} import { Part } from \"./parts\";\n        \u{2575}          ~~~~\n\
\n\u{25b2} [WARNING] A warning that is no diagnostic\n\
\n    input/src/app.tsx:4:1:\n\
\n2 errors\n";

#[test]
fn the_bundlers_errors_become_diagnostics_with_package_paths() -> TestResult {
    let found = diagnostics(TWO_ERRORS.as_bytes());
    let summary: Vec<Shown<'_>> = found
        .iter()
        .map(|diagnostic| {
            (
                diagnostic
                    .location
                    .as_ref()
                    .map(|at| (at.path.as_str(), at.line, at.column)),
                diagnostic.message.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                Some(("src/main.tsx", 1, 16)),
                "Could not resolve \"left-pad\""
            ),
            (
                Some(("src/app.tsx", 3, 9)),
                "No matching export in \"src/parts.ts\" for import \"Part\""
            ),
        ]
    );
    let absolute =
        diagnostics(b"X [ERROR] Unexpected end of file\n\n    /input/src/main.tsx:7:0:\n");
    assert_eq!(
        absolute
            .first()
            .and_then(|diagnostic| diagnostic.location.as_ref()),
        Some(&SourceLocation {
            path: "src/main.tsx".parse()?,
            line: 7,
            column: 0
        })
    );
    Ok(())
}

#[test]
fn only_a_mount_that_starts_a_path_is_rewritten() {
    let found = diagnostics(
        b"X [ERROR] Could not resolve \"./components/input/field\" from /input/src/a.ts\n\n    input/src/a.ts:1:1:\n",
    );
    assert_eq!(
        found
            .first()
            .map(|diagnostic| (diagnostic.message.as_str(), diagnostic.location.is_none())),
        Some((
            "Could not resolve \"./components/input/field\" from src/a.ts",
            true
        )),
        "a bare input/ lies under the working directory, not the mount"
    );
}

#[test]
fn a_message_without_a_location_in_the_revision_points_at_no_file() {
    let found = diagnostics(
        b"\xe2\x9c\x98 [ERROR] Cannot read the module\n\n    /tools/modules/react/index.js:1:1:\n",
    );
    assert_eq!(found.len(), 1);
    assert_eq!(
        found
            .first()
            .and_then(|diagnostic| diagnostic.location.clone()),
        None
    );
    let bare = diagnostics(b"\xe2\x9c\x98 [ERROR] Something failed\n");
    assert_eq!(
        bare.first().map(|diagnostic| diagnostic.message.as_str()),
        Some("Something failed")
    );
}

#[test]
fn diagnostics_are_bounded_and_cleaned() {
    let many = (0..30).fold(String::new(), |mut text, index| {
        let _ = write!(
            text,
            "\u{2718} [ERROR] error {index}\n\n    ../input/src/main.tsx:{index}:1:\n\n"
        );
        text
    });
    let found = diagnostics(many.as_bytes());
    assert_eq!(found.len(), MAX_DIAGNOSTICS);
    assert_eq!(
        found.last().map(|diagnostic| diagnostic.message.as_str()),
        Some("error 19")
    );

    // Invalid bytes are replaced, a control character becomes a space, and a
    // long message is cut at a character boundary within the bound.
    let mut bytes = "\u{2718} [ERROR] bad\x07byte ".as_bytes().to_vec();
    bytes.push(0xff);
    bytes.extend_from_slice("\u{e9}".repeat(400).as_bytes());
    bytes.push(b'\n');
    let found = diagnostics(&bytes);
    let message = found
        .first()
        .map_or("", |diagnostic| diagnostic.message.as_str());
    assert!(message.starts_with("bad byte \u{fffd}"), "{message}");
    assert!(
        message.len() <= MAX_DIAGNOSTIC_MESSAGE_BYTES,
        "{}",
        message.len()
    );
    assert!(
        message.len() > MAX_DIAGNOSTIC_MESSAGE_BYTES - 4,
        "{}",
        message.len()
    );
}

#[test]
fn a_failure_without_a_readable_message_still_says_so() {
    for stderr in [&b""[..], b"panic: runtime error\n", b"\xff\xfe\n1 error\n"] {
        let found = diagnostics(stderr);
        assert_eq!(found.len(), 1, "{stderr:?}");
        assert_eq!(
            found.first().map(|diagnostic| diagnostic.message.as_str()),
            Some("the bundler failed without a message that could be read")
        );
    }
}

#[test]
fn the_fixed_diagnostics_point_at_no_file() -> TestResult {
    let resolver = resolver_diagnostics(Some(&"src/tsconfig.json".parse()?));
    assert_eq!(resolver.len(), 1);
    assert!(resolver.first().is_some_and(|diagnostic| {
        diagnostic.location.is_none()
            && diagnostic
                .message
                .as_str()
                .starts_with("src/tsconfig.json would change")
    }));
    assert_eq!(resolver_diagnostics(None).len(), 1);
    assert_eq!(filter_kill_diagnostics().len(), 1);
    Ok(())
}
