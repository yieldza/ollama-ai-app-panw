use crate::handlers::utils::{
    build_json_response, build_json_response_with_status, format_security_violation_message,
};
use crate::handlers::ApiError;
use crate::types::EmbeddingsRequest;
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::Response, Json};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

pub async fn handle_embeddings(
    State(state): State<AppState>,
    Json(request): Json<EmbeddingsRequest>,
) -> Result<Response, ApiError> {
    debug!("Received embeddings request for model: {}", request.model);

    let assessment = state
        .security_client
        .assess_content(
            &request.prompt,
            &request.model,
            true, // This is a prompt
        )
        .await?;

    if !assessment.is_safe {
        // Reject with HTTP 403 and an explicit error body. We deliberately
        // do NOT return a zero-vector under the legacy `{"embedding": [...]}`
        // shape: a fake embedding silently corrupts downstream similarity
        // calculations (cosine of a zero vector is undefined; nearest-
        // neighbor matches against it are nonsensical) and the dimensions
        // would not match the requested model. A 4xx response forces the
        // caller to handle the failure instead of feeding garbage into a
        // vector store.
        let body = serde_json::json!({
            "error": format_security_violation_message(&assessment),
            "category": assessment.category,
            "action": assessment.action,
        });
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| ApiError::InternalError(format!("serialize block: {}", e)))?;
        return build_json_response_with_status(StatusCode::FORBIDDEN, Bytes::from(bytes));
    }

    // Forward to Ollama
    let response = state
        .ollama_client
        .forward("/api/embeddings", &request)
        .await?;
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| ApiError::InternalError(e.to_string()))?;
    build_json_response(body_bytes)
}

/// Request shape for the newer `POST /api/embed` endpoint, where `input`
/// can be a single string or an array of strings.
#[derive(Debug, Deserialize, Serialize)]
pub struct EmbedRequest {
    pub model: String,
    /// Either a JSON string or a JSON array of strings, per the Ollama API.
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<Value>,
}

/// Returns every input string as a flat `Vec<String>` so a security scan can
/// be applied to all inputs at once. Unrecognized JSON shapes degrade to an
/// empty vec; the request still forwards to Ollama and returns its native
/// error.
fn flatten_inputs(input: &Value) -> Vec<String> {
    match input {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Handler for the newer `POST /api/embed` endpoint.
///
/// Differs from `/api/embeddings` (legacy) by accepting an `input` field that
/// may be a single string or an array of strings, and returning
/// `{"embeddings": [[...]]}`.
///
/// Security model: the concatenated inputs are submitted to PANW for prompt
/// scanning. On a block we return an empty `embeddings` array along with the
/// original model name so a client library doesn't crash on missing fields.
pub async fn handle_embed(
    State(state): State<AppState>,
    Json(request): Json<EmbedRequest>,
) -> Result<Response, ApiError> {
    debug!("Received /api/embed request for model: {}", request.model);

    let inputs = flatten_inputs(&request.input);
    let joined = inputs.join("\n");

    let assessment = state
        .security_client
        .assess_content(&joined, &request.model, true)
        .await?;

    if !assessment.is_safe {
        // See handle_embeddings above for why we return 403 instead of an
        // empty-vector body. Same reasoning applies to /api/embed: an empty
        // `embeddings: []` would let a caller silently insert zero matches
        // into a vector store without realizing the input was blocked.
        let body = serde_json::json!({
            "error": format_security_violation_message(&assessment),
            "category": assessment.category,
            "action": assessment.action,
            "model": request.model,
        });
        let bytes = serde_json::to_vec(&body)
            .map_err(|e| ApiError::InternalError(format!("serialize block: {}", e)))?;
        return build_json_response_with_status(StatusCode::FORBIDDEN, Bytes::from(bytes));
    }

    let response = state
        .ollama_client
        .forward("/api/embed", &request)
        .await?;
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| ApiError::InternalError(e.to_string()))?;
    build_json_response(body_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flatten_string_input() {
        let v = json!("hello");
        assert_eq!(flatten_inputs(&v), vec!["hello".to_string()]);
    }

    #[test]
    fn flatten_array_input() {
        let v = json!(["a", "b", "c"]);
        assert_eq!(
            flatten_inputs(&v),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn flatten_unrecognized_shape_returns_empty() {
        let v = json!({"oops": 1});
        assert!(flatten_inputs(&v).is_empty());
    }
}
