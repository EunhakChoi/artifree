//! Production web asset and SPA fallback serving.
//!
//! When enabled with `--web-dir <dir>`, the server provides the production web
//! workspace and its assets on the API listener without requiring a Vite
//! development server. API routes (`/v1/...`) and the MCP endpoint (`/mcp`)
//! retain their strict authentication, rate limits, and JSON error framing,
//! completely separated from web assets and SPA fallback.

use std::fs;
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};

/// Serves static assets and SPA fallback from a local directory.
#[derive(Debug, Clone)]
pub struct WebAssets {
    web_dir: PathBuf,
}

impl WebAssets {
    /// Validates and initializes asset serving from `path`.
    ///
    /// # Errors
    ///
    /// When `path` does not exist, is not a directory, is missing `index.html`,
    /// or cannot be canonicalized.
    pub fn try_new(path: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = path.into();
        if !dir.exists() {
            return Err(format!("web directory {}: does not exist", dir.display()));
        }
        if !dir.is_dir() {
            return Err(format!(
                "web directory {}: is not a directory",
                dir.display()
            ));
        }
        let index_file = dir.join("index.html");
        if !index_file.is_file() {
            return Err(format!(
                "web directory {}: is missing index.html",
                dir.display()
            ));
        }
        let canonical = dir.canonicalize().map_err(|error| {
            format!(
                "cannot canonicalize web directory {}: {error}",
                dir.display()
            )
        })?;
        Ok(Self { web_dir: canonical })
    }

    /// Canonical path to the root web assets directory.
    #[must_use]
    pub fn web_dir(&self) -> &Path {
        &self.web_dir
    }

    /// Serves static files or SPA fallback for a request.
    ///
    /// Only `GET` and `HEAD` methods are accepted. Any request containing
    /// path traversal sequences (`..`), null bytes, or attempting to escape
    /// `web_dir` is rejected with an error response.
    #[must_use]
    pub fn serve(&self, method: &Method, uri_path: &str) -> Response {
        if *method != Method::GET && *method != Method::HEAD {
            let mut response =
                (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed").into_response();
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return response;
        }

        let is_head = *method == Method::HEAD;
        let Some(decoded) = percent_decode(uri_path) else {
            return (StatusCode::BAD_REQUEST, "Invalid URL encoding").into_response();
        };

        if decoded.contains('\0') {
            return (StatusCode::BAD_REQUEST, "Null bytes forbidden").into_response();
        }

        let trimmed = decoded.trim_start_matches('/');
        let relative_path = Path::new(trimmed);

        for component in relative_path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return (StatusCode::NOT_FOUND, "Not Found").into_response();
                }
            }
        }

        let has_extension = relative_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| !ext.is_empty());

        if trimmed.is_empty() {
            return serve_file(
                &self.web_dir.join("index.html"),
                "text/html; charset=utf-8",
                false,
                is_head,
            );
        }

        let target = self.web_dir.join(relative_path);

        if let Ok(canonical_target) = target.canonicalize() {
            if !canonical_target.starts_with(&self.web_dir) {
                return (StatusCode::NOT_FOUND, "Not Found").into_response();
            }
            if canonical_target.is_file() {
                let mime = mime_type(relative_path);
                let is_immutable_asset = trimmed.starts_with("assets/");
                return serve_file(&canonical_target, mime, is_immutable_asset, is_head);
            }
        }

        if has_extension {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }

        let index_path = self.web_dir.join("index.html");
        serve_file(&index_path, "text/html; charset=utf-8", false, is_head)
    }
}

fn serve_file(path: &Path, content_type: &str, immutable: bool, is_head: bool) -> Response {
    let Ok(content) = fs::read(path) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };

    let mut response = if is_head {
        (StatusCode::OK, Body::empty()).into_response()
    } else {
        (StatusCode::OK, Body::from(content)).into_response()
    };

    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );

    if immutable {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }

    response
}

/// Decodes percent-encoded UTF-8 bytes in a URI path.
fn percent_decode(input: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let high = chars.next()?;
            let low = chars.next()?;
            let val = hex_pair_to_byte(high, low)?;
            bytes.push(val);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

fn hex_pair_to_byte(high: u8, low: u8) -> Option<u8> {
    let h = hex_val(high)?;
    let l = hex_val(low)?;
    Some((h << 4) | l)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Determines the MIME content type for a given path.
fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_works() {
        assert_eq!(
            percent_decode("/assets/%2e%2e/test"),
            Some("/assets/../test".into())
        );
        assert_eq!(
            percent_decode("/hello%20world.js"),
            Some("/hello world.js".into())
        );
        assert_eq!(percent_decode("/invalid%2"), None);
        assert_eq!(percent_decode("/invalid%zz"), None);
    }

    #[test]
    fn mime_type_mapping() {
        assert_eq!(
            mime_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_type(Path::new("bundle.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_type(Path::new("style.css")), "text/css; charset=utf-8");
        assert_eq!(mime_type(Path::new("data.json")), "application/json");
        assert_eq!(mime_type(Path::new("logo.svg")), "image/svg+xml");
        assert_eq!(
            mime_type(Path::new("unknown.xyz")),
            "application/octet-stream"
        );
    }
}
