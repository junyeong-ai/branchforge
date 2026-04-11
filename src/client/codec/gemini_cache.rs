//! Stateless helpers for the Gemini `cachedContents` API.
//!
//! Gemini's prompt caching is a **separate resource lifecycle**: a
//! client POSTs to `/cachedContents` to mint a cache handle (name),
//! then references that handle in subsequent `generateContent` calls
//! via the `cachedContent` field. This is fundamentally different
//! from Anthropic/OpenAI inline `cache_control` markers and cannot
//! be represented as a per-message IR annotation.
//!
//! Rather than tying the cache lifecycle to a particular transport
//! (Direct vs Vertex) or HTTP client, this module exposes **pure
//! request/response shaping helpers**: callers feed them into their
//! own `reqwest::Client` (or any HTTP layer that handles auth) and
//! then plumb the returned cache name back into
//! [`crate::ir::GeminiOptions::cached_content`] on subsequent
//! requests.
//!
//! # Example flow
//!
//! ```ignore
//! use branchforge::client::codec::gemini_cache::*;
//! use serde_json::json;
//!
//! // 1. Build the create-cache request body.
//! let req = build_create_cache_body(CreateCacheParams {
//!     model: "models/gemini-2.5-flash",
//!     system_instruction: Some("You are a senior code reviewer."),
//!     contents: vec![json!({"role":"user","parts":[{"text":"<long doc>"}]})],
//!     ttl_seconds: Some(3600),
//!     display_name: Some("code-review-context"),
//! });
//!
//! // 2. POST it via your HTTP client (auth is the caller's responsibility).
//! // let response = http.post(url).json(&req).send().await?.json::<Value>().await?;
//!
//! // 3. Extract the cache name and use it in subsequent IR requests.
//! // let name = parse_cache_name(&response)?;
//! // request.provider_options.gemini = Some(GeminiOptions {
//! //     cached_content: Some(name),
//! //     ..Default::default()
//! // });
//! ```

use serde_json::{Value, json};

use crate::Result;

/// Parameters for [`build_create_cache_body`].
#[derive(Debug, Clone)]
pub struct CreateCacheParams<'a> {
    /// Fully-qualified model id (e.g. `"models/gemini-2.5-flash"`).
    /// The cache is bound to one model.
    pub model: &'a str,
    /// Optional system instruction text. Becomes a `systemInstruction`
    /// block on the wire.
    pub system_instruction: Option<&'a str>,
    /// Pre-built `contents` array entries (already shaped as Gemini
    /// `Content` objects with `role` and `parts`). The caller is
    /// responsible for the inner shape because callers typically
    /// have their own message-to-Gemini conversion.
    pub contents: Vec<Value>,
    /// Cache TTL in seconds. The Gemini API encodes this as the
    /// string `"<seconds>s"` on the wire.
    pub ttl_seconds: Option<u64>,
    /// Human-readable label for the cache resource. Optional.
    pub display_name: Option<&'a str>,
}

/// Build the JSON body for `POST /v1beta/cachedContents` (Gemini Developer API)
/// or `POST /v1beta1/projects/.../cachedContents` (Vertex AI).
///
/// The wire shape is identical between the two endpoints; the
/// difference is the URL prefix and auth, which the caller handles.
pub fn build_create_cache_body(params: CreateCacheParams<'_>) -> Value {
    let mut body = json!({
        "model": params.model,
        "contents": params.contents,
    });

    if let Some(sys) = params.system_instruction {
        body["systemInstruction"] = json!({
            "parts": [{ "text": sys }]
        });
    }
    if let Some(secs) = params.ttl_seconds {
        body["ttl"] = json!(format!("{secs}s"));
    }
    if let Some(name) = params.display_name {
        body["displayName"] = json!(name);
    }

    body
}

/// Parse the cache resource name from a Gemini `cachedContents`
/// create-or-get response.
///
/// The Gemini API returns the resource name as the top-level `name`
/// field, e.g. `"cachedContents/abc123"`. Callers stash this string
/// on subsequent [`crate::ir::GeminiOptions::cached_content`] for
/// the codec to forward.
pub fn parse_cache_name(response: &Value) -> Result<String> {
    response
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            crate::Error::Parse(
                "Gemini cachedContents response missing top-level `name` field".into(),
            )
        })
}

/// Build the URL path fragment for deleting a cached content
/// resource. Concatenate with the API base + version prefix the
/// caller's transport already knows about.
///
/// Example output: `"cachedContents/abc123"`. The caller issues
/// `DELETE <base>/<this>`.
pub fn delete_cache_path(name: &str) -> String {
    // Names returned by the create endpoint are already prefixed
    // with `cachedContents/`. If the caller passed a bare id, prefix
    // it. This is forgiving by design — the Gemini API rejects bare
    // ids, so coercing here saves a round trip.
    if name.starts_with("cachedContents/") {
        name.to_owned()
    } else {
        format!("cachedContents/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_minimal_create_body() {
        let body = build_create_cache_body(CreateCacheParams {
            model: "models/gemini-2.5-flash",
            system_instruction: None,
            contents: vec![json!({"role":"user","parts":[{"text":"hi"}]})],
            ttl_seconds: None,
            display_name: None,
        });
        assert_eq!(body["model"], "models/gemini-2.5-flash");
        assert_eq!(body["contents"][0]["role"], "user");
        assert!(body.get("systemInstruction").is_none());
        assert!(body.get("ttl").is_none());
    }

    #[test]
    fn build_full_create_body_includes_all_fields() {
        let body = build_create_cache_body(CreateCacheParams {
            model: "models/gemini-2.5-pro",
            system_instruction: Some("You are helpful."),
            contents: vec![json!({"role":"user","parts":[{"text":"long doc"}]})],
            ttl_seconds: Some(3600),
            display_name: Some("my-cache"),
        });
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        assert_eq!(body["ttl"], "3600s");
        assert_eq!(body["displayName"], "my-cache");
    }

    #[test]
    fn parse_cache_name_extracts_name() {
        let response = json!({
            "name": "cachedContents/xyz789",
            "model": "models/gemini-2.5-flash",
            "createTime": "2026-04-11T12:00:00Z"
        });
        assert_eq!(
            parse_cache_name(&response).unwrap(),
            "cachedContents/xyz789"
        );
    }

    #[test]
    fn parse_cache_name_errors_on_missing_field() {
        let response = json!({"model": "models/gemini-2.5-flash"});
        assert!(parse_cache_name(&response).is_err());
    }

    #[test]
    fn delete_path_accepts_full_or_bare_name() {
        assert_eq!(
            delete_cache_path("cachedContents/abc"),
            "cachedContents/abc"
        );
        assert_eq!(delete_cache_path("abc"), "cachedContents/abc");
    }
}
