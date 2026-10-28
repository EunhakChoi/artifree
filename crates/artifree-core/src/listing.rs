//! Artifact list queries: what to list, in which order, and the opaque token
//! that continues a list after its last page.

use std::str::FromStr;

use artifree_domain::identity::{InvalidValue, UnixMillis};
use artifree_ports::artifact_store::{ArtifactRecord, ListOrder, ListPosition};
use sha2::{Digest, Sha256};

/// Maximum search text length in Unicode scalar values, as for a title.
pub const MAX_SEARCH_CHARS: usize = 200;

/// Text to find in titles: 1-200 characters without control characters.
/// Matching ignores the case of ASCII letters only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchText(String);

impl SearchText {
    /// The text as given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SearchText {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let length = value.chars().count();
        if length == 0 || length > MAX_SEARCH_CHARS {
            return Err(InvalidValue {
                what: "search text",
                rule: "must contain 1-200 characters",
            });
        }
        if value.chars().any(char::is_control) {
            return Err(InvalidValue {
                what: "search text",
                rule: "must not contain control characters",
            });
        }
        Ok(Self(value.to_owned()))
    }
}

/// The number of artifacts in one page, from 1 to [`PageSize::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSize(usize);

impl PageSize {
    /// The largest page, which is also the default: a caller written before
    /// lists had pages still receives every artifact of a workspace up to
    /// this size.
    pub const MAX: Self = Self(500);

    /// The number of artifacts.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::MAX
    }
}

impl TryFrom<u64> for PageSize {
    type Error = InvalidValue;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        usize::try_from(value)
            .ok()
            .filter(|size| (1..=Self::MAX.0).contains(size))
            .map(Self)
            .ok_or(InvalidValue {
                what: "page size",
                rule: "must be 1-500",
            })
    }
}

/// What to list. The default lists everything, most recently updated first,
/// in pages of the largest size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    /// Only titles containing this text.
    pub search: Option<SearchText>,
    /// Only pinned, or only unpinned, artifacts.
    pub pinned: Option<bool>,
    /// Only artifacts in the trash, instead of those outside it.
    pub deleted: bool,
    /// The order.
    pub order: ListOrder,
    /// The page size.
    pub limit: PageSize,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            search: None,
            pinned: None,
            deleted: false,
            order: ListOrder::Updated,
            limit: PageSize::default(),
        }
    }
}

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPage {
    /// The artifacts, in the query's order.
    pub artifacts: Vec<ArtifactRecord>,
    /// Continues the list, present only when more artifacts follow.
    pub next_page_token: Option<String>,
}

const TOKEN_VERSION: &str = "v1";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// The token that continues `query` after `last`. It names the order, a
/// digest of the filters, and the last position; the page size may change
/// between pages.
pub(crate) fn page_token(query: &ListQuery, last: &ArtifactRecord) -> String {
    let at = match query.order {
        ListOrder::Updated => last.updated_at,
        ListOrder::Created => last.created_at,
    };
    format!(
        "{TOKEN_VERSION}.{}.{}.{}.{}",
        order_name(query.order),
        filters_digest(query),
        at.0,
        last.id
    )
}

/// The position a token continues from, or `None` when it is malformed or
/// belongs to another order or other filters.
pub(crate) fn position(token: &str, query: &ListQuery) -> Option<ListPosition> {
    let mut parts = token.splitn(5, '.');
    let (Some(version), Some(order), Some(digest), Some(at), Some(id)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    if version != TOKEN_VERSION
        || order != order_name(query.order)
        || digest != filters_digest(query)
    {
        return None;
    }
    Some(ListPosition {
        at: UnixMillis(at.parse().ok()?),
        id: id.parse().ok()?,
    })
}

fn order_name(order: ListOrder) -> &'static str {
    match order {
        ListOrder::Updated => "updated",
        ListOrder::Created => "created",
    }
}

/// The first eight bytes of a digest of the filters, in hexadecimal.
fn filters_digest(query: &ListQuery) -> String {
    let mut hasher = Sha256::new();
    match &query.search {
        Some(search) => {
            hasher.update([1]);
            hasher.update((search.as_str().len() as u64).to_be_bytes());
            hasher.update(search.as_str());
        }
        None => hasher.update([0]),
    }
    hasher.update([match query.pinned {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }]);
    // Added after the other fields, and only for the trash, so tokens of
    // other lists keep their digest.
    if query.deleted {
        hasher.update(b"trash");
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for byte in &digest[..8] {
        hex.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

#[cfg(test)]
mod tests {
    use artifree_domain::identity::{ActorId, ArtifactId, Title};
    use artifree_domain::{ArtifactKind, MetadataRevision, SourceRevision};

    use super::*;

    fn parsed<T: FromStr>(value: &str) -> T
    where
        T::Err: std::fmt::Display,
    {
        value
            .parse()
            .unwrap_or_else(|error| panic!("{value}: {error}"))
    }

    fn size(value: u64) -> PageSize {
        PageSize::try_from(value).unwrap_or_else(|error| panic!("{value}: {error}"))
    }

    fn record() -> ArtifactRecord {
        ArtifactRecord {
            id: ArtifactId::from_random([7; 16]),
            owner: parsed::<ActorId>("local"),
            kind: ArtifactKind::App,
            title: parsed::<Title>("Plan"),
            pinned: false,
            current_revision: SourceRevision(3),
            metadata_revision: MetadataRevision(1),
            deleted_at: None,
            created_at: UnixMillis(10),
            updated_at: UnixMillis(20),
        }
    }

    #[test]
    fn a_token_continues_only_the_query_it_came_from() {
        let query = ListQuery {
            search: Some(parsed("pla")),
            ..ListQuery::default()
        };
        let token = page_token(&query, &record());
        assert_eq!(
            position(&token, &query),
            Some(ListPosition {
                at: UnixMillis(20),
                id: record().id
            })
        );
        let smaller = ListQuery {
            limit: size(5),
            ..query.clone()
        };
        assert!(position(&token, &smaller).is_some(), "page size may change");
        for other in [
            ListQuery::default(),
            ListQuery {
                order: ListOrder::Created,
                ..query.clone()
            },
            ListQuery {
                pinned: Some(true),
                ..query.clone()
            },
            ListQuery {
                search: Some(parsed("plan")),
                ..query.clone()
            },
            ListQuery {
                deleted: true,
                ..query.clone()
            },
        ] {
            assert_eq!(position(&token, &other), None, "{other:?}");
        }
    }

    #[test]
    fn a_malformed_token_is_refused() {
        let query = ListQuery::default();
        let valid = page_token(&query, &record());
        for token in [
            "",
            "v1",
            &valid.replacen("v1", "v2", 1),
            &valid.replace(".20.", ".x."),
            &format!("{valid}0"),
        ] {
            assert_eq!(position(token, &query), None, "{token:?}");
        }
    }

    #[test]
    fn page_size_and_search_text_are_bounded() {
        assert!(PageSize::try_from(0).is_err());
        assert!(PageSize::try_from(501).is_err());
        assert_eq!(PageSize::try_from(500).map(PageSize::get), Ok(500));
        assert!("".parse::<SearchText>().is_err());
        assert!("x".repeat(201).parse::<SearchText>().is_err());
        assert!("tab\there".parse::<SearchText>().is_err());
        assert!(" spaced ".parse::<SearchText>().is_ok());
    }
}
