//! `AgentContract<T>` — typed input/output contracts for subagent
//! invocations.
//!
//! The built-in [`super::TaskInput`] / [`super::TaskOutput`] surface is
//! string-in / `Value`-out: `prompt: String` becomes the user message
//! and `structured_output: Option<Value>` carries the result. That is
//! convenient for free-form delegation but useless for callers who
//! already have a typed schema for what they want the subagent to
//! produce.
//!
//! `AgentContract` lets a caller declare a typed input + typed output
//! pair and invoke a subagent against it without giving up Rust's
//! type system at the boundary. The trait is intentionally
//! **data-only** — it carries no methods. The actual dispatch lives
//! on [`TypedAgentInvoker`], which thinly wraps an existing
//! `TaskTool` invocation: the typed input is serialised to JSON and
//! sent as the prompt body, and the response is deserialised from
//! the subagent's `structured_output` (or its concatenated text, as
//! a fallback) back into the contract's output type.
//!
//! # Example
//!
//! ```ignore
//! use branchforge::agent::{AgentContract, TypedAgentInvoker};
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, JsonSchema)]
//! struct ResearchInput {
//!     topic: String,
//!     max_sources: u32,
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! struct ResearchOutput {
//!     summary: String,
//!     citations: Vec<String>,
//! }
//!
//! struct ResearchContract;
//! impl AgentContract for ResearchContract {
//!     type Input = ResearchInput;
//!     type Output = ResearchOutput;
//!     const SUBAGENT_TYPE: &'static str = "research";
//! }
//!
//! // At call site:
//! let invoker = TypedAgentInvoker::<ResearchContract>::new(task_tool);
//! let out = invoker.invoke(ResearchInput { topic: "rust async".into(), max_sources: 5 }).await?;
//! println!("{}", out.summary);
//! ```

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Result;

/// A typed input/output contract for subagent invocations.
///
/// Implementors are zero-sized marker types that pin a particular
/// subagent to a particular `(Input, Output)` schema. The contract
/// has no methods of its own; dispatch lives on
/// [`TypedAgentInvoker`].
pub trait AgentContract: Send + Sync + 'static {
    /// Typed payload sent to the subagent. Serialised to JSON and
    /// passed as the user prompt body. Must implement `JsonSchema`
    /// so the contract can be advertised to the model when needed.
    type Input: Serialize + DeserializeOwned + schemars::JsonSchema + Send + Sync;

    /// Typed result expected back from the subagent. Deserialised
    /// from the subagent's `structured_output` field, or from its
    /// concatenated text content as a fallback (the text must
    /// itself be valid JSON for the latter path).
    type Output: Serialize + DeserializeOwned + Send + Sync;

    /// Built-in subagent name this contract dispatches to. The
    /// agent runtime must have a subagent registered under this
    /// name (via the standard subagent registration path).
    const SUBAGENT_TYPE: &'static str;

    /// Optional short description that surfaces in the model's
    /// `Task` tool catalogue when this contract is exposed as a
    /// callable. Defaults to the subagent type.
    fn description() -> &'static str {
        Self::SUBAGENT_TYPE
    }
}

/// Thin invoker around the existing untyped [`super::TaskTool`] that
/// adds typed marshalling for one specific [`AgentContract`].
///
/// `TypedAgentInvoker` is intentionally a wrapper rather than a new
/// dispatch path: it serialises `C::Input` to JSON, hands the JSON
/// to the existing `TaskTool` as a prompt body, then parses the
/// response back into `C::Output`. This keeps the existing dispatch,
/// scheduling, security, and observability surfaces unchanged.
pub struct TypedAgentInvoker<C: AgentContract> {
    _marker: PhantomData<fn() -> C>,
}

impl<C: AgentContract> Default for TypedAgentInvoker<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: AgentContract> TypedAgentInvoker<C> {
    /// Construct a new invoker for the given contract.
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    /// Build the prompt body that the underlying `TaskTool` will
    /// receive. Encoded as a JSON object so the subagent can
    /// `serde_json::from_str::<C::Input>(prompt)` to recover its
    /// typed input losslessly.
    pub fn encode_prompt(&self, input: &C::Input) -> Result<String> {
        serde_json::to_string(input).map_err(crate::Error::from)
    }

    /// Decode a [`super::TaskOutput`] back into the typed contract
    /// output. Tries `structured_output` first; falls back to
    /// parsing the concatenated text content as JSON. Returns
    /// [`crate::Error::Parse`] when neither path produces a valid
    /// `C::Output`.
    pub fn decode_output(&self, output: &super::TaskOutput) -> Result<C::Output> {
        if let Some(structured) = &output.structured_output {
            return serde_json::from_value(structured.clone()).map_err(|e| {
                crate::Error::Parse(format!(
                    "TypedAgentInvoker: subagent `{subagent}` returned structured_output \
                     that does not match contract output: {e}",
                    subagent = C::SUBAGENT_TYPE,
                ))
            });
        }

        if let Some(text) = &output.text {
            return serde_json::from_str(text).map_err(|e| {
                crate::Error::Parse(format!(
                    "TypedAgentInvoker: subagent `{subagent}` text body is not valid \
                     JSON for contract output: {e}",
                    subagent = C::SUBAGENT_TYPE,
                ))
            });
        }

        Err(crate::Error::Parse(format!(
            "TypedAgentInvoker: subagent `{subagent}` produced neither structured_output \
             nor text content",
            subagent = C::SUBAGENT_TYPE,
        )))
    }

    /// The subagent name this invoker dispatches to. Convenience for
    /// callers building [`super::TaskInput`] manually.
    pub const fn subagent_type(&self) -> &'static str {
        C::SUBAGENT_TYPE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, schemars::JsonSchema, Debug, PartialEq)]
    struct DemoIn {
        topic: String,
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct DemoOut {
        summary: String,
    }

    struct DemoContract;
    impl AgentContract for DemoContract {
        type Input = DemoIn;
        type Output = DemoOut;
        const SUBAGENT_TYPE: &'static str = "demo";
    }

    #[test]
    fn encode_prompt_round_trips() {
        let inv = TypedAgentInvoker::<DemoContract>::new();
        let s = inv
            .encode_prompt(&DemoIn {
                topic: "rust".into(),
            })
            .unwrap();
        let parsed: DemoIn = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.topic, "rust");
    }

    #[test]
    fn decode_output_from_structured_output() {
        let inv = TypedAgentInvoker::<DemoContract>::new();
        let task_out = super::super::TaskOutput {
            agent_id: "a".into(),
            status: crate::agent::task_output::TaskStatus::Completed,
            text: None,
            content: None,
            structured_output: Some(serde_json::json!({"summary": "hello"})),
            response_metadata: None,
            execution: None,
            error: None,
        };
        let out = inv.decode_output(&task_out).unwrap();
        assert_eq!(out.summary, "hello");
    }

    #[test]
    fn decode_output_falls_back_to_text_json() {
        let inv = TypedAgentInvoker::<DemoContract>::new();
        let task_out = super::super::TaskOutput {
            agent_id: "a".into(),
            status: crate::agent::task_output::TaskStatus::Completed,
            text: Some(r#"{"summary":"from text"}"#.into()),
            content: None,
            structured_output: None,
            response_metadata: None,
            execution: None,
            error: None,
        };
        let out = inv.decode_output(&task_out).unwrap();
        assert_eq!(out.summary, "from text");
    }

    #[test]
    fn decode_output_errors_on_neither() {
        let inv = TypedAgentInvoker::<DemoContract>::new();
        let task_out = super::super::TaskOutput {
            agent_id: "a".into(),
            status: crate::agent::task_output::TaskStatus::Completed,
            text: None,
            content: None,
            structured_output: None,
            response_metadata: None,
            execution: None,
            error: None,
        };
        assert!(inv.decode_output(&task_out).is_err());
    }

    #[test]
    fn decode_output_errors_on_schema_mismatch() {
        let inv = TypedAgentInvoker::<DemoContract>::new();
        let task_out = super::super::TaskOutput {
            agent_id: "a".into(),
            status: crate::agent::task_output::TaskStatus::Completed,
            text: None,
            content: None,
            structured_output: Some(serde_json::json!({"wrong_field": 1})),
            response_metadata: None,
            execution: None,
            error: None,
        };
        assert!(inv.decode_output(&task_out).is_err());
    }
}
