//! Provider-specific prompt cache lifecycles.
//!
//! Most providers (Anthropic, OpenAI) use **inline** cache markers on
//! individual messages — the cache lifecycle is implicit and the codec
//! layer handles it via `cache_control` annotations on the IR. Gemini
//! is different: it has a **separate resource** (`cachedContents/<id>`)
//! that must be minted ahead of time, then referenced by name on
//! subsequent `generateContent` calls. That lifecycle lives here, not
//! in the codec, because it is stateful and involves dedicated HTTP
//! endpoints outside the main inference flow.

pub mod gemini;

pub use gemini::{
    CreateCacheParams, GeminiCacheAuth, GeminiCacheClient, build_create_cache_body,
    delete_cache_path, parse_cache_name,
};
