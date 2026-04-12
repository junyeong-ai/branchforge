//! Cross-cutting [`DecisionReason`] trait for explainable agent
//! decisions.
//!
//! # Motivation
//!
//! Every subsystem in BranchForge that answers "why did you decide
//! that?" — authorization, compaction, recovery, budget — exposes
//! the answer via a typed reason enum. This trait is the minimal
//! common interface the observability layer uses to tag OTel spans
//! and structured logs with a cardinality-bounded category string
//! and a human-readable summary, without knowing anything about
//! the individual domains.
//!
//! Adding a new decision domain is **additive**: define
//! `MyDomainReason`, implement `DecisionReason` for it, and every
//! consumer (span attributes, dashboards, audit logs) works
//! automatically. No central registry, no dispatch table, no
//! modifications outside the new domain — this follows the
//! Open-Closed Principle the same way [`crate::client::codec::ModelCodec`]
//! does.
//!
//! # Category string contract
//!
//! `category()` returns a **compile-time constant per variant** and
//! must stay bounded in cardinality (no interpolated user data). It
//! serves as the metric/span label vocabulary — think of it as the
//! enum's "discriminant name" projected to a string.
//!
//! `summary()` returns a free-form description suitable for log
//! lines and audit trails. It MUST NOT be used as a metrics label.
//!
//! # Why not a derive macro?
//!
//! Hand-written `impl`s are cheap (the enums are small), type-safe,
//! and avoid adding a proc-macro build dependency to the pure
//! Layer 1 SDK core. If the number of domains grows past ~10 we
//! can revisit.

/// Trait implemented by every domain's "reason" enum. The
/// observability layer consumes this through dynamic dispatch, so
/// the trait is `Send + Sync` and the types are `Clone + Debug`
/// so they can be shipped across async boundaries and recorded in
/// tracing fields.
pub trait DecisionReason: std::fmt::Debug + Send + Sync {
    /// Low-cardinality category label for metrics and span
    /// attributes. Must be a compile-time constant per variant —
    /// **do not** interpolate user data into this string.
    ///
    /// Recommended shape: `snake_case` category names scoped by
    /// domain prefix where a single category name would be
    /// ambiguous across modules (e.g. `recovery.rate_limit` vs
    /// `budget.rate_limit` — though in practice we keep the
    /// prefix off when the host key already disambiguates).
    fn category(&self) -> &'static str;

    /// Human-readable summary for logs and audit trails. Arbitrary
    /// cardinality permitted; MUST NOT be used as a metrics label.
    fn summary(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    // A toy implementation verifying the trait is usable from
    // downstream domains. Real impls live next to their enum in
    // the authorization / compact / recovery / budget modules.
    #[derive(Debug, Clone)]
    enum TestReason {
        A,
        B(String),
    }

    impl DecisionReason for TestReason {
        fn category(&self) -> &'static str {
            match self {
                Self::A => "a",
                Self::B(_) => "b",
            }
        }
        fn summary(&self) -> String {
            match self {
                Self::A => "reason a".into(),
                Self::B(msg) => format!("reason b: {msg}"),
            }
        }
    }

    #[test]
    fn category_is_compile_time_constant() {
        assert_eq!(TestReason::A.category(), "a");
        assert_eq!(TestReason::B("x".into()).category(), "b");
    }

    #[test]
    fn summary_is_free_form() {
        assert_eq!(TestReason::A.summary(), "reason a");
        assert_eq!(TestReason::B("boom".into()).summary(), "reason b: boom");
    }

    #[test]
    fn reason_can_be_stored_as_trait_object() {
        let reasons: Vec<Box<dyn DecisionReason>> = vec![
            Box::new(TestReason::A),
            Box::new(TestReason::B("details".into())),
        ];
        let categories: Vec<&'static str> = reasons.iter().map(|r| r.category()).collect();
        assert_eq!(categories, vec!["a", "b"]);
    }
}
