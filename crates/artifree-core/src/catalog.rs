//! The built-in type catalog (decision 0059 rules 1 to 4): first-party types
//! compiled into the server, each with append-only numbered releases.
//!
//! A release is the files of one `types/<id>/<release>/` directory: the
//! descriptor `type.txt`, the guide `guide.md`, the starter files under
//! `starter/` and the reference files under `references/`. [`Catalog::builtin`]
//! reads the embedded copies of those files; a test checks that every file
//! in the directories is embedded, and `npm run check` checks the directories
//! against their lock.

use std::fmt;

use artifree_domain::ArtifactKind;
use artifree_domain::artifact_type::{ArtifactType, TypeId, TypeRelease};
use artifree_domain::identity::InvalidValue;
use artifree_domain::job::ProfileName;
use artifree_domain::package::{FilePackage, PackagePath};

use crate::jobs::require_build_profile;

/// The longest description, in characters (decision 0059 rule 2).
pub const MAX_DESCRIPTION_CHARS: usize = 280;
/// The longest guide and the longest reference file, in bytes (rule 2).
pub const MAX_GUIDE_BYTES: usize = 32 * 1024;

/// One release of a type: what its descriptor says and the files it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The type's id.
    pub id: TypeId,
    /// The release number.
    pub release: TypeRelease,
    /// A short name for people.
    pub title: String,
    /// What the type is for, at most [`MAX_DESCRIPTION_CHARS`] characters.
    pub description: String,
    /// The kind every instance gets.
    pub kind: ArtifactKind,
    /// The paths every revision of an instance must hold, in path order.
    pub required: Vec<PackagePath>,
    /// The files of a new instance whose creator sends none.
    pub starter: FilePackage,
    /// The build profile that builds an instance, or `None`.
    pub build_profile: Option<ProfileName>,
    /// How to fill the type, in Markdown, at most [`MAX_GUIDE_BYTES`].
    pub guide: String,
    /// Further Markdown files by path, in path order, each at most
    /// [`MAX_GUIDE_BYTES`].
    pub references: Vec<(PackagePath, String)>,
}

impl Release {
    /// The type and release an instance of this release records.
    #[must_use]
    pub fn artifact_type(&self) -> ArtifactType {
        ArtifactType {
            id: self.id.clone(),
            release: self.release,
        }
    }

    /// Whether this server can build an instance (decision 0059 rule 4): the
    /// release names a build profile and `build_profiles`, the profiles this
    /// build knows, admit it as a public one.
    #[must_use]
    pub fn build_available(&self, build_profiles: &[ProfileName]) -> bool {
        self.build_profile
            .as_ref()
            .is_some_and(|profile| require_build_profile(profile, build_profiles).is_ok())
    }

    /// The required paths that `package` lacks, in path order.
    #[must_use]
    pub fn missing(&self, package: &FilePackage) -> Vec<PackagePath> {
        self.required
            .iter()
            .filter(|path| package.file(path).is_none())
            .cloned()
            .collect()
    }

    /// The reference file at `path`, or `None`.
    #[must_use]
    pub fn reference(&self, path: &PackagePath) -> Option<&str> {
        self.references
            .iter()
            .find(|(reference, _)| reference == path)
            .map(|(_, text)| text.as_str())
    }
}

/// Why a catalog or one of its releases is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError {
    /// The release directory, such as `react/1`, or `catalog` for the whole.
    pub at: String,
    /// What is wrong.
    pub reason: String,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "type catalog {}: {}", self.at, self.reason)
    }
}

impl std::error::Error for CatalogError {}

/// The fixed set of types a server offers, in a fixed order, each with all
/// of its releases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Each type's releases, oldest first; the last one is current.
    types: Vec<Vec<Release>>,
}

impl Catalog {
    /// A catalog with no types.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A catalog of `types` in the given order, each a type's releases from
    /// release 1 up without a gap.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] for a type with no release, releases of different ids
    /// or not numbered 1, 2, ..., an id used twice, a release whose kind is
    /// not `app` or whose starter lacks a required path, or a text over its
    /// limit.
    pub fn new(types: Vec<Vec<Release>>) -> Result<Self, CatalogError> {
        let catalog_error = |reason: String| CatalogError {
            at: "catalog".to_owned(),
            reason,
        };
        let mut seen: Vec<&TypeId> = Vec::new();
        for releases in &types {
            let Some(first) = releases.first() else {
                return Err(catalog_error("a type has no release".to_owned()));
            };
            if seen.contains(&&first.id) {
                return Err(catalog_error(format!("the id {} is used twice", first.id)));
            }
            seen.push(&first.id);
            for (number, release) in (1_u32..).zip(releases) {
                if release.id != first.id || release.release.get() != number {
                    return Err(catalog_error(format!(
                        "the releases of {} are not numbered 1, 2, ... in order",
                        first.id
                    )));
                }
                check_release(release)?;
            }
        }
        Ok(Self { types })
    }

    /// The types compiled into this server, from the repository's `types/`
    /// directory.
    ///
    /// # Errors
    ///
    /// [`CatalogError`] when an embedded release cannot be read; a test
    /// proves that every embedded release can.
    pub fn builtin() -> Result<Self, CatalogError> {
        let mut types = Vec::new();
        for releases in builtin::TYPES {
            let mut parsed = Vec::new();
            for (at, files) in *releases {
                parsed.push(parse_release(at, files)?);
            }
            types.push(parsed);
        }
        Self::new(types)
    }

    /// The current release of each type, in catalog order.
    pub fn current_releases(&self) -> impl Iterator<Item = &Release> {
        self.types.iter().filter_map(|releases| releases.last())
    }

    /// The current release of the type `id`, or `None` for an unknown type.
    #[must_use]
    pub fn current(&self, id: &TypeId) -> Option<&Release> {
        self.releases(id).and_then(<[Release]>::last)
    }

    /// The release `release` of the type `id`, or `None` when this server does
    /// not know it.
    #[must_use]
    pub fn release(&self, id: &TypeId, release: TypeRelease) -> Option<&Release> {
        self.releases(id)?
            .iter()
            .find(|known| known.release == release)
    }

    fn releases(&self, id: &TypeId) -> Option<&[Release]> {
        self.types
            .iter()
            .find(|releases| releases.first().is_some_and(|first| &first.id == id))
            .map(Vec::as_slice)
    }
}

/// The checks of one release that its files cannot make by themselves.
fn check_release(release: &Release) -> Result<(), CatalogError> {
    let error = |reason: String| CatalogError {
        at: format!("{}/{}", release.id, release.release),
        reason,
    };
    // Every create path makes an app today; the other kinds have no package
    // model to fill yet.
    if release.kind != ArtifactKind::App {
        return Err(error(format!(
            "kind {} cannot be created",
            release.kind.as_str()
        )));
    }
    if release.title.is_empty() || release.title.contains(['\n', '\r']) {
        return Err(error("the title must be one non-empty line".to_owned()));
    }
    if release.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(error(format!(
            "the description is longer than {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    if release.guide.len() > MAX_GUIDE_BYTES {
        return Err(error("the guide is larger than 32 KiB".to_owned()));
    }
    if let Some((path, _)) = release
        .references
        .iter()
        .find(|(_, text)| text.len() > MAX_GUIDE_BYTES)
    {
        return Err(error(format!("the reference {path} is larger than 32 KiB")));
    }
    if let Some(path) = release.missing(&release.starter).first() {
        return Err(error(format!("the starter lacks the required {path}")));
    }
    Ok(())
}

/// Reads one release from its files, given as paths relative to the release
/// directory `at` (such as `react/1`).
fn parse_release(at: &str, files: &[(&str, &[u8])]) -> Result<Release, CatalogError> {
    let error = |reason: String| CatalogError {
        at: at.to_owned(),
        reason,
    };
    let text = |path: &str, bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| error(format!("{path} is not UTF-8")))
    };
    let mut descriptor = None;
    let mut guide = None;
    let mut starter = Vec::new();
    let mut references = Vec::new();
    for (path, bytes) in files {
        if *path == "type.txt" {
            descriptor = Some(text(path, bytes)?);
        } else if *path == "guide.md" {
            guide = Some(text(path, bytes)?);
        } else if let Some(inner) = path.strip_prefix("starter/") {
            let parsed = inner
                .parse::<PackagePath>()
                .map_err(|reason| error(format!("{path}: {reason}")))?;
            starter.push((parsed, bytes.to_vec()));
        } else if let Some(inner) = path.strip_prefix("references/") {
            let parsed = inner
                .parse::<PackagePath>()
                .map_err(|reason| error(format!("{path}: {reason}")))?;
            references.push((parsed, text(path, bytes)?));
        } else {
            return Err(error(format!("{path} is not a release file")));
        }
    }
    let descriptor = descriptor.ok_or_else(|| error("type.txt is missing".to_owned()))?;
    let guide = guide.ok_or_else(|| error("guide.md is missing".to_owned()))?;
    let fields = Descriptor::parse(&descriptor).map_err(error)?;
    if format!("{}/{}", fields.id, fields.release) != at {
        return Err(error(format!(
            "the descriptor names {}/{}",
            fields.id, fields.release
        )));
    }
    let starter =
        FilePackage::new(starter).map_err(|reason| error(format!("starter: {reason}")))?;
    references.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(Release {
        id: fields.id,
        release: fields.release,
        title: fields.title,
        description: fields.description,
        kind: fields.kind,
        required: fields.required,
        starter,
        build_profile: fields.build_profile,
        guide,
        references,
    })
}

/// The fields of a `type.txt`.
struct Descriptor {
    id: TypeId,
    release: TypeRelease,
    title: String,
    description: String,
    kind: ArtifactKind,
    required: Vec<PackagePath>,
    build_profile: Option<ProfileName>,
}

impl Descriptor {
    /// Parses `key: value` lines. Blank lines and lines starting with `#`
    /// are skipped; `required` may repeat and every other key appears at
    /// most once; `build_profile` is optional, the rest are required.
    fn parse(text: &str) -> Result<Self, String> {
        fn invalid(key: &'static str) -> impl Fn(InvalidValue) -> String {
            move |reason| format!("type.txt: {key}: {reason}")
        }
        let mut single: Vec<(&str, &str)> = Vec::new();
        let mut required = Vec::new();
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(": ")
                .ok_or_else(|| format!("type.txt: the line {line:?} is not \"key: value\""))?;
            match key {
                "required" => required.push(
                    value
                        .parse::<PackagePath>()
                        .map_err(|reason| format!("type.txt: required {value}: {reason}"))?,
                ),
                "id" | "release" | "title" | "description" | "kind" | "build_profile" => {
                    if single.iter().any(|(seen, _)| *seen == key) {
                        return Err(format!("type.txt: {key} appears twice"));
                    }
                    single.push((key, value));
                }
                other => return Err(format!("type.txt: unknown key {other}")),
            }
        }
        let get = |key: &str| {
            single
                .iter()
                .find(|(seen, _)| *seen == key)
                .map(|(_, value)| *value)
        };
        let need = |key: &str| get(key).ok_or_else(|| format!("type.txt: {key} is missing"));
        required.sort();
        if required.windows(2).any(|pair| pair.first() == pair.get(1)) {
            return Err("type.txt: a required path appears twice".to_owned());
        }
        Ok(Self {
            id: need("id")?.parse().map_err(invalid("id"))?,
            release: need("release")?.parse().map_err(invalid("release"))?,
            title: need("title")?.to_owned(),
            description: need("description")?.to_owned(),
            kind: need("kind")?.parse().map_err(invalid("kind"))?,
            required,
            build_profile: get("build_profile")
                .map(str::parse)
                .transpose()
                .map_err(invalid("build_profile"))?,
        })
    }
}

/// The embedded release files. Every file of every release directory is
/// listed; the test `every_release_file_is_embedded` compares the list with
/// the directories.
mod builtin {
    /// The files of one release: its directory and each file's path in it.
    pub(super) type ReleaseFiles = (&'static str, &'static [(&'static str, &'static [u8])]);

    macro_rules! release_file {
        ($dir:literal, $path:literal) => {
            (
                $path,
                include_bytes!(concat!("../../../types/", $dir, "/", $path)),
            )
        };
    }

    const REACT_1: ReleaseFiles = (
        "react/1",
        &[
            release_file!("react/1", "type.txt"),
            release_file!("react/1", "guide.md"),
            release_file!("react/1", "starter/index.html"),
            release_file!("react/1", "starter/src/main.tsx"),
            release_file!("react/1", "starter/src/app.css"),
            release_file!("react/1", "references/layout-example.md"),
        ],
    );

    /// Each type's releases, oldest first, in catalog order.
    pub(super) const TYPES: &[&[ReleaseFiles]] = &[&[REACT_1]];
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::error::Error;
    use std::fs;
    use std::path::{Path, PathBuf};

    use artifree_domain::job::ProfileName;
    use artifree_domain::package::PackagePath;

    use super::{Catalog, builtin, parse_release};

    fn types_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../types")
    }

    /// Every file under `dir`, as `/`-separated paths relative to `root`.
    fn files_under(
        root: &Path,
        dir: &Path,
        found: &mut BTreeSet<String>,
    ) -> Result<(), Box<dyn Error>> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                files_under(root, &path, found)?;
            } else {
                let relative = path.strip_prefix(root)?;
                let parts: Vec<String> = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                found.insert(parts.join("/"));
            }
        }
        Ok(())
    }

    #[test]
    fn every_release_file_is_embedded() -> Result<(), Box<dyn Error>> {
        let root = types_dir();
        let mut on_disk = BTreeSet::new();
        files_under(&root, &root, &mut on_disk)?;
        // The lock of rule 3 sits beside the type directories.
        on_disk.remove("releases.lock.json");
        let embedded: BTreeSet<String> = builtin::TYPES
            .iter()
            .flat_map(|releases| releases.iter())
            .flat_map(|(at, files)| files.iter().map(move |(path, _)| format!("{at}/{path}")))
            .collect();
        assert_eq!(embedded, on_disk);
        Ok(())
    }

    #[test]
    fn the_builtin_catalog_holds_react_1() -> Result<(), Box<dyn Error>> {
        let catalog = Catalog::builtin()?;
        let ids: Vec<String> = catalog
            .current_releases()
            .map(|release| format!("{}-{}", release.id, release.release))
            .collect();
        assert_eq!(ids, ["react-1"]);
        let react = catalog.current(&"react".parse()?).ok_or("no react type")?;
        let required: Vec<&str> = react.required.iter().map(PackagePath::as_str).collect();
        assert_eq!(required, ["index.html", "src/main.tsx"]);
        let starter: Vec<&str> = react
            .starter
            .files()
            .map(|(path, _)| path.as_str())
            .collect();
        assert_eq!(starter, ["index.html", "src/app.css", "src/main.tsx"]);
        assert_eq!(
            react.build_profile.as_ref().map(ProfileName::as_str),
            Some("react-v1")
        );
        assert!(react.reference(&"layout-example.md".parse()?).is_some());
        assert!(react.build_available(&["react-v1".parse()?]));
        assert!(!react.build_available(&[]));
        Ok(())
    }

    #[test]
    fn a_release_is_refused_for_a_bad_descriptor() {
        let good: &[(&str, &[u8])] = &[
            (
                "type.txt",
                b"id: a\nrelease: 1\ntitle: A\ndescription: d\nkind: app\nrequired: index.html\n",
            ),
            ("guide.md", b"# A\n"),
            ("starter/index.html", b"<p>a</p>"),
        ];
        assert!(parse_release("a/1", good).is_ok());
        // The directory must be the one the descriptor names.
        assert!(parse_release("a/2", good).is_err());
        let cases: &[&[u8]] = &[
            b"id: a\nrelease: 1\ntitle: A\ndescription: d\nkind: app\nrequired: src/main.tsx\n",
            b"id: a\nrelease: 1\ntitle: A\ndescription: d\nkind: document\n",
            b"id: a\nrelease: 1\nrelease: 1\ntitle: A\ndescription: d\nkind: app\n",
            b"id: a\nrelease: 1\ntitle: A\ndescription: d\nkind: app\ncolor: red\n",
            b"id: a\nrelease: 1\ntitle: A\nkind: app\n",
        ];
        for descriptor in cases {
            let files: &[(&str, &[u8])] = &[
                ("type.txt", descriptor),
                ("guide.md", b"# A\n"),
                ("starter/index.html", b"<p>a</p>"),
            ];
            let parsed =
                parse_release("a/1", files).and_then(|release| Catalog::new(vec![vec![release]]));
            assert!(parsed.is_err(), "{}", String::from_utf8_lossy(descriptor));
        }
    }
}
