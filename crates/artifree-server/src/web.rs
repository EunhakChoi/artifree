//! Production web assets serving with immutable asset caching and SPA fallback.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::header::{self, HeaderValue};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};

/// Error validating production web directory.
#[derive(Debug, Clone)]
pub struct WebAssetsError(String);

impl std::fmt::Display for WebAssetsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for WebAssetsError {}

/// Serves static web assets from a production build directory (`apps/web/dist`).
#[derive(Debug, Clone)]
pub struct WebAssets {
    web_dir: PathBuf,
}

impl WebAssets {
    /// Validates and initializes production web asset serving from `web_dir`.
    ///
    /// # Errors
    ///
    /// Returns `WebAssetsError` if `web_dir` does not exist, is not a directory,
    /// or does not contain `index.html`.
    pub fn new(web_dir: &Path) -> Result<Self, WebAssetsError> {
        if !web_dir.exists() {
            return Err(WebAssetsError(format!(
                "web directory does not exist: {}",
                web_dir.display()
            )));
        }
        if !web_dir.is_dir() {
            return Err(WebAssetsError(format!(
                "web directory is not a directory: {}",
                web_dir.display()
            )));
        }
        let index_path = web_dir.join("index.html");
        if !index_path.is_file() {
            return Err(WebAssetsError(format!(
                "web directory missing index.html: {}",
                web_dir.display()
            )));
        }
        let canonical_dir = web_dir.canonicalize().map_err(|error| {
            WebAssetsError(format!(
                "cannot canonicalize web directory {}: {error}",
                web_dir.display()
            ))
        })?;
        Ok(Self {
            web_dir: canonical_dir,
        })
    }

    /// Handles an incoming HTTP request for web assets.
    pub fn handle(&self, req: &Request<Body>) -> Response {
        let method = req.method();
        let is_head = match *method {
            Method::GET => false,
            Method::HEAD => true,
            _ => {
                let mut res =
                    (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed").into_response();
                res.headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
                return res;
            }
        };

        let raw_path = req.uri().path();
        let Some(decoded_path) = percent_decode(raw_path) else {
            return (StatusCode::BAD_REQUEST, "Bad Request").into_response();
        };

        let trimmed = decoded_path.trim_start_matches('/');

        // Reject null bytes
        if trimmed.contains('\0') {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }

        let relative_path = Path::new(trimmed);
        for comp in relative_path.components() {
            match comp {
                std::path::Component::Normal(_) | std::path::Component::CurDir => {}
                _ => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
            }
        }

        let has_extension = relative_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| !ext.is_empty());

        if trimmed.is_empty() || trimmed == "index.html" {
            let index_path = self.web_dir.join("index.html");
            return serve_file(&index_path, "text/html; charset=utf-8", false, is_head);
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

        // Defense in depth for nested subpath asset requests (e.g. /artifacts/assets/file.js)
        if let Some(assets_pos) = trimmed.find("assets/") {
            let asset_suffix = &trimmed[assets_pos..];
            let alt_target = self.web_dir.join(asset_suffix);
            if let Ok(canonical_alt) = alt_target.canonicalize()
                && canonical_alt.starts_with(&self.web_dir)
                && canonical_alt.is_file()
            {
                let mime = mime_type(Path::new(asset_suffix));
                return serve_file(&canonical_alt, mime, true, is_head);
            }
        }

        // Missing asset with extension returns 404 rather than SPA fallback
        if has_extension {
            return (StatusCode::NOT_FOUND, "Not Found").into_response();
        }

        // SPA fallback for client-side routes without extensions
        let index_path = self.web_dir.join("index.html");
        serve_file(&index_path, "text/html; charset=utf-8", false, is_head)
    }
}

fn serve_file(path: &Path, mime: &'static str, is_immutable: bool, is_head: bool) -> Response {
    let Ok(content) = std::fs::read(path) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };

    let cache_control = if is_immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, content.len().to_string())
        .header(header::CACHE_CONTROL, cache_control);

    if is_head {
        builder.body(Body::empty()).unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Error").into_response()
        })
    } else {
        builder.body(Body::from(content)).unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Error").into_response()
        })
    }
}

fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..=i + 2]).ok()?;
            let val = u8::from_str_radix(hex, 16).ok()?;
            decoded.push(val);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}
