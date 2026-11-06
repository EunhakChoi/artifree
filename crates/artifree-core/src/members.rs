//! What a listing of the members of an object needs besides the data itself
//! (decision 0040, rules 7 to 11): the page size, the position a page
//! continues from, and the request and answer of
//! [`ArtifactService::data_members`](crate::artifacts::ArtifactService).

use artifree_domain::AppDataVersion;
use artifree_domain::data::{DataPath, MemberName};
use artifree_domain::identity::{ArtifactId, InvalidValue};
use sha2::{Digest, Sha256};

/// Bytes of the tag that ties a [`MemberToken`] to one artifact and path.
pub const TAG_BYTES: usize = 16;
/// Fewest bytes of an encoded [`MemberToken`]: the tag and a one-byte name.
pub const MIN_TOKEN_BYTES: usize = TAG_BYTES + 1;
/// Most bytes of an encoded [`MemberToken`]: the tag and a 64-byte name.
pub const MAX_TOKEN_BYTES: usize = TAG_BYTES + artifree_domain::data::MAX_SEGMENT_BYTES;

/// The number of names in one page, from 1 to [`MemberLimit::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemberLimit(usize);

impl MemberLimit {
    /// The largest page.
    pub const MAX: Self = Self(100);

    /// The number of names.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for MemberLimit {
    /// 50 names.
    fn default() -> Self {
        Self(50)
    }
}

impl TryFrom<u64> for MemberLimit {
    type Error = InvalidValue;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        usize::try_from(value)
            .ok()
            .filter(|size| (1..=Self::MAX.0).contains(size))
            .map(Self)
            .ok_or(InvalidValue {
                what: "member page size",
                rule: "must be 1-100",
            })
    }
}

/// The position after which a page of member names continues: the last name
/// of the page before, tied to the artifact and path it was listed for. It
/// names a position, not a permission; every page checks access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberToken {
    tag: [u8; TAG_BYTES],
    name: MemberName,
}

impl MemberToken {
    /// The position after `last`, a member of the object at `path` of
    /// `artifact`.
    #[must_use]
    pub fn new(artifact: &ArtifactId, path: &DataPath, last: MemberName) -> Self {
        Self {
            tag: tag(artifact, path),
            name: last,
        }
    }

    /// Reads a token from its bytes: a 16-byte tag, then a name that a path
    /// can address (1 to 64 bytes). Whether the tag belongs to a request is
    /// [`Self::matches`].
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&bytes.len()) {
            return None;
        }
        let (tag, name) = bytes.split_at(TAG_BYTES);
        Some(Self {
            tag: tag.try_into().ok()?,
            name: std::str::from_utf8(name).ok()?.parse().ok()?,
        })
    }

    /// The bytes of the token: 17 to 80 of them.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(TAG_BYTES + self.name.as_str().len());
        bytes.extend_from_slice(&self.tag);
        bytes.extend_from_slice(self.name.as_str().as_bytes());
        bytes
    }

    /// Whether the token was issued for this artifact and path.
    #[must_use]
    pub fn matches(&self, artifact: &ArtifactId, path: &DataPath) -> bool {
        self.tag == tag(artifact, path)
    }

    /// The last name of the page the token continues from.
    #[must_use]
    pub fn name(&self) -> &MemberName {
        &self.name
    }
}

/// The first 16 bytes of SHA-256 over the artifact identifier, a zero byte,
/// and the canonical path.
fn tag(artifact: &ArtifactId, path: &DataPath) -> [u8; TAG_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(artifact.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(path.to_string().as_bytes());
    let digest = hasher.finalize();
    let mut tag = [0; TAG_BYTES];
    tag.copy_from_slice(&digest[..TAG_BYTES]);
    tag
}

/// What to list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberQuery {
    /// The object whose members are listed; the root lists the top level.
    pub path: DataPath,
    /// Continues after this position.
    pub after: Option<MemberToken>,
    /// The most names to list.
    pub limit: MemberLimit,
    /// Answer only if the space is at this version.
    pub at_data_version: Option<AppDataVersion>,
}

/// One page of member names, from one read of the space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataMembersPage {
    /// The version of the space that was read: the version of the names.
    pub version: AppDataVersion,
    /// Whether an object is at the path. False for nothing there or a path
    /// through a value that is not an object, with no names.
    pub exists: bool,
    /// The names, ascending by UTF-8 bytes.
    pub names: Vec<MemberName>,
    /// Continues the listing; present only when more names follow.
    pub next: Option<MemberToken>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(seed: u8) -> ArtifactId {
        ArtifactId::from_random([seed; 16])
    }

    fn path(text: &str) -> DataPath {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn name(text: &str) -> MemberName {
        text.parse()
            .unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    #[test]
    fn a_token_is_its_tag_then_the_name_in_clear() {
        let token = MemberToken::new(&artifact(1), &path("a/b"), name("last-one"));
        let bytes = token.to_bytes();
        assert_eq!(bytes.len(), TAG_BYTES + "last-one".len());
        assert_eq!(&bytes[TAG_BYTES..], b"last-one");
        assert_eq!(MemberToken::from_bytes(&bytes), Some(token));
    }

    #[test]
    fn persisted_ascii_token_bytes_survive_the_unicode_grammar_expansion() {
        // Independently recorded SHA-256(artifact || NUL || "a/b")[..16]
        // followed by the ASCII name. This is the unchanged pre-0046 layout.
        let old = [
            0x50, 0xca, 0x75, 0x81, 0xa6, 0xda, 0x88, 0x31, 0x56, 0x42, 0x9e, 0xa2, 0x3e, 0x35,
            0xc9, 0xd7, 0x6c, 0x61, 0x73, 0x74, 0x2d, 0x6f, 0x6e, 0x65,
        ];
        let token = MemberToken::from_bytes(&old).unwrap_or_else(|| panic!("old ASCII token"));
        assert!(token.matches(&artifact(1), &path("a/b")));
        assert_eq!(token.name().as_str(), "last-one");
        assert_eq!(
            MemberToken::new(&artifact(1), &path("a/b"), name("last-one")).to_bytes(),
            old
        );
        for text in ["é", "e\u{301}", "한글", "𝄞"] {
            let token = MemberToken::new(&artifact(1), &path(text), name(text));
            assert_eq!(MemberToken::from_bytes(&token.to_bytes()), Some(token));
        }
        let composed = MemberToken::new(&artifact(1), &path("é"), name("x"));
        assert!(!composed.matches(&artifact(1), &path("e\u{301}")));
    }

    #[test]
    fn the_tag_is_the_first_16_bytes_of_sha256_of_artifact_zero_path() {
        let id = artifact(1);
        let mut hasher = Sha256::new();
        hasher.update(id.as_str());
        hasher.update([0]);
        hasher.update("a/b");
        let digest = hasher.finalize();
        let bytes = MemberToken::new(&id, &path("a/b"), name("x")).to_bytes();
        assert_eq!(&bytes[..TAG_BYTES], &digest[..TAG_BYTES]);
    }

    #[test]
    fn a_token_matches_only_its_artifact_and_path() {
        let token = MemberToken::new(&artifact(1), &path("a/b"), name("x"));
        assert!(token.matches(&artifact(1), &path("a/b")));
        assert!(!token.matches(&artifact(2), &path("a/b")), "other artifact");
        assert!(!token.matches(&artifact(1), &path("a")), "other path");
        assert!(!token.matches(&artifact(1), &path("")), "root");
        // The zero byte keeps the end of the identifier apart from the path.
        assert_ne!(
            MemberToken::new(&artifact(1), &path(""), name("x")),
            MemberToken::new(&artifact(1), &path("a"), name("x"))
        );
    }

    #[test]
    fn the_layout_is_bounded_to_17_through_80_bytes_of_valid_names() {
        let tag = [7_u8; TAG_BYTES];
        let with = |name: &[u8]| [tag.as_slice(), name].concat();
        assert!(MemberToken::from_bytes(&with(b"x")).is_some());
        assert!(MemberToken::from_bytes(&with(&[b'x'; 64])).is_some());
        for (case, bytes) in [
            ("empty", Vec::new()),
            ("tag only", tag.to_vec()),
            ("short tag", b"x".to_vec()),
            ("65-byte name", with(&[b'x'; 65])),
            ("slash", with(b"a/b")),
            ("space", with(b"a b")),
            ("default ignorable", with("\u{fe0f}".as_bytes())),
            ("invalid UTF-8", with(&[0xff])),
        ] {
            assert_eq!(MemberToken::from_bytes(&bytes), None, "{case}");
        }
    }

    #[test]
    fn a_page_size_is_one_to_one_hundred() {
        assert_eq!(MemberLimit::default().get(), 50);
        for refused in [0, 101, u64::MAX] {
            assert!(MemberLimit::try_from(refused).is_err(), "{refused}");
        }
        assert_eq!(MemberLimit::try_from(1).map(MemberLimit::get), Ok(1));
        assert_eq!(MemberLimit::try_from(100).map(MemberLimit::get), Ok(100));
    }
}
