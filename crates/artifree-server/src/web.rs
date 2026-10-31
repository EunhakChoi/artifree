//! Production web asset serving and SPA fallback.
//!
//! Serves built web frontend assets from a directory (`dist`), disallowing
//! path traversal and non-public files, while falling back to `index.html`
//! for client-side routes.

use std::fs;
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{Method, Request, Response, StatusCode};

/// Production web assets served from a root directory.
#[derive(Clone, Debug)]
pub struct WebAssets {
    root: PathBuf,
}

impl WebAssets {
    /// Validates and initializes asset serving from the given root directory.
    ///
    /// # Errors
    /// Returns an error message if the directory does not exist or lacks an `index.html` file.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = root.as_ref();
        let canonical = root.canonicalize().map_err(|e| {
            format!(
                "web directory {} could not be resolved: {e}",
                root.display()
            )
        })?;

        if !canonical.is_dir() {
            return Err(format!(
                "web directory {} is not a directory",
                root.display()
            ));
        }

        let index = canonical.join("index.html");
        if !index.is_file() {
            return Err(format!(
                "web directory {} must contain an index.html file",
                root.display()
            ));
        }

        Ok(Self { root: canonical })
    }

    /// Serves a request for static assets or falls back to `index.html` for client routes.
    #[must_use]
    pub fn handle(&self, req: &Request<Body>) -> Response<Body> {
        if req.method() != Method::GET && req.method() != Method::HEAD {
            let mut headers = HeaderMap::new();
            headers.insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                headers,
                Body::from("405 Method Not Allowed"),
            )
                .into_response();
        }

        let raw_path = req.uri().path();
        let trimmed = raw_path.trim_start_matches('/');

        // Reject forbidden path segments or traversal attempts
        if trimmed.split('/').any(|segment| {
            segment == ".." || segment == "." || segment.starts_with('.') || segment.contains('\\')
        }) {
            return (StatusCode::NOT_FOUND, Body::from("404 Not Found")).into_response();
        }

        // Subpath mapping: if a route like /artifacts/assets/index.js is requested,
        // map it to assets/index.js relative to the root.
        let effective_relative = if let Some(idx) = trimmed.find("assets/") {
            &trimmed[idx..]
        } else {
            trimmed
        };

        if !effective_relative.is_empty() {
            let candidate = self.root.join(effective_relative);
            // Verify safe resolution within root
            if let Ok(canon_candidate) = candidate.canonicalize()
                && canon_candidate.starts_with(&self.root)
                && canon_candidate.is_file()
            {
                return self.serve_file(&canon_candidate, req.method());
            }

            // If a specific file with an extension was requested and does not exist, return 404
            if has_file_extension(effective_relative) {
                return (StatusCode::NOT_FOUND, Body::from("404 Not Found")).into_response();
            }
        }

        // Fall back to index.html for client SPA routes
        let index_path = self.root.join("index.html");
        self.serve_file(&index_path, req.method())
    }

    fn serve_file(&self, path: &Path, method: &Method) -> Response<Body> {
        let content_type = mime_for_path(path);
        let cache_control = if path.file_name().and_then(|n| n.to_str()) == Some("index.html") {
            "no-cache"
        } else if path.starts_with(self.root.join("assets")) {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=3600"
        };

        let mut headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(content_type) {
            headers.insert(header::CONTENT_TYPE, val);
        }
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );

        if method == Method::HEAD {
            return (StatusCode::OK, headers, Body::empty()).into_response();
        }

        match fs::read(path) {
            Ok(bytes) => (StatusCode::OK, headers, Body::from(bytes)).into_response(),
            Err(_) => (StatusCode::NOT_FOUND, Body::from("404 Not Found")).into_response(),
        }
    }
}

fn has_file_extension(path: &str) -> bool {
    Path::new(path)
        .components()
        .next_back()
        .and_then(|c| match c {
            Component::Normal(os) => os.to_str(),
            _ => None,
        })
        .is_some_and(|filename| filename.contains('.'))
}

fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

use axum::response::IntoResponse;
