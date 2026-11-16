//! Who may reach an artifact other than its owner (decision 0060).
//!
//! An artifact has one owner for its whole life. The owner may make another
//! registered user a member with one [`Role`]. This module names the roles,
//! how a user stands towards one artifact, and what each stand allows (0060
//! rule 3), so the application core and the store, which reads the role again
//! where a write commits (rule 5), apply one table.

use std::fmt;
use std::str::FromStr;

use crate::data::DataValue;

/// The role of a member of an artifact (decision 0060 rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Reads the artifact and changes its source and metadata.
    Editor,
    /// Reads the artifact.
    Viewer,
}

impl Role {
    /// Stable lowercase name used in storage and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Viewer => "viewer",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A role name that is not `editor` or `viewer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownRole;

impl fmt::Display for UnknownRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a role is editor or viewer")
    }
}

impl std::error::Error for UnknownRole {}

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "editor" => Ok(Self::Editor),
            "viewer" => Ok(Self::Viewer),
            _ => Err(UnknownRole),
        }
    }
}

/// How a user stands towards one artifact they can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// The user who created the artifact.
    Owner,
    /// A member, with the role the owner gave.
    Member(Role),
}

/// What an operation needs of its caller (decision 0060 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Need {
    /// Reading the artifact, duplicating it, or using one's own data and
    /// asset space: any member.
    Read,
    /// Committing source or metadata, or enqueueing a job: owner or editor.
    Change,
    /// Moving it to or from the trash, purging it, or changing who has
    /// access: the owner.
    Own,
}

impl Need {
    /// What a metadata change needs: the owner, when it names the trash state
    /// (a move to or from the trash), and a change otherwise.
    #[must_use]
    pub fn of_metadata(names_trash: bool) -> Self {
        if names_trash { Self::Own } else { Self::Change }
    }
}

impl Access {
    /// Whether this stand allows what an operation `need`s.
    #[must_use]
    pub fn permits(self, need: Need) -> bool {
        match (self, need) {
            (Self::Owner, _)
            | (Self::Member(_), Need::Read)
            | (Self::Member(Role::Editor), Need::Change) => true,
            (Self::Member(_), Need::Own) | (Self::Member(Role::Viewer), Need::Change) => false,
        }
    }
}

/// Which data or asset space of an artifact a request names (decision 0060
/// rule 10). Nothing names the shared space by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Space {
    /// The caller's own space, which only the caller reads and writes.
    #[default]
    Own,
    /// The artifact's declared shared space.
    Shared,
}

impl Space {
    /// Stable lowercase name used in storage, fingerprints and wire contracts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Shared => "shared",
        }
    }
}

/// The largest runtime manifest an artifact may hold, in bytes (decision 0020).
pub const MAX_RUNTIME_MANIFEST_BYTES: usize = 16 * 1024;

/// Who may write an artifact's declared shared space (decision 0060 rule 10).
/// The owner and every member may read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SharedWriters {
    /// The owner and every editor.
    Editors,
    /// The owner and every member.
    Members,
}

/// A runtime manifest whose `shared` declaration cannot be read: text that is
/// not one JSON object, or a `shared` member that is not `"editors"` or
/// `"members"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedDeclaration;

impl fmt::Display for MalformedDeclaration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a shared space is declared as \"editors\" or \"members\"")
    }
}

impl std::error::Error for MalformedDeclaration {}

impl SharedWriters {
    /// Stable lowercase name, as a manifest declares it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editors => "editors",
            Self::Members => "members",
        }
    }

    /// What a write of the shared space needs of its caller.
    #[must_use]
    pub fn need(self) -> Need {
        match self {
            Self::Editors => Need::Change,
            Self::Members => Need::Read,
        }
    }

    /// The shared space a runtime manifest declares: `None` when its object
    /// has no `shared` member. Only that member is read here; the runtime
    /// checks the manifest's other members where it uses them.
    ///
    /// # Errors
    ///
    /// [`MalformedDeclaration`] for a manifest over
    /// [`MAX_RUNTIME_MANIFEST_BYTES`], text that is not one JSON object under
    /// the rules of application data, or a `shared` member of any other value.
    pub fn declared_in(manifest: &[u8]) -> Result<Option<Self>, MalformedDeclaration> {
        if manifest.len() > MAX_RUNTIME_MANIFEST_BYTES {
            return Err(MalformedDeclaration);
        }
        let text = std::str::from_utf8(manifest).map_err(|_| MalformedDeclaration)?;
        let DataValue::Object(members) =
            DataValue::parse_unbounded(text).map_err(|_| MalformedDeclaration)?
        else {
            return Err(MalformedDeclaration);
        };
        match members.get("shared") {
            None => Ok(None),
            Some(DataValue::String(name)) if name == "editors" => Ok(Some(Self::Editors)),
            Some(DataValue::String(name)) if name == "members" => Ok(Some(Self::Members)),
            Some(_) => Err(MalformedDeclaration),
        }
    }
}

/// The number an artifact's member list is at: zero until the first change of
/// access, then raised by one with each change (decision 0060 rule 6). A grant
/// or a revocation is conditional on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessVersion(pub u64);

impl AccessVersion {
    /// The version of an artifact whose access never changed.
    pub const INITIAL: Self = Self(0);
}

impl fmt::Display for AccessVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// The generation of one user's access to one artifact (decision 0060 rule
/// 16): raised by every grant, role change and revocation of that user and by
/// every move of the artifact to or from the trash, and never lowered, also
/// across a revocation and a later grant. Zero means the user was never a
/// member, which a registration of a member never saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessGeneration(pub u64);

impl AccessGeneration {
    /// The generation of a user who was never a member.
    pub const NEVER: Self = Self(0);
}

impl fmt::Display for AccessGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Access, MAX_RUNTIME_MANIFEST_BYTES, MalformedDeclaration, Need, Role, SharedWriters,
    };

    #[test]
    fn each_stand_allows_what_rule_3_says() {
        let table = [
            (Access::Owner, [true, true, true]),
            (Access::Member(Role::Editor), [true, true, false]),
            (Access::Member(Role::Viewer), [true, false, false]),
        ];
        for (access, allowed) in table {
            for (need, expected) in [Need::Read, Need::Change, Need::Own]
                .into_iter()
                .zip(allowed)
            {
                assert_eq!(access.permits(need), expected, "{access:?} {need:?}");
            }
        }
    }

    #[test]
    fn a_manifest_declares_a_shared_space_by_one_member() {
        let declared = |text: &str| SharedWriters::declared_in(text.as_bytes());
        assert_eq!(declared(r#"{"sdk":"x"}"#), Ok(None));
        assert_eq!(
            declared(r#"{"sdk":"x","shared":"editors"}"#),
            Ok(Some(SharedWriters::Editors))
        );
        assert_eq!(
            declared(r#"{"shared":"members"}"#),
            Ok(Some(SharedWriters::Members))
        );
        for text in [
            r#"{"shared":"viewers"}"#,
            r#"{"shared":"Editors"}"#,
            r#"{"shared":null}"#,
            r#"{"shared":["editors"]}"#,
            r#"{"shared":"editors","shared":"members"}"#,
            r#"["shared"]"#,
            "not json",
        ] {
            assert_eq!(declared(text), Err(MalformedDeclaration), "{text}");
        }
        let long = format!(
            r#"{{"shared":"editors","pad":"{}"}}"#,
            "a".repeat(MAX_RUNTIME_MANIFEST_BYTES)
        );
        assert_eq!(declared(&long), Err(MalformedDeclaration));
        assert_eq!(SharedWriters::Editors.need(), Need::Change);
        assert_eq!(SharedWriters::Members.need(), Need::Read);
    }

    #[test]
    fn a_role_reads_back_only_from_its_own_name() {
        for role in [Role::Editor, Role::Viewer] {
            assert_eq!(role.as_str().parse(), Ok(role));
        }
        for name in ["", "owner", "Editor", " viewer", "editors"] {
            assert!(name.parse::<Role>().is_err(), "accepted {name:?}");
        }
    }
}
