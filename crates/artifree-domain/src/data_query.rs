//! The query over a data space (decision 0038): list the members of an object
//! in the order of their names, a bounded page at a time.
//!
//! A query is validated and completed once, in [`MembersQuery::new`], so a
//! value of the type is always within the bounds below and two requests that
//! mean the same thing are equal. No error quotes the input.

use std::ops::Bound;

use crate::data::{
    DataError, DataPath, DataSpace, DataValue, MAX_MEMBER_NAME_BYTES, MAX_SEGMENT_BYTES,
    valid_segment,
};

/// Members in a page when the query names no limit.
pub const DEFAULT_LIMIT: usize = 50;
/// Most members in a page.
pub const MAX_LIMIT: usize = 100;
/// Most bytes of member names and canonical values in a page that selects
/// values. The first member is always listed, so a page makes progress: a
/// value is at most [`crate::data::MAX_VALUE_BYTES`] and a name 256 bytes.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Most bytes in a name prefix.
pub const MAX_PREFIX_BYTES: usize = MAX_SEGMENT_BYTES;

/// What a page holds of each member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemberSelect {
    /// Only the names.
    #[default]
    Names,
    /// The names and the values.
    Values,
}

/// A validated, completed query: the members of the object at `path` whose
/// names start with `prefix`, after the name `after`, at most `limit` of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembersQuery {
    path: DataPath,
    prefix: Option<String>,
    select: MemberSelect,
    limit: usize,
    after: Option<String>,
}

impl MembersQuery {
    /// Validates the parts of a query and fills in the defaults.
    ///
    /// # Errors
    ///
    /// [`DataError::InvalidQuery`] for a `prefix` that is empty, longer than
    /// 64 bytes, or has a character other than ASCII letters, digits, `_`, and
    /// `-`; a `limit` of 0 or more than [`MAX_LIMIT`]; and an `after` that is
    /// empty or longer than 256 bytes (the bounds of a member name).
    pub fn new(
        path: DataPath,
        prefix: Option<&str>,
        select: Option<MemberSelect>,
        limit: Option<u64>,
        after: Option<&str>,
    ) -> Result<Self, DataError> {
        if let Some(prefix) = prefix
            && !valid_segment(prefix)
        {
            return Err(DataError::InvalidQuery(
                "prefix is 1-64 ASCII letters, digits, '_' or '-'",
            ));
        }
        let limit = match limit {
            None => DEFAULT_LIMIT,
            Some(limit) => usize::try_from(limit)
                .ok()
                .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                .ok_or(DataError::InvalidQuery("limit is 1-100"))?,
        };
        if let Some(after) = after
            && !(1..=MAX_MEMBER_NAME_BYTES).contains(&after.len())
        {
            return Err(DataError::InvalidQuery(
                "after is a member name of 1-256 bytes",
            ));
        }
        Ok(Self {
            path,
            prefix: prefix.map(str::to_owned),
            select: select.unwrap_or_default(),
            limit,
            after: after.map(str::to_owned),
        })
    }

    /// The collection: the object whose members are listed.
    #[must_use]
    pub fn path(&self) -> &DataPath {
        &self.path
    }

    /// Only members whose name starts with this.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref()
    }

    /// What a page holds of each member.
    #[must_use]
    pub fn select(&self) -> MemberSelect {
        self.select
    }

    /// Most members in the page, 1 to [`MAX_LIMIT`].
    #[must_use]
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The page starts after this name.
    #[must_use]
    pub fn after(&self) -> Option<&str> {
        self.after.as_deref()
    }
}

/// One member of a listed object.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// The member's name.
    pub name: String,
    /// Its value, when the query selects values.
    pub value: Option<DataValue>,
}

/// A page of members.
#[derive(Debug, Clone, PartialEq)]
pub struct Members {
    /// Whether an object is at the path. False where nothing is and where the
    /// value is not an object; the page is then empty.
    pub exists: bool,
    /// The members of the page, by the bytes of their names.
    pub entries: Vec<Member>,
    /// The name to pass as `after` for the next page: the last name of this
    /// page, present exactly when a further matching member exists.
    pub next_after: Option<String>,
}

impl DataSpace {
    /// The page of members that `query` asks for.
    #[must_use]
    pub fn members(&self, query: &MembersQuery) -> Members {
        let Some(DataValue::Object(map)) = self.get(query.path()) else {
            return Members {
                exists: false,
                entries: Vec::new(),
                next_after: None,
            };
        };
        let prefix = query.prefix();
        // Start at the later of the prefix and the name after the cursor.
        let lower = match (query.after(), prefix) {
            (Some(after), Some(prefix)) if after < prefix => Bound::Included(prefix),
            (Some(after), _) => Bound::Excluded(after),
            (None, Some(prefix)) => Bound::Included(prefix),
            (None, None) => Bound::Unbounded,
        };
        let matching = map
            .range::<str, _>((lower, Bound::Unbounded))
            .take_while(|(name, _)| prefix.is_none_or(|prefix| name.starts_with(prefix)));
        let values = query.select() == MemberSelect::Values;
        let mut entries: Vec<Member> = Vec::new();
        let mut bytes = 0_usize;
        let mut more = false;
        for (name, value) in matching {
            let weight = name.len()
                + if values {
                    value.to_canonical().len()
                } else {
                    0
                };
            if entries.len() == query.limit()
                || (!entries.is_empty() && bytes + weight > MAX_PAGE_BYTES)
            {
                more = true;
                break;
            }
            bytes += weight;
            entries.push(Member {
                name: name.clone(),
                value: values.then(|| value.clone()),
            });
        }
        let next_after = more
            .then(|| entries.last().map(|member| member.name.clone()))
            .flatten();
        Members {
            exists: true,
            entries,
            next_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn space(text: &str) -> DataSpace {
        DataSpace::from_canonical(text).unwrap_or_else(|error| panic!("{text}: {error}"))
    }

    fn query(
        path: &str,
        prefix: Option<&str>,
        select: Option<MemberSelect>,
        limit: Option<u64>,
        after: Option<&str>,
    ) -> Result<MembersQuery, DataError> {
        let path: DataPath = path
            .parse()
            .unwrap_or_else(|error| panic!("{path}: {error}"));
        MembersQuery::new(path, prefix, select, limit, after)
    }

    fn names(members: &Members) -> Vec<&str> {
        members
            .entries
            .iter()
            .map(|member| member.name.as_str())
            .collect()
    }

    #[test]
    fn a_query_that_breaks_a_bound_is_refused_with_its_rule() {
        let long_prefix = "p".repeat(65);
        let long_name = "n".repeat(257);
        let cases: [(Result<MembersQuery, DataError>, &str); 8] = [
            (query("", Some(""), None, None, None), "prefix"),
            (query("", Some(&long_prefix), None, None, None), "prefix"),
            (query("", Some("a.b"), None, None, None), "prefix"),
            (query("", Some("a/b"), None, None, None), "prefix"),
            (query("", None, None, Some(0), None), "limit"),
            (query("", None, None, Some(101), None), "limit"),
            (query("", None, None, None, Some("")), "after"),
            (query("", None, None, None, Some(&long_name)), "after"),
        ];
        for (result, what) in cases {
            let Err(DataError::InvalidQuery(rule)) = result else {
                panic!("a bad {what} was accepted: {result:?}");
            };
            assert!(rule.starts_with(what), "{what}: {rule}");
        }
        // The refusal quotes the rule and never the input.
        let Err(error) = query("", Some("secret.value"), None, None, None) else {
            panic!("a bad prefix was accepted");
        };
        assert!(!error.to_string().contains("secret"), "{error}");
    }

    #[test]
    fn a_query_is_completed_with_its_defaults_and_equal_to_the_same_request() -> TestResult {
        let bare = query("todos", None, None, None, None)?;
        assert_eq!(bare.limit(), 50);
        assert_eq!(bare.select(), MemberSelect::Names);
        assert_eq!(bare.prefix(), None);
        assert_eq!(bare.after(), None);
        assert_eq!(bare.path().to_string(), "todos");
        let spelled = query("todos", None, Some(MemberSelect::Names), Some(50), None)?;
        assert_eq!(bare, spelled);
        // The bounds themselves are accepted, the byte bound of a name too.
        let edge = "n".repeat(256);
        let widest = query("", Some(&"p".repeat(64)), None, Some(100), Some(&edge))?;
        assert_eq!(widest.limit(), 100);
        assert_eq!(query("", None, None, Some(1), None)?.limit(), 1);
        Ok(())
    }

    #[test]
    fn members_come_in_the_order_of_their_utf8_bytes() -> TestResult {
        // 'B' < '_' < 'a' < 'b' < 'é' (0xC3 0xA9) in bytes; not in any locale.
        let data = space(r#"{"b":1,"é":2,"a":3,"_":4,"B":5}"#);
        let page = data.members(&query("", None, None, None, None)?);
        assert_eq!(names(&page), ["B", "_", "a", "b", "é"]);
        assert!(page.exists);
        assert_eq!(page.next_after, None);
        assert!(page.entries.iter().all(|member| member.value.is_none()));
        Ok(())
    }

    #[test]
    fn pages_follow_the_last_name_without_repeating_or_skipping() -> TestResult {
        let data = space(r#"{"a":1,"b":2,"c":3,"d":4,"e":5}"#);
        let first = data.members(&query("", None, None, Some(2), None)?);
        assert_eq!(names(&first), ["a", "b"]);
        assert_eq!(first.next_after.as_deref(), Some("b"));
        let second = data.members(&query(
            "",
            None,
            None,
            Some(2),
            first.next_after.as_deref(),
        )?);
        assert_eq!(names(&second), ["c", "d"]);
        assert_eq!(second.next_after.as_deref(), Some("d"));
        let third = data.members(&query(
            "",
            None,
            None,
            Some(2),
            second.next_after.as_deref(),
        )?);
        assert_eq!(names(&third), ["e"]);
        assert_eq!(third.next_after, None, "the last page promised another");
        // Exactly `limit` members left is the last page, not one more.
        let exact = data.members(&query("", None, None, Some(2), Some("c"))?);
        assert_eq!(names(&exact), ["d", "e"]);
        assert_eq!(exact.next_after, None);
        Ok(())
    }

    #[test]
    fn a_cursor_survives_writes_around_it() -> TestResult {
        let before = space(r#"{"a":1,"c":3,"e":5}"#);
        let page = before.members(&query("", None, None, Some(2), None)?);
        assert_eq!(names(&page), ["a", "c"]);
        // A member before the cursor, one after it, and the cursor's own
        // removal change nothing about where the next page starts.
        let after = space(r#"{"A":0,"d":4,"e":5,"f":6}"#);
        let next = after.members(&query("", None, None, Some(2), page.next_after.as_deref())?);
        assert_eq!(names(&next), ["d", "e"]);
        Ok(())
    }

    #[test]
    fn a_prefix_selects_a_range_and_meets_the_cursor_correctly() -> TestResult {
        let data = space(r#"{"item-1":1,"item-2":2,"item-3":3,"other":4,"a":5,"z":6}"#);
        let run = |prefix: &str, after: Option<&str>| -> Result<Vec<String>, DataError> {
            let page = data.members(&query("", Some(prefix), None, None, after)?);
            Ok(page.entries.into_iter().map(|member| member.name).collect())
        };
        assert_eq!(run("item-", None)?, ["item-1", "item-2", "item-3"]);
        // A cursor before the prefix starts at the prefix, inside it after the
        // cursor, and past it finds nothing.
        assert_eq!(run("item-", Some("a"))?, ["item-1", "item-2", "item-3"]);
        assert_eq!(run("item-", Some("item-1"))?, ["item-2", "item-3"]);
        assert_eq!(run("item-", Some("item-3"))?, Vec::<String>::new());
        assert_eq!(run("item-", Some("z"))?, Vec::<String>::new());
        assert_eq!(run("none", None)?, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn a_page_of_values_stops_at_the_byte_bound_but_always_holds_one_member() -> TestResult {
        let big = "x".repeat(250 * 1024);
        let text = format!(r#"{{"a":"{big}","b":"{big}","c":"{big}"}}"#);
        let data = space(&text);
        let page = data.members(&query("", None, Some(MemberSelect::Values), None, None)?);
        assert_eq!(
            names(&page),
            ["a", "b"],
            "250 KiB twice fits 512 KiB, three does not"
        );
        assert_eq!(page.next_after.as_deref(), Some("b"));
        let rest = data.members(&query(
            "",
            None,
            Some(MemberSelect::Values),
            None,
            page.next_after.as_deref(),
        )?);
        assert_eq!(names(&rest), ["c"]);
        assert_eq!(
            rest.entries
                .first()
                .and_then(|member| member.value.as_ref()),
            Some(&DataValue::String(big.clone()))
        );
        // The same members as names are not bounded by their values.
        let names_only = data.members(&query("", None, None, None, None)?);
        assert_eq!(names(&names_only), ["a", "b", "c"]);
        // A single value at the value limit still makes a page.
        let single = space(&format!(r#"{{"only":"{}"}}"#, "y".repeat(255 * 1024)));
        let page = single.members(&query("", None, Some(MemberSelect::Values), None, None)?);
        assert_eq!(names(&page), ["only"]);
        Ok(())
    }

    #[test]
    fn nothing_listed_where_no_object_is() -> TestResult {
        let data = space(r#"{"list":[1,2],"text":"x","nested":{"a":1}}"#);
        for path in ["missing", "list", "text", "text/deeper", "list/0"] {
            let page = data.members(&query(path, None, None, None, None)?);
            assert!(!page.exists, "{path}");
            assert!(page.entries.is_empty(), "{path}");
        }
        let nested = data.members(&query("nested", None, None, None, None)?);
        assert!(nested.exists);
        assert_eq!(names(&nested), ["a"]);
        // An object with no members exists and is empty.
        let empty = space("{}").members(&query("", None, None, None, None)?);
        assert!(empty.exists);
        assert!(empty.entries.is_empty());
        Ok(())
    }

    #[test]
    fn a_member_with_a_control_character_in_its_name_can_be_paged_past() -> TestResult {
        // `after` accepts any valid member name, so a name the escapes can
        // spell is not a place the listing gets stuck.
        let data = space(r#"{"a\u0001":1,"b":2}"#);
        let first = data.members(&query("", None, None, Some(1), None)?);
        assert_eq!(names(&first), ["a\u{1}"]);
        let second = data.members(&query(
            "",
            None,
            None,
            Some(1),
            first.next_after.as_deref(),
        )?);
        assert_eq!(names(&second), ["b"]);
        Ok(())
    }
}
