//! Who may reach an artifact other than its owner (decision 0060).
//!
//! An artifact has one owner for its whole life. The owner may make another
//! registered user a member with one [`Role`]. This module names the roles,
//! how a user stands towards one artifact, and what each stand allows (0060
//! rule 3), so the application core and the store, which reads the role again
//! where a write commits (rule 5), apply one table.

use std::fmt;
use std::str::FromStr;

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
    use super::{Access, Need, Role};

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
    fn a_role_reads_back_only_from_its_own_name() {
        for role in [Role::Editor, Role::Viewer] {
            assert_eq!(role.as_str().parse(), Ok(role));
        }
        for name in ["", "owner", "Editor", " viewer", "editors"] {
            assert!(name.parse::<Role>().is_err(), "accepted {name:?}");
        }
    }
}
