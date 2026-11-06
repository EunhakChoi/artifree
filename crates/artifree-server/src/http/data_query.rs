//! Strict wire-byte checks before the existing query shape parser (0046).

use axum::extract::Query;
use axum::http::Uri;
use serde::de::DeserializeOwned;

use crate::operations::ApiError;

/// Reject malformed percent escapes and invalid UTF-8 before the form parser
/// can replace them. Validation never feeds decoded text back into the parser:
/// its only input is the original URI, so `%252F` remains `%2F` as data.
pub(super) fn parse<T: DeserializeOwned>(uri: &Uri) -> Result<T, ApiError> {
    let query = uri.query().unwrap_or_default().as_bytes();
    let mut decoded = Vec::with_capacity(query.len());
    let mut offset = 0;
    while offset < query.len() {
        let byte = query[offset];
        if byte == b'%' {
            let high = query.get(offset + 1).copied().and_then(hex);
            let low = query.get(offset + 2).copied().and_then(hex);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(ApiError::invalid(
                    "query contains a malformed percent escape",
                ));
            };
            decoded.push(high * 16 + low);
            offset += 3;
        } else {
            decoded.push(byte);
            offset += 1;
        }
    }
    if std::str::from_utf8(&decoded).is_err() {
        return Err(ApiError::invalid("query is not valid UTF-8"));
    }
    Query::<T>::try_from_uri(uri)
        .map(|Query(value)| value)
        .map_err(|_| ApiError::invalid("query does not match the data request"))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
