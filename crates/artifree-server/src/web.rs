//! Production web asset serving: static files from `apps/web/dist` and SPA
//! client route fallback.

use std::fs;
use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::header::{self, HeaderValue};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};

/// Production web assets served alongside the API.
#[derive(Debug, Clone)]
pub struct WebAssets {
    root: PathBuf,
    canonical_index: PathBuf,
}

impl WebAssets {
    /// Inspects and prepares the web directory for serving.
    ///
    /// # Errors
    ///
    /// When the directory does not exist, `index.html` is missing, or `index.html`
    /// resolves outside the web directory root via a symlink.
    pub fn new(root: &Path) -> Result<Self, String> {
        let canonical_root = fs::canonicalize(root)
            .map_err(|error| format!("cannot resolve web directory {}: {error}", root.display()))?;
        let index_path = canonical_root.join("index.html");
        let canonical_index = fs::canonicalize(&index_path).map_err(|error| {
            format!(
                "cannot find index.html in {}: {error}",
                canonical_root.display()
            )
        })?;

        if !canonical_index.starts_with(&canonical_root) {
            return Err(format!(
                "index.html {} escapes web directory {}",
                canonical_index.display(),
                canonical_root.display()
            ));
        }

        Ok(Self {
            root: canonical_root,
            canonical_index,
        })
    }

    /// Serves a static file from the web directory or falls back to `index.html`
    /// for SPA client routes.
    #[must_use]
    pub fn handle(&self, req: &Request<Body>) -> Response {
        if req.method() != Method::GET && req.method() != Method::HEAD {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }

        let is_head = req.method() == Method::HEAD;
        let path = req.uri().path();

        // 1. Root index.html request
        if path == "/" || path.is_empty() {
            return serve_file(
                &self.canonical_index,
                "text/html; charset=utf-8",
                false,
                is_head,
            );
        }

        // 2. Safe relative path traversal check
        let trimmed = path.trim_start_matches('/');
        if let Some(target) = self.safe_resolve(trimmed).filter(|p| p.is_file()) {
            let content_type = mime_for_path(&target);
            let is_immutable = trimmed.starts_with("assets/");
            return serve_file(&target, content_type, is_immutable, is_head);
        }

        // 3. Subpath asset recovery: handle nested SPA routes requesting assets
        if let Some(assets_idx) = path.find("/assets/") {
            let asset_subpath = &path[assets_idx + 1..];
            if let Some(target) = self.safe_resolve(asset_subpath).filter(|p| p.is_file()) {
                let content_type = mime_for_path(&target);
                return serve_file(&target, content_type, true, is_head);
            }
        }

        // 4. SPA client route fallback: serve index.html for client routes,
        // excluding paths that look like missing static assets or system paths.
        if is_spa_route(path)
            && fs::canonicalize(&self.canonical_index)
                .is_ok_and(|canon| canon.starts_with(&self.root))
        {
            return serve_file(
                &self.canonical_index,
                "text/html; charset=utf-8",
                false,
                is_head,
            );
        }

        StatusCode::NOT_FOUND.into_response()
    }

    /// Resolves a relative path within the root, forbidding `..` and symlink escapes.
    fn safe_resolve(&self, relative: &str) -> Option<PathBuf> {
        let path = Path::new(relative);
        for component in path.components() {
            match component {
                Component::Normal(_) => {}
                _ => return None,
            }
        }
        let full = self.root.join(path);
        let canonical = fs::canonicalize(&full).ok()?;
        if canonical.starts_with(&self.root) {
            Some(canonical)
        } else {
            None
        }
    }
}

/// Reads a file and returns an HTTP response with appropriate caching headers.
fn serve_file(path: &Path, content_type: &'static str, immutable: bool, is_head: bool) -> Response {
    let Ok(contents) = fs::read(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let cache_control = if immutable {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    } else {
        HeaderValue::from_static("no-cache")
    };

    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, contents.len().to_string())
        .header(header::CACHE_CONTROL, cache_control);

    if is_head {
        builder
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        builder
            .body(Body::from(contents))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
    }
}

/// Identifies whether an unmatched path represents an SPA client route.
fn is_spa_route(path: &str) -> bool {
    if path.starts_with("/v1/") || path == "/v1" || path.starts_with("/mcp") || path == "/healthz" {
        return false;
    }
    // Requests with a file extension in the last path segment are asset requests, not SPA routes.
    let last_segment = path.rsplit('/').next().unwrap_or(path);
    !last_segment.contains('.')
}

/// Determines the MIME content type from a file's extension.
fn mime_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
