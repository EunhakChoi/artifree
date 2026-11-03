//! Listing the members of a collection of application data (decision 0040).
//!
//! A collection is the object at a path of a [`DataSpace`]. A page lists its
//! members in the byte order of their UTF-8 names, after an optional cursor,
//! within two size bounds: a member whose value alone is larger than
//! [`MAX_MEMBER_VALUE_BYTES`] is reported without it, and the values of a page
//! add up to at most [`MAX_PAGE_VALUE_BYTES`]. Sizes are bytes of canonical
//! text.

use std::ops::Bound;

use crate::data::{DataError, DataPath, DataSpace, DataValue, MAX_MEMBER_NAME_BYTES};

/// Members in a page when the query names no limit.
pub const DEFAULT_LIMIT: usize = 50;
/// Most members in a page.
pub const MAX_LIMIT: usize = 100;
/// Largest canonical value that a page carries; a larger one is `too large`.
pub const MAX_MEMBER_VALUE_BYTES: usize = 32 * 1024;
/// Most bytes of canonical value text in one page.
pub const MAX_PAGE_VALUE_BYTES: usize = 256 * 1024;

/// What a page says about each member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// Names only.
    Names,
    /// Names and values.
    Values,
}

/// A validated query for one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembersQuery {
    path: DataPath,
    select: Select,
    limit: usize,
    after: Option<String>,
}

impl MembersQuery {
    /// A query for `limit` members (the default when `None`) strictly after
    /// the member named `after`.
    ///
    /// # Errors
    ///
    /// [`DataError::InvalidQuery`] for a limit outside `1..=100`, or an
    /// `after` that is empty or longer than 256 bytes.
    pub fn new(
        path: DataPath,
        select: Select,
        limit: Option<usize>,
        after: Option<String>,
    ) -> Result<Self, DataError> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(DataError::InvalidQuery("limit is 1 to 100"));
        }
        if after
            .as_deref()
            .is_some_and(|name| name.is_empty() || name.len() > MAX_MEMBER_NAME_BYTES)
        {
            return Err(DataError::InvalidQuery("after is 1 to 256 bytes"));
        }
        Ok(Self {
            path,
            select,
            limit,
            after,
        })
    }

    /// The collection.
    #[must_use]
    pub fn path(&self) -> &DataPath {
        &self.path
    }

    /// What the page says about each member.
    #[must_use]
    pub fn select(&self) -> Select {
        self.select
    }

    /// The most members in the page.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The cursor.
    #[must_use]
    pub fn after(&self) -> Option<&str> {
        self.after.as_deref()
    }
}

/// What a page says about one member's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberValue {
    /// The query selected names only.
    Hidden,
    /// The canonical JSON text of the value.
    Text(String),
    /// The value is larger than [`MAX_MEMBER_VALUE_BYTES`].
    TooLarge,
}

/// One member of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member's name.
    pub name: String,
    /// Its value.
    pub value: MemberValue,
}

/// One page of members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    /// Whether an object is at the path. When false the page is empty.
    pub is_collection: bool,
    /// The members, in byte order of their names.
    pub members: Vec<Member>,
    /// The last member's name, when more members follow.
    pub next_after: Option<String>,
}

impl DataSpace {
    /// The page `query` asks for.
    #[must_use]
    pub fn members(&self, query: &MembersQuery) -> Members {
        let Some(DataValue::Object(map)) = self.get(&query.path) else {
            return Members {
                is_collection: false,
                members: Vec::new(),
                next_after: None,
            };
        };
        let rest: Box<dyn Iterator<Item = (&String, &DataValue)>> = match &query.after {
            Some(after) => {
                Box::new(map.range::<str, _>((Bound::Excluded(after.as_str()), Bound::Unbounded)))
            }
            None => Box::new(map.iter()),
        };
        let mut members: Vec<Member> = Vec::new();
        let mut budget = MAX_PAGE_VALUE_BYTES;
        let mut more = false;
        for (name, value) in rest {
            if members.len() == query.limit {
                more = true;
                break;
            }
            let value = match query.select {
                Select::Names => MemberValue::Hidden,
                Select::Values => {
                    let text = value.to_canonical();
                    if text.len() > MAX_MEMBER_VALUE_BYTES {
                        MemberValue::TooLarge
                    } else if text.len() > budget {
                        more = true;
                        break;
                    } else {
                        budget -= text.len();
                        MemberValue::Text(text)
                    }
                }
            };
            members.push(Member {
                name: name.clone(),
                value,
            });
        }
        let next_after = if more {
            members.last().map(|member| member.name.clone())
        } else {
            None
        };
        Members {
            is_collection: true,
            members,
            next_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(text: &str) -> DataSpace {
        // As the store reads a space back: a space may exceed the size of one
        // written value.
        DataSpace::from_canonical(text).unwrap_or_else(|error| panic!("{error}: not a space"))
    }

    fn query(path: &str, select: Select, limit: usize, after: Option<&str>) -> MembersQuery {
        let path = path
            .parse()
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        MembersQuery::new(path, select, Some(limit), after.map(str::to_owned))
            .unwrap_or_else(|error| panic!("query: {error}"))
    }

    fn names(page: &Members) -> Vec<&str> {
        page.members.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn orders_by_utf8_bytes_not_utf16_units() {
        // U+FF5E is one UTF-16 unit above 0xD800; U+10000 is a surrogate
        // pair below it. UTF-8 order puts U+FF5E first, UTF-16 order last.
        let space = space("{\"\u{10000}\":1,\"\u{ff5e}\":2,\"z\":3}");
        let page = space.members(&query("", Select::Names, 10, None));
        assert_eq!(names(&page), ["z", "\u{ff5e}", "\u{10000}"]);
    }

    #[test]
    fn the_cursor_is_exclusive_and_pages_chain_to_the_end() {
        let space = space(r#"{"a":1,"b":2,"c":3,"d":4,"e":5}"#);
        let first = space.members(&query("", Select::Names, 2, None));
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(first.next_after.as_deref(), Some("b"));
        let second = space.members(&query("", Select::Names, 2, first.next_after.as_deref()));
        assert_eq!(names(&second), ["c", "d"]);
        assert_eq!(second.next_after.as_deref(), Some("d"));
        let last = space.members(&query("", Select::Names, 2, second.next_after.as_deref()));
        assert_eq!(names(&last), ["e"]);
        assert_eq!(last.next_after, None);
    }

    #[test]
    fn an_exact_fit_has_no_next_page() {
        let space = space(r#"{"a":1,"b":2}"#);
        let page = space.members(&query("", Select::Names, 2, None));
        assert_eq!(names(&page), ["a", "b"]);
        assert_eq!(page.next_after, None);
    }

    #[test]
    fn a_cursor_that_is_not_a_member_starts_at_the_next_name() {
        let space = space(r#"{"a":1,"c":3}"#);
        let page = space.members(&query("", Select::Names, 5, Some("b")));
        assert_eq!(names(&page), ["c"]);
    }

    #[test]
    fn values_are_canonical_and_names_hide_them() {
        let space = space(r#"{"a":{"y":1,"x":[true,null]},"b":"s"}"#);
        let page = space.members(&query("", Select::Values, 5, None));
        assert_eq!(
            page.members[0].value,
            MemberValue::Text(r#"{"x":[true,null],"y":1}"#.into())
        );
        assert_eq!(page.members[1].value, MemberValue::Text(r#""s""#.into()));
        let page = space.members(&query("", Select::Names, 5, None));
        assert!(page.members.iter().all(|m| m.value == MemberValue::Hidden));
    }

    #[test]
    fn a_stored_null_is_a_value() {
        let space = space(r#"{"a":null}"#);
        let page = space.members(&query("", Select::Values, 5, None));
        assert_eq!(page.members[0].value, MemberValue::Text("null".into()));
    }

    #[test]
    fn a_value_over_the_member_bound_is_marked_and_has_no_text() {
        let big = "x".repeat(MAX_MEMBER_VALUE_BYTES);
        let exact = "y".repeat(MAX_MEMBER_VALUE_BYTES - 2);
        let one_over = "z".repeat(MAX_MEMBER_VALUE_BYTES - 1);
        let space = space(&format!(
            r#"{{"big":"{big}","exact":"{exact}","over":"{one_over}"}}"#
        ));
        let page = space.members(&query("", Select::Values, 5, None));
        assert_eq!(page.members[0].value, MemberValue::TooLarge);
        assert!(
            matches!(&page.members[1].value, MemberValue::Text(t) if t.len() == MAX_MEMBER_VALUE_BYTES),
            "a value of exactly the bound is carried"
        );
        assert_eq!(
            page.members[2].value,
            MemberValue::TooLarge,
            "a value one byte over the bound is marked"
        );
        let names_only = space.members(&query("", Select::Names, 5, None));
        assert!(
            names_only
                .members
                .iter()
                .all(|m| m.value == MemberValue::Hidden)
        );
    }

    #[test]
    fn a_page_ends_before_the_value_that_would_pass_the_page_bound() {
        // Nine values of just under 30 KiB: 8 fit in 256 KiB, the 9th does not.
        let text = "z".repeat(30 * 1024);
        let members: Vec<String> = (0..9).map(|n| format!(r#""m{n}":"{text}""#)).collect();
        let space = space(&format!("{{{}}}", members.join(",")));
        let page = space.members(&query("", Select::Values, 100, None));
        let total: usize = page
            .members
            .iter()
            .map(|m| match &m.value {
                MemberValue::Text(t) => t.len(),
                _ => 0,
            })
            .sum();
        assert!(total <= MAX_PAGE_VALUE_BYTES, "page carries {total} bytes");
        assert_eq!(page.members.len(), 8);
        assert_eq!(page.next_after.as_deref(), Some("m7"));
        let rest = space.members(&query("", Select::Values, 100, page.next_after.as_deref()));
        assert_eq!(names(&rest), ["m8"]);
    }

    #[test]
    fn a_path_below_the_root_lists_that_object() {
        let space = space(r#"{"todos":{"b":1,"a":2},"n":5}"#);
        let page = space.members(&query("todos", Select::Names, 5, None));
        assert!(page.is_collection);
        assert_eq!(names(&page), ["a", "b"]);
    }

    #[test]
    fn a_missing_path_and_a_value_that_is_not_an_object_are_empty_non_collections() {
        let space = space(r#"{"list":[1,2],"s":"x"}"#);
        for path in ["nothing", "list", "s", "s/deeper"] {
            let page = space.members(&query(path, Select::Values, 5, None));
            assert_eq!(
                page,
                Members {
                    is_collection: false,
                    members: vec![],
                    next_after: None
                },
                "path {path}"
            );
        }
    }

    #[test]
    fn an_empty_object_is_a_collection_without_members() {
        let page = space(r#"{"e":{}}"#).members(&query("e", Select::Names, 5, None));
        assert!(page.is_collection && page.members.is_empty());
    }

    #[test]
    fn the_query_refuses_limits_and_cursors_out_of_range() {
        let build = |limit, after: Option<String>| {
            MembersQuery::new(DataPath::root(), Select::Names, limit, after)
        };
        assert!(build(Some(0), None).is_err());
        assert!(build(Some(101), None).is_err());
        assert!(build(Some(100), None).is_ok());
        assert!(build(Some(1), None).is_ok());
        assert_eq!(
            build(None, None).map(|q| q.limit()),
            Ok(DEFAULT_LIMIT),
            "the default limit"
        );
        assert!(build(None, Some(String::new())).is_err());
        assert!(build(None, Some("a".repeat(257))).is_err());
        assert!(build(None, Some("a".repeat(256))).is_ok());
        // Bytes, not characters: 129 two-byte characters are 258 bytes.
        assert!(build(None, Some("é".repeat(129))).is_err());
    }
}
