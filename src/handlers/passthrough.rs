//! Catch-all fallback handler.
//!
//! Forwards any request whose path is not handled by an explicit route to
//! upstream Ollama as a raw HTTP passthrough. The proxy applies **no**
//! security scanning to these requests; they exist so transparent
//! compatibility with future Ollama API additions (model pulls, blob
//! uploads, /api/tags, etc.) is not gated on this proxy adding explicit
//! support.
//!
//! ## Security note for operators
//!
//! Anything that flows through this fallback is **not scanned by PANW AIRS**.
//! When you onboard a new Ollama endpoint where prompt or response content
//! must be assessed, add an explicit handler that calls
//! `state.security_client.assess_content(...)` before forwarding.
//!
//! ### Known prompt-bearing compatibility shims are blocked by default
//!
//! The OpenAI / Anthropic compatibility paths (`/v1/chat/completions`,
//! `/v1/completions`, `/v1/messages`, `/v1/embeddings`) carry user prompts
//! and model responses, so allowing them to passthrough unscanned would
//! defeat the proxy's purpose. They are **rejected with 501** by default.
//! Operators who explicitly accept the risk (e.g. internal-only deployment
//! where a different control owns scanning) may set
//! `PASSTHROUGH_COMPAT_UNSAFE=1` to allow them.
//!
//! The whole fallback can be disabled at runtime by setting
//! `PASSTHROUGH_DISABLED=1`, in which case all unhandled paths return `404`.

use axum::{
    body::Body,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, Response, StatusCode},
};
use bytes::Bytes;
use http_body_util::{BodyExt, Limited};
use tracing::{info, warn};

use crate::AppState;

/// Body size limit for the passthrough fallback (200 MiB). Larger than
/// scanned routes because `/api/blobs/:digest` is used to upload GGUF
/// model files which can be hundreds of MiB. Still bounded — without
/// this, the catch-all body has no cap, which lets a client OOM the
/// proxy by streaming a multi-GB payload.
const PASSTHROUGH_BODY_LIMIT: usize = 200 * 1024 * 1024;

/// Returns true when the operator has explicitly opted out of passthrough.
fn passthrough_disabled() -> bool {
    matches!(
        std::env::var("PASSTHROUGH_DISABLED").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Returns true when the operator has explicitly opted IN to forwarding the
/// known prompt-bearing OpenAI/Anthropic compat shims without scanning.
fn passthrough_compat_unsafe_enabled() -> bool {
    matches!(
        std::env::var("PASSTHROUGH_COMPAT_UNSAFE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Returns true when `path` is one of the known OpenAI / Anthropic
/// compatibility endpoints that carries user prompts or model responses.
/// These MUST NOT be unscanned-forwarded by default — see module doc.
fn is_unsafe_compat_path(path: &str) -> bool {
    // Match exact paths so unrelated prefixes (e.g. /v1/models) still pass.
    matches!(
        path,
        "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/messages"
            | "/v1/embeddings"
    )
}

/// Strips per-hop response headers that must not be forwarded as-is.
fn sanitize_response_headers(src: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut dst = HeaderMap::new();
    for (name, value) in src.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if matches!(
            n.as_str(),
            "connection"
                | "transfer-encoding"
                | "content-length"
                | "upgrade"
                | "proxy-connection"
                | "keep-alive"
        ) {
            continue;
        }
        if let Ok(name) = axum::http::HeaderName::try_from(name.as_str()) {
            if let Ok(value) = axum::http::HeaderValue::try_from(value.as_bytes()) {
                dst.append(name, value);
            }
        }
    }
    dst
}

/// Catch-all fallback handler. Mounted via `Router::fallback`.
pub async fn passthrough(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    if passthrough_disabled() {
        warn!(
            "Passthrough disabled (PASSTHROUGH_DISABLED=1); rejecting {} {}",
            method, uri
        );
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Not Found"))
            .expect("build 404 response");
    }

    // Block known prompt-bearing compat shims unless operator opted in.
    // Without this guard a client could send malicious prompts to
    // /v1/chat/completions and bypass PANW scanning entirely.
    if is_unsafe_compat_path(uri.path()) && !passthrough_compat_unsafe_enabled() {
        warn!(
            "Blocking unscanned compat passthrough for {} {} (set PASSTHROUGH_COMPAT_UNSAFE=1 to allow)",
            method, uri
        );
        return Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"error":"This OpenAI/Anthropic compat path is not scanned by PANW AIRS and is blocked by default. Use the native Ollama endpoints (/api/chat, /api/generate, /api/embed) which are scanned, or set PASSTHROUGH_COMPAT_UNSAFE=1 to accept the risk."}"#,
            ))
            .expect("build 501 response");
    }

    info!(
        "Passthrough (no scan) {} {} -> upstream Ollama",
        method, uri
    );

    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| uri.path().to_string());

    // Collect the request body up to PASSTHROUGH_BODY_LIMIT. Limited wraps
    // the inner Body and returns an error from `.collect()` as soon as more
    // than the cap has been read — without this, a client could stream a
    // multi-GB body and OOM the proxy.
    let body_bytes: Bytes = match Limited::new(body, PASSTHROUGH_BODY_LIMIT).collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            // Limited error is a boxed dyn Error; treat any failure here as
            // payload-too-large to be safe (could also be a network read err,
            // but we don't want to leak server internals).
            warn!(
                "Passthrough body read failed or exceeded {} byte cap: {}",
                PASSTHROUGH_BODY_LIMIT, e
            );
            return Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .body(Body::from("Request body too large or unreadable"))
                .expect("build 413 response");
        }
    };

    // Convert axum HeaderMap into reqwest HeaderMap.
    let mut upstream_headers = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if let Ok(n) = reqwest::header::HeaderName::try_from(name.as_str()) {
            if let Ok(v) = reqwest::header::HeaderValue::try_from(value.as_bytes()) {
                upstream_headers.append(n, v);
            }
        }
    }

    // Convert axum Method to reqwest Method via byte string.
    let upstream_method = match reqwest::Method::from_bytes(method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::from("Method Not Allowed"))
                .expect("build 405 response");
        }
    };

    let upstream = match state
        .ollama_client
        .forward_raw(upstream_method, &path_and_query, upstream_headers, body_bytes)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("Passthrough upstream error: {}", e);
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from("Upstream Ollama unavailable."))
                .expect("build 502 response");
        }
    };

    let status = match StatusCode::from_u16(upstream.status().as_u16()) {
        Ok(s) => s,
        Err(_) => StatusCode::BAD_GATEWAY,
    };
    let resp_headers = sanitize_response_headers(upstream.headers());

    // Stream the upstream body back to the client without buffering. This
    // matters for /v1/chat/completions SSE streams and large /api/blobs
    // downloads.
    let stream = upstream.bytes_stream();
    let body = Body::from_stream(stream);

    let mut builder = Response::builder().status(status);
    if let Some(h) = builder.headers_mut() {
        *h = resp_headers;
    }
    builder.body(body).unwrap_or_else(|_| {
        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from("Failed to construct upstream response"))
            .expect("build 502 fallback")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_openai_anthropic_paths_are_unsafe() {
        assert!(is_unsafe_compat_path("/v1/chat/completions"));
        assert!(is_unsafe_compat_path("/v1/completions"));
        assert!(is_unsafe_compat_path("/v1/messages"));
        assert!(is_unsafe_compat_path("/v1/embeddings"));
    }

    #[test]
    fn benign_paths_are_allowed() {
        // Model-management & non-prompt paths must still passthrough freely.
        assert!(!is_unsafe_compat_path("/v1/models"));
        assert!(!is_unsafe_compat_path("/api/tags"));
        assert!(!is_unsafe_compat_path("/api/show"));
        assert!(!is_unsafe_compat_path("/api/blobs/sha256-abc"));
        assert!(!is_unsafe_compat_path("/"));
    }

    #[test]
    fn paths_must_match_exactly_not_by_prefix() {
        // Defense against bypass like /v1/chat/completions/extra
        assert!(!is_unsafe_compat_path("/v1/chat/completions/extra"));
        assert!(!is_unsafe_compat_path("/v1/messages/foo"));
    }
}
