//! Listing the members of an object in a data space (decision 0040).
//!
//! A query names a path, an optional name prefix, whether to return values,
//! a page size, and the name after which to continue. Members are ordered by
//! the UTF-8 bytes of their names, which is the canonical order of a space, so
//! a cursor that is a member name stays valid when members are written between
//! two pages. A page has a bound on the bytes of the values it carries; a
//! member whose value alone is over that bound is listed without its value.

use std::collections::BTreeMap;
use std::ops::Bound;

use crate::data::{DataError, DataPath, DataValue, MAX_MEMBER_NAME_BYTES};

/// Most members one page lists.
pub const MAX_PAGE_MEMBERS: usize = 100;
/// Members one page lists when the query gives no limit.
pub const DEFAULT_PAGE_MEMBERS: usize = 25;
/// Largest canonical value, in bytes, that a page carries for one member, and
/// the most value bytes one page carries in total.
pub const MAX_PAGE_VALUE_BYTES: usize = 128 * 1024;

/// What a page carries for each member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemberSelect {
    /// Names only.
    #[default]
    Names,
    /// Names and canonical values.
    Values,
}

/// A validated query for the members of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberQuery {
    path: DataPath,
    prefix: String,
    select: MemberSelect,
    limit: usize,
    after: Option<String>,
}

impl MemberQuery {
    /// A query after checking its rules.
    ///
    /// # Errors
    ///
    /// [`DataError::InvalidQuery`] when `prefix` is over 256 bytes, `limit` is
    /// not in `1..=100`, or `after` is empty or over 256 bytes. The error
    /// names the rule and never the value.
    pub fn new(
        path: DataPath,
        prefix: String,
        select: MemberSelect,
        limit: usize,
        after: Option<String>,
    ) -> Result<Self, DataError> {
        if prefix.len() > MAX_MEMBER_NAME_BYTES {
            return Err(DataError::InvalidQuery("prefix is at most 256 bytes"));
        }
        if !(1..=MAX_PAGE_MEMBERS).contains(&limit) {
            return Err(DataError::InvalidQuery("limit is 1 to 100"));
        }
        if after
            .as_ref()
            .is_some_and(|name| name.is_empty() || name.len() > MAX_MEMBER_NAME_BYTES)
        {
            return Err(DataError::InvalidQuery(
                "after is a member name of 1 to 256 bytes",
            ));
        }
        Ok(Self {
            path,
            prefix,
            select,
            limit,
            after,
        })
    }

    /// The object whose members are listed.
    #[must_use]
    pub fn path(&self) -> &DataPath {
        &self.path
    }

    /// Only members whose name starts with this are listed.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// What each member carries.
    #[must_use]
    pub fn select(&self) -> MemberSelect {
        self.select
    }

    /// Most members in a page.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The member after which the page begins.
    #[must_use]
    pub fn after(&self) -> Option<&str> {
        self.after.as_deref()
    }
}

/// One listed member. Each variant is exactly what the member has: a name
/// alone, a name and a value, or a name and the fact that the value was too
/// large to carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    /// A member of a page of names.
    Name(String),
    /// A member of a page of values, with the canonical text of its value.
    Value {
        /// The member name.
        name: String,
        /// The canonical JSON text of the value.
        canonical: String,
    },
    /// A member of a page of values whose canonical value is over
    /// [`MAX_PAGE_VALUE_BYTES`].
    TooLarge(String),
}

impl Member {
    /// The member name.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Name(name) | Self::TooLarge(name) | Self::Value { name, .. } => name,
        }
    }
}

/// A page of members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberPage {
    /// Whether an object is at the path. When false there are no members.
    pub collection: bool,
    /// The members, ordered by the UTF-8 bytes of their names.
    pub members: Vec<Member>,
    /// The name to pass as `after` for the next page, present exactly when
    /// more members match.
    pub next_after: Option<String>,
}

impl DataValue {
    /// The page of members of this value that `query` asks for. A value that
    /// is not an object has no members; `query.path()` is not read here, the
    /// caller has already followed it.
    #[must_use]
    pub fn members(&self, query: &MemberQuery) -> MemberPage {
        let DataValue::Object(members) = self else {
            return MemberPage {
                collection: false,
                members: Vec::new(),
                next_after: None,
            };
        };
        page(members, query)
    }
}

fn page(members: &BTreeMap<String, DataValue>, query: &MemberQuery) -> MemberPage {
    let prefix = query.prefix();
    // The first name that can match: after the cursor, and not before the
    // prefix (every name with the prefix sorts at or after it).
    let lower = match query.after() {
        Some(after) if after >= prefix => Bound::Excluded(after),
        _ => Bound::Included(prefix),
    };
    let mut matching = members
        .range::<str, _>((lower, Bound::Unbounded))
        .take_while(|(name, _)| name.starts_with(prefix))
        .peekable();
    let mut listed = Vec::new();
    let mut value_bytes = 0_usize;
    let mut next_after = None;
    while let Some((name, value)) = matching.next() {
        let member = match query.select() {
            MemberSelect::Names => Member::Name(name.clone()),
            MemberSelect::Values => {
                let canonical = value.to_canonical();
                if canonical.len() > MAX_PAGE_VALUE_BYTES {
                    Member::TooLarge(name.clone())
                } else if value_bytes + canonical.len() > MAX_PAGE_VALUE_BYTES {
                    // The page is full. Something was listed already, because
                    // a value alone never exceeds the bound.
                    next_after = listed.last().map(|last: &Member| last.name().to_owned());
                    break;
                } else {
                    value_bytes += canonical.len();
                    Member::Value {
                        name: name.clone(),
                        canonical,
                    }
                }
            }
        };
        listed.push(member);
        if listed.len() == query.limit() {
            if matching.peek().is_some() {
                next_after = Some(name.clone());
            }
            break;
        }
    }
    MemberPage {
        collection: true,
        members: listed,
        next_after,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> DataValue {
        DataValue::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn query(prefix: &str, select: MemberSelect, limit: usize, after: Option<&str>) -> MemberQuery {
        MemberQuery::new(
            DataPath::root(),
            prefix.to_owned(),
            select,
            limit,
            after.map(str::to_owned),
        )
        .unwrap_or_else(|error| panic!("query: {error}"))
    }

    fn names(page: &MemberPage) -> Vec<&str> {
        page.members.iter().map(Member::name).collect()
    }

    #[test]
    fn members_are_ordered_by_utf8_bytes_not_by_characters() {
        // U+FF5E sorts after U+10000 by UTF-16 code units but before it by
        // UTF-8 bytes; "B" sorts before "a".
        let object = value("{\"a\":1,\"B\":2,\"\u{ff5e}\":3,\"\u{10000}\":4}");
        let page = object.members(&query("", MemberSelect::Names, 100, None));
        assert_eq!(names(&page), ["B", "a", "\u{ff5e}", "\u{10000}"]);
    }

    #[test]
    fn a_cursor_continues_after_a_name_that_is_gone() {
        let before = value("{\"a\":1,\"b\":2,\"c\":3,\"d\":4}");
        let first = before.members(&query("", MemberSelect::Names, 2, None));
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(first.next_after.as_deref(), Some("b"));
        // "b" is deleted and "bb" and "0" are written between the pages.
        let after = value("{\"0\":0,\"a\":1,\"bb\":5,\"c\":3,\"d\":4}");
        let second = after.members(&query("", MemberSelect::Names, 2, Some("b")));
        assert_eq!(names(&second), ["bb", "c"]);
        assert_eq!(second.next_after.as_deref(), Some("c"));
        let third = after.members(&query("", MemberSelect::Names, 2, Some("c")));
        assert_eq!(names(&third), ["d"]);
        assert_eq!(third.next_after, None, "the last page has no cursor");
    }

    #[test]
    fn a_full_last_page_has_no_cursor() {
        let object = value("{\"a\":1,\"b\":2}");
        let page = object.members(&query("", MemberSelect::Names, 2, None));
        assert_eq!(names(&page), ["a", "b"]);
        assert_eq!(page.next_after, None);
    }

    #[test]
    fn the_prefix_bounds_the_listing_with_and_without_a_cursor() {
        let object = value("{\"ant\":1,\"apple\":2,\"apricot\":3,\"bee\":4}");
        let all = object.members(&query("ap", MemberSelect::Names, 100, None));
        assert_eq!(names(&all), ["apple", "apricot"]);
        // A cursor before the prefix does not widen the listing.
        let before = object.members(&query("ap", MemberSelect::Names, 100, Some("a")));
        assert_eq!(names(&before), ["apple", "apricot"]);
        // A cursor past every match lists nothing.
        let past = object.members(&query("ap", MemberSelect::Names, 100, Some("b")));
        assert!(past.members.is_empty() && past.next_after.is_none());
        let none = object.members(&query("zz", MemberSelect::Names, 100, None));
        assert!(none.collection && none.members.is_empty());
    }

    #[test]
    fn a_member_has_exactly_what_the_selection_allows() {
        let object = value("{\"a\":{\"k\":[1,2]},\"b\":null}");
        let names_only = object.members(&query("", MemberSelect::Names, 10, None));
        assert_eq!(
            names_only.members,
            [Member::Name("a".into()), Member::Name("b".into())]
        );
        let values = object.members(&query("", MemberSelect::Values, 10, None));
        assert_eq!(
            values.members,
            [
                Member::Value {
                    name: "a".into(),
                    canonical: "{\"k\":[1,2]}".into()
                },
                Member::Value {
                    name: "b".into(),
                    canonical: "null".into()
                },
            ]
        );
    }

    #[test]
    fn a_value_over_the_bound_is_listed_without_its_value() {
        let big = "x".repeat(MAX_PAGE_VALUE_BYTES);
        // The canonical text has two quotes more than the string.
        let object = value(&format!("{{\"big\":\"{big}\",\"small\":1}}"));
        let page = object.members(&query("", MemberSelect::Values, 10, None));
        assert_eq!(
            page.members,
            [
                Member::TooLarge("big".into()),
                Member::Value {
                    name: "small".into(),
                    canonical: "1".into()
                },
            ]
        );
        let exact = "x".repeat(MAX_PAGE_VALUE_BYTES - 2);
        let fits = value(&format!("{{\"fits\":\"{exact}\"}}"));
        let page = fits.members(&query("", MemberSelect::Values, 10, None));
        assert!(
            matches!(&page.members[..], [Member::Value { canonical, .. }] if canonical.len() == MAX_PAGE_VALUE_BYTES),
            "a value of exactly the bound is carried: {page:?}"
        );
    }

    #[test]
    fn a_page_ends_before_the_member_that_would_pass_the_value_bound() {
        let half = "x".repeat(MAX_PAGE_VALUE_BYTES / 2 - 2);
        let object = value(&format!(
            "{{\"a\":\"{half}\",\"b\":\"{half}\",\"c\":\"{half}\",\"d\":1}}"
        ));
        let first = object.members(&query("", MemberSelect::Values, 100, None));
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(first.next_after.as_deref(), Some("b"));
        let second = object.members(&query("", MemberSelect::Values, 100, Some("b")));
        assert_eq!(names(&second), ["c", "d"]);
        assert_eq!(second.next_after, None);
    }

    #[test]
    fn a_value_that_is_not_an_object_has_no_members() {
        for text in ["[1,2]", "\"a\"", "7", "null", "true"] {
            let page = value(text).members(&query("", MemberSelect::Names, 10, None));
            assert!(
                !page.collection && page.members.is_empty() && page.next_after.is_none(),
                "{text}"
            );
        }
    }

    #[test]
    fn query_rules_name_the_rule_and_not_the_value() {
        let make = |prefix: &str, limit: usize, after: Option<&str>| {
            MemberQuery::new(
                DataPath::root(),
                prefix.to_owned(),
                MemberSelect::Names,
                limit,
                after.map(str::to_owned),
            )
        };
        assert!(make("", 1, None).is_ok());
        assert!(make("", 100, None).is_ok());
        assert!(make(&"p".repeat(256), 10, Some(&"a".repeat(256))).is_ok());
        for (label, result) in [
            ("limit 0", make("", 0, None)),
            ("limit 101", make("", 101, None)),
            ("prefix 257", make(&"p".repeat(257), 10, None)),
            ("after empty", make("", 10, Some(""))),
            ("after 257", make("", 10, Some(&"a".repeat(257)))),
        ] {
            let Err(DataError::InvalidQuery(rule)) = result else {
                panic!("{label} was accepted");
            };
            assert!(
                !rule.contains("ppp") && !rule.contains("aaa"),
                "{label}: {rule}"
            );
        }
    }
}
