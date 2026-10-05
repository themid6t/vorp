//! The dashboard's static files: the Vite build in `ui/dist`, embedded in the
//! binary. Debug builds read the folder from disk, so a rebuilt UI shows up
//! without recompiling the relay.

use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

/// A checkout without `npm run build` still compiles; `/` then explains why
/// there is no dashboard instead of failing the Rust build.
#[derive(Embed)]
#[folder = "ui/dist"]
#[allow_missing = true]
struct Dist;

/// Scripts, styles and fonts load only from this origin, so injected markup
/// cannot run inline code. `data:` covers images inlined by the build.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     font-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'none'";

const NOT_BUILT: &str = "<!doctype html><title>vorp</title>\
     <p>The dashboard was not built into this binary. Run <code>npm ci &amp;&amp; \
     npm run build</code> in <code>crates/web/ui</code>, then rebuild vorp.</p>";

pub(crate) async fn index() -> Response {
    match Dist::get("index.html") {
        Some(file) => respond(
            "text/html; charset=utf-8",
            "no-cache",
            file.data.into_owned(),
        ),
        None => respond("text/html; charset=utf-8", "no-cache", NOT_BUILT.into()),
    }
}

/// Any other dashboard file. Names under `assets/` carry a content hash, so a
/// browser may keep them forever; everything else is revalidated.
pub(crate) async fn file(Path(path): Path<String>) -> Response {
    let Some(file) = Dist::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mime = file.metadata.mimetype().to_owned();
    respond(&mime, cache, file.data.into_owned())
}

fn respond(content_type: &str, cache: &'static str, body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    fn router() -> Router {
        Router::new()
            .route("/", get(index))
            .route("/{*path}", get(file))
    }

    async fn fetch(path: &str) -> Response {
        router()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    #[tokio::test]
    async fn index_is_html_with_csp_built_or_not() {
        let response = fetch("/").await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .expect("ascii")
                .starts_with("text/html")
        );
        assert!(
            headers[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .expect("ascii")
                .contains("script-src 'self'")
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
    }

    #[tokio::test]
    async fn unknown_and_escaping_paths_are_not_found() {
        for path in [
            "/assets/missing.js",
            "/../Cargo.toml",
            "/assets/../../Cargo.toml",
        ] {
            assert_eq!(fetch(path).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    /// Only meaningful once `npm run build` has produced `ui/dist`; CI builds it first.
    #[tokio::test]
    async fn built_assets_are_immutable_with_csp() {
        let Some(path) = Dist::iter().find(|p| p.starts_with("assets/")) else {
            return;
        };
        let response = fetch(&format!("/{path}")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::CACHE_CONTROL]
                .to_str()
                .expect("ascii")
                .contains("immutable")
        );
        assert!(
            response
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
    }
}
