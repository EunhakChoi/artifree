//! `GET /v1/artifacts/{id}/data/members`: the members of a collection of the
//! caller's application data, in bounded pages (decision 0040).
//!
//! The query is validated and echoed in its normalized form. A page is cut
//! by the member count and by a byte bound, and a member whose value is over
//! its own bound is listed without it. Members come in the byte order of
//! their names, so a name is a stable cursor.

use std::ops::Bound;

use artifree_contracts::api::{
    DATA_MEMBER_MAX_BYTES, DATA_MEMBER_NAME_MAX_BYTES, DATA_MEMBERS_DEFAULT_LIMIT,
    DATA_MEMBERS_MAX_LIMIT, DATA_PAGE_MAX_BYTES, DataMember, DataMembers, DataMembersQuery,
    DataSelect, JsonValue,
};
use artifree_domain::data::{DataPath, DataValue};
use axum::Json;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};

use crate::auth::AuthContext;
use crate::http::AppState;
use crate::operations::{self, ApiError};

/// A members query after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MembersQuery {
    pub(crate) path: DataPath,
    pub(crate) select: DataSelect,
    pub(crate) limit: u64,
    pub(crate) after: Option<String>,
}

impl MembersQuery {
    /// Checks every parameter. A path that breaks the grammar is `422` as for
    /// a read of data; any other bad parameter is `400`.
    pub(crate) fn validate(query: DataMembersQuery) -> Result<Self, ApiError> {
        let path = operations::data_path("path", query.path.as_deref().unwrap_or_default())?;
        let limit = query.limit.unwrap_or(DATA_MEMBERS_DEFAULT_LIMIT);
        if !(1..=DATA_MEMBERS_MAX_LIMIT).contains(&limit) {
            return Err(ApiError::invalid(format!(
                "limit must be from 1 to {DATA_MEMBERS_MAX_LIMIT}"
            )));
        }
        if let Some(after) = &query.after
            && (after.is_empty() || after.len() > DATA_MEMBER_NAME_MAX_BYTES)
        {
            return Err(ApiError::invalid(format!(
                "after must be a member name of 1 to {DATA_MEMBER_NAME_MAX_BYTES} bytes"
            )));
        }
        Ok(Self {
            path,
            select: query.select.unwrap_or_default(),
            limit,
            after: query.after,
        })
    }
}

/// One page of members: whether an object is there, the members, and the
/// cursor of the next page when more follow.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) exists: bool,
    pub(crate) members: Vec<DataMember>,
    pub(crate) next_after: Option<String>,
}

/// The page of `query` over the value found at its path.
pub(crate) fn page(found: Option<&DataValue>, query: &MembersQuery) -> Page {
    let Some(DataValue::Object(members)) = found else {
        return Page {
            exists: false,
            members: Vec::new(),
            next_after: None,
        };
    };
    let start = query
        .after
        .as_deref()
        .map_or(Bound::Unbounded, Bound::Excluded);
    let mut page = Page {
        exists: true,
        members: Vec::new(),
        next_after: None,
    };
    let mut bytes = 0_usize;
    for (name, value) in members.range::<str, _>((start, Bound::Unbounded)) {
        let selected = match query.select {
            DataSelect::Names => None,
            DataSelect::Values => Some(value.to_canonical()),
        };
        let payload = selected
            .as_ref()
            .filter(|text| text.len() <= DATA_MEMBER_MAX_BYTES)
            .map_or(0, String::len);
        let full = page.members.len() >= usize::try_from(query.limit).unwrap_or(usize::MAX);
        // A name and a value of the bounded sizes always fit an empty page.
        let over = !page.members.is_empty() && bytes + name.len() + payload > DATA_PAGE_MAX_BYTES;
        if full || over {
            page.next_after = page.members.last().map(|member| member.name.clone());
            break;
        }
        bytes += name.len() + payload;
        page.members.push(match selected {
            None => DataMember {
                name: name.clone(),
                value: None,
                too_large: None,
            },
            Some(text) if text.len() > DATA_MEMBER_MAX_BYTES => DataMember {
                name: name.clone(),
                value: None,
                too_large: Some(true),
            },
            Some(text) => DataMember {
                name: name.clone(),
                value: JsonValue::from_text(text),
                too_large: None,
            },
        });
    }
    page
}

/// `GET /v1/artifacts/{id}/data/members`.
pub(crate) async fn list(
    State(state): State<AppState>,
    auth: AuthContext,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<DataMembersQuery>, QueryRejection>,
) -> Result<Json<DataMembers>, ApiError> {
    let Path(id) = path?;
    let id = operations::artifact_id(&id)?;
    let Query(query) = query?;
    let query = MembersQuery::validate(query)?;
    let artifact = id.to_string();
    let echo = query.clone();
    let actor = auth.actor;
    let (version, page) = state
        .run(move |artifacts| {
            let read = artifacts.data(&actor, &id, &query.path)?;
            Ok((read.version, page(read.value.as_ref(), &query)))
        })
        .await?;
    Ok(Json(DataMembers {
        artifact_id: artifact,
        path: echo.path.to_string(),
        select: echo.select,
        limit: echo.limit,
        after: echo.after,
        data_version: version.0,
        exists: page.exists,
        members: page.members,
        next_after: page.next_after,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(text: &str) -> DataValue {
        DataValue::parse_unbounded(text).unwrap_or_else(|error| panic!("fixture {text}: {error}"))
    }

    fn query(select: DataSelect, limit: u64, after: Option<&str>) -> MembersQuery {
        MembersQuery {
            path: DataPath::root(),
            select,
            limit,
            after: after.map(str::to_owned),
        }
    }

    fn names(page: &Page) -> Vec<&str> {
        page.members.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn members_come_in_utf8_byte_order_and_after_is_exclusive() {
        // "Z" (0x5A) < "a" (0x61) < "é" (0xC3 0xA9): byte order, not locale.
        let value = object(r#"{"é":1,"a":2,"Z":3,"b":4}"#);
        let all = page(Some(&value), &query(DataSelect::Names, 10, None));
        assert_eq!(names(&all), ["Z", "a", "b", "é"]);
        let rest = page(Some(&value), &query(DataSelect::Names, 10, Some("a")));
        assert_eq!(names(&rest), ["b", "é"]);
        assert_eq!(rest.next_after, None);
    }

    #[test]
    fn next_after_is_present_exactly_when_more_members_follow() {
        let value = object(r#"{"a":1,"b":2,"c":3}"#);
        let two = page(Some(&value), &query(DataSelect::Names, 2, None));
        assert_eq!(
            (names(&two), two.next_after.as_deref()),
            (vec!["a", "b"], Some("b"))
        );
        let exact = page(Some(&value), &query(DataSelect::Names, 3, None));
        assert_eq!(
            (names(&exact), exact.next_after.as_deref()),
            (vec!["a", "b", "c"], None)
        );
    }

    #[test]
    fn names_carry_neither_value_nor_too_large_and_values_exactly_one() {
        let big = "x".repeat(DATA_MEMBER_MAX_BYTES);
        let value = object(&format!(r#"{{"big":"{big}","small":1}}"#));
        let names_only = page(Some(&value), &query(DataSelect::Names, 10, None));
        assert!(
            names_only
                .members
                .iter()
                .all(|m| m.value.is_none() && m.too_large.is_none()),
            "{names_only:?}"
        );
        let values = page(Some(&value), &query(DataSelect::Values, 10, None));
        let shapes: Vec<(bool, bool)> = values
            .members
            .iter()
            .map(|m| (m.value.is_some(), m.too_large == Some(true)))
            .collect();
        // "big" is 32 KiB of text plus two quotes: over its bound.
        assert_eq!(shapes, [(false, true), (true, false)]);
    }

    #[test]
    fn a_member_at_its_bound_is_sent_and_one_byte_over_is_not() {
        // The canonical text of a string is its length plus two quotes.
        let at = "x".repeat(DATA_MEMBER_MAX_BYTES - 2);
        let over = "x".repeat(DATA_MEMBER_MAX_BYTES - 1);
        let value = object(&format!(r#"{{"at":"{at}","over":"{over}"}}"#));
        let values = page(Some(&value), &query(DataSelect::Values, 10, None));
        assert_eq!(values.members[0].too_large, None);
        assert!(values.members[0].value.is_some());
        assert_eq!(values.members[1].too_large, Some(true));
    }

    #[test]
    fn a_page_stops_before_the_byte_bound_and_still_holds_one_member() {
        // Nine members of about 30 KiB each cannot all fit 256 KiB.
        let text = "y".repeat(30_000);
        let members: Vec<String> = (0..9).map(|n| format!(r#""m{n}":"{text}""#)).collect();
        let value = object(&format!("{{{}}}", members.join(",")));
        let first = page(Some(&value), &query(DataSelect::Values, 100, None));
        assert!(first.members.len() < 9 && !first.members.is_empty());
        let last = first.members.last().map(|m| m.name.clone());
        assert_eq!(first.next_after, last);
        let total: usize = first
            .members
            .iter()
            .map(|m| m.name.len() + m.value.as_ref().map_or(0, |v| v.get().len()))
            .sum();
        assert!(total <= DATA_PAGE_MAX_BYTES, "page is {total} bytes");
    }

    #[test]
    fn a_missing_path_or_a_value_that_is_not_an_object_has_no_members() {
        let scalar = object("[1,2]");
        for found in [None, Some(&scalar)] {
            let empty = page(found, &query(DataSelect::Names, 5, None));
            assert_eq!(
                empty,
                Page {
                    exists: false,
                    members: Vec::new(),
                    next_after: None
                }
            );
        }
    }

    #[test]
    fn the_query_is_validated_up_front() {
        let ok = |q: DataMembersQuery| MembersQuery::validate(q).is_ok();
        let base = DataMembersQuery::default();
        assert!(ok(base.clone()));
        for limit in [0, 101] {
            assert!(
                !ok(DataMembersQuery {
                    limit: Some(limit),
                    ..base.clone()
                }),
                "limit {limit}"
            );
        }
        for limit in [1, 100] {
            assert!(
                ok(DataMembersQuery {
                    limit: Some(limit),
                    ..base.clone()
                }),
                "limit {limit}"
            );
        }
        assert!(!ok(DataMembersQuery {
            after: Some(String::new()),
            ..base.clone()
        }));
        assert!(!ok(DataMembersQuery {
            after: Some("a".repeat(DATA_MEMBER_NAME_MAX_BYTES + 1)),
            ..base.clone()
        }));
        assert!(ok(DataMembersQuery {
            after: Some("a".repeat(DATA_MEMBER_NAME_MAX_BYTES)),
            ..base.clone()
        }));
        assert!(!ok(DataMembersQuery {
            path: Some("a//b".into()),
            ..base
        }));
    }
}
