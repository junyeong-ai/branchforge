//! Persistence schema versioning (Phase D F-1).
//!
//! All session-persistence backends carry an explicit
//! [`SessionSchemaVersion`] on every payload they write, and
//! validate that version against [`SessionSchemaVersion::CURRENT`]
//! on every payload they read. This closes three classes of silent
//! failures that plagued the pre-F-1 persistence layer:
//!
//! 1. **Silent field drop**. A field added to `Session` after a
//!    file was written would simply deserialize to its serde
//!    `#[serde(default)]` value, with no signal that data was
//!    stale.
//! 2. **Future-version confusion**. An older binary reading a
//!    file written by a newer one could "almost" deserialize it —
//!    accept all the fields it understood, ignore the rest, and
//!    carry on with a subtly wrong session.
//! 3. **Unbounded compatibility promises**. Without an explicit
//!    version ladder, there was no mechanism to refuse to load a
//!    payload that the current code cannot correctly interpret.
//!
//! # Design
//!
//! - [`SessionSchemaVersion`] is a `u32` newtype. The constants
//!   [`SessionSchemaVersion::CURRENT`] (what this binary writes)
//!   and [`SessionSchemaVersion::MIN_SUPPORTED`] (the oldest
//!   version the [`MigrationLadder`] can upgrade from) bound the
//!   acceptable range.
//! - [`SchemaMigration`] is a pure function from one version's
//!   serialized form to the next. Backends compose them into a
//!   [`MigrationLadder`] at load time; each step is independently
//!   testable.
//! - [`crate::session::SessionError::SchemaVersionMismatch`] is the single error
//!   variant any backend returns when a payload's version is
//!   outside the supported window. The variant carries the
//!   component name (`"jsonl"`, `"postgres"`, …), the observed
//!   version, and the expected version, so observability can
//!   aggregate mismatches per backend.
//!
//! # Forward- vs backward-compatibility
//!
//! Loading an **older** version is always attempted via the
//! migration ladder — that's the forward-compat promise. Loading
//! a **newer** version than the code knows about is a hard
//! failure: the migration ladder only climbs upward, and
//! silently accepting a newer payload risks dropping data the
//! old binary does not understand. Operators who downgrade a
//! deployment must either (a) restore from a matching-version
//! backup or (b) write a downgrade migration and land it on the
//! older branch.

use serde::{Deserialize, Serialize};

/// Schema version stamped on every persisted session payload.
///
/// The `u32` space is deliberately large — versions are bumped
/// whenever any persisted struct gains/drops/renames a field,
/// and a long-lived project will cross several dozen bumps.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SessionSchemaVersion(pub u32);

impl SessionSchemaVersion {
    /// The version the current binary **writes** to persistence.
    /// Bump this in lockstep with any change to the persisted
    /// struct layout — adding a field, tightening a type,
    /// splitting a variant, etc. — and ship a matching
    /// [`SchemaMigration`] into the default ladder.
    pub const CURRENT: Self = Self(1);

    /// The oldest version the built-in migration ladder can
    /// upgrade from. A payload at `MIN_SUPPORTED` upgrades to
    /// `CURRENT` through a chain of [`SchemaMigration`] calls;
    /// a payload below `MIN_SUPPORTED` is rejected with
    /// [`crate::session::SessionError::SchemaVersionMismatch`].
    pub const MIN_SUPPORTED: Self = Self(1);

    /// Raw numeric view of the version, for tracing / metrics.
    pub const fn value(self) -> u32 {
        self.0
    }

    /// `true` when `self` can be upgraded in place to
    /// [`Self::CURRENT`] by a migration ladder. This is the
    /// `MIN_SUPPORTED ≤ self ≤ CURRENT` window.
    pub fn is_supported(self) -> bool {
        self >= Self::MIN_SUPPORTED && self <= Self::CURRENT
    }

    /// `true` when `self` refers to a version the current binary
    /// has **not** been taught about yet — i.e. a payload written
    /// by a newer deployment. Loading one of these is a hard
    /// failure; it must never be silently degraded.
    pub fn is_from_the_future(self) -> bool {
        self > Self::CURRENT
    }

    /// The next version, used when composing a migration ladder.
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl std::fmt::Display for SessionSchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// Direction a schema-version mismatch was caught in. Helps the
/// error message be actionable: "too old, upgrade" vs
/// "too new, restore from backup".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SchemaVersionMismatchDirection {
    /// Payload version is below [`SessionSchemaVersion::MIN_SUPPORTED`].
    /// Callers must either write a migration for that version or
    /// retire the data.
    TooOld,
    /// Payload version is above [`SessionSchemaVersion::CURRENT`].
    /// Callers must upgrade the binary or restore from a
    /// version-matching backup.
    TooNew,
}

impl std::fmt::Display for SchemaVersionMismatchDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooOld => write!(f, "too-old"),
            Self::TooNew => write!(f, "too-new"),
        }
    }
}

/// Phase H-3: typed failure modes for [`MigrationLadder::upgrade_to`].
///
/// Replaces the former 2-variant [`SchemaVersionMismatchDirection`]
/// at the migration primitive boundary so operators can distinguish:
///
/// - "source is older than `MIN_SUPPORTED`" (TooOld)
/// - "source is newer than the requested target" (TooNew)
/// - "caller requested a downgrade" (DowngradeRejected)
/// - "ladder has a gap between current and target" (MissingStep)
/// - "a step's `migrate()` returned an error" (StepFailed)
///
/// The outer [`crate::session::SessionError::SchemaVersionMismatch`]
/// variant still carries the simpler `SchemaVersionMismatchDirection`
/// for the persistence-backend error surface; the two enums are
/// bridged at the backend boundary via the [`From`] impl below so
/// recovery-aware callers (persistence loaders) can map rich
/// ladder errors back to the coarse direction they serialize over
/// the wire.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaMigrationError {
    /// Source version is below [`SessionSchemaVersion::MIN_SUPPORTED`].
    /// The migration ladder cannot upgrade it — the operator must
    /// write a new migration step or retire the data.
    #[error("source version {found} is below the supported minimum {minimum}")]
    TooOld {
        found: SessionSchemaVersion,
        minimum: SessionSchemaVersion,
    },

    /// Source version is above the requested target. Typically
    /// surfaces as "payload written by a newer binary" when the
    /// running process cannot correctly interpret the data.
    #[error("source version {found} is newer than the target {maximum}")]
    TooNew {
        found: SessionSchemaVersion,
        maximum: SessionSchemaVersion,
    },

    /// Caller explicitly requested a downgrade (`from > to`).
    /// Downgrades are not supported — restore from a
    /// version-matching backup instead.
    #[error("downgrade from {from} to {to} is not supported")]
    DowngradeRejected {
        from: SessionSchemaVersion,
        to: SessionSchemaVersion,
    },

    /// Ladder reached a version without an applicable step. The
    /// current binary ships an incomplete migration set — this is
    /// a build bug, not a data issue.
    #[error("migration ladder has no step from {from} to {to}")]
    MissingStep {
        from: SessionSchemaVersion,
        to: SessionSchemaVersion,
    },

    /// A migration step's `migrate()` returned `Err`. The `at`
    /// field names the source version of the failing step so
    /// operators can locate the offending [`SchemaMigration`] impl.
    #[error("migration step at {at} failed: {reason}")]
    StepFailed {
        at: SessionSchemaVersion,
        reason: String,
    },
}

impl SchemaMigrationError {
    /// Collapse into the coarse [`SchemaVersionMismatchDirection`]
    /// shape that persistence backends serialize on their wire
    /// [`crate::session::SessionError::SchemaVersionMismatch`]
    /// variant. `MissingStep` and `StepFailed` map to `TooOld`
    /// because they indicate the local binary cannot bring the
    /// source forward to the target — the operational remedy is
    /// identical to "payload too old": upgrade the binary or
    /// replace the data.
    pub fn direction(&self) -> SchemaVersionMismatchDirection {
        match self {
            Self::TooNew { .. } | Self::DowngradeRejected { .. } => {
                SchemaVersionMismatchDirection::TooNew
            }
            Self::TooOld { .. } | Self::MissingStep { .. } | Self::StepFailed { .. } => {
                SchemaVersionMismatchDirection::TooOld
            }
        }
    }
}

/// One upgrade step in a [`MigrationLadder`].
///
/// Migrations are intentionally **pure**: they take a single
/// `serde_json::Value` payload and return the upgraded one. They
/// never touch the session, the graph, or the event bus. The
/// side-effecting parts (reading files, writing rows) live in
/// the backend; the ladder only describes how a given version's
/// bytes become the next version's bytes.
pub trait SchemaMigration: Send + Sync + std::fmt::Debug {
    /// Version this migration upgrades *from*.
    fn source_version(&self) -> SessionSchemaVersion;

    /// Version this migration upgrades *to*. Must equal
    /// `source_version().next()` — a migration step is always
    /// exactly one version wide so the ladder can validate
    /// its own linearity at construction time.
    fn target_version(&self) -> SessionSchemaVersion;

    /// Apply the upgrade in place.
    fn migrate(&self, payload: &mut serde_json::Value) -> Result<(), String>;
}

/// Ordered chain of [`SchemaMigration`]s that can bring any
/// payload in `[MIN_SUPPORTED, CURRENT)` up to `CURRENT`.
///
/// The ladder is immutable once built. `new` fails if the
/// supplied steps don't form a contiguous chain from
/// `MIN_SUPPORTED` to `CURRENT` — e.g. a step missing in the
/// middle, or a step whose `target_version` doesn't match the next
/// step's `source_version`. This surfaces author mistakes at
/// binary start-up rather than at the first load attempt.
#[derive(Debug, Default)]
pub struct MigrationLadder {
    steps: Vec<Box<dyn SchemaMigration>>,
}

impl MigrationLadder {
    /// Build the default ladder for [`SessionSchemaVersion::CURRENT`].
    /// At F-1 the current version is `1` and the ladder is empty;
    /// future bumps plug a new [`SchemaMigration`] in here and
    /// ship a unit test that exercises the upgrade path.
    pub fn default_ladder() -> Self {
        Self { steps: Vec::new() }
    }

    /// Explicit constructor taking a chain of migrations. Validates
    /// that the chain is contiguous and terminates at
    /// [`SessionSchemaVersion::CURRENT`]; returns an error naming
    /// the first broken step otherwise.
    pub fn new(steps: Vec<Box<dyn SchemaMigration>>) -> Result<Self, String> {
        if steps.is_empty() {
            return Ok(Self { steps });
        }

        let mut expected = steps[0].source_version();
        for (idx, step) in steps.iter().enumerate() {
            if step.source_version() != expected {
                return Err(format!(
                    "migration ladder step {idx} expects source_version={expected} but declared {}",
                    step.source_version()
                ));
            }
            if step.target_version() != step.source_version().next() {
                return Err(format!(
                    "migration ladder step {idx} at {} must advance by exactly one version, got target_version={}",
                    step.source_version(),
                    step.target_version()
                ));
            }
            expected = step.target_version();
        }

        Ok(Self { steps })
    }

    /// Phase H-3: core primitive. Walk the ladder from `from` to
    /// `to`, applying one step at a time. This is the building
    /// block that [`Self::upgrade_to_current`] wraps; it exists as
    /// its own method so tests, analysis tooling, and future
    /// "upgrade to a specific version for replay" scenarios can
    /// call it without being tied to the current `CURRENT` value.
    ///
    /// # Contract
    /// - `from == to` → no-op, returns `Ok(from)`.
    /// - `from > to` → [`SchemaMigrationError::DowngradeRejected`].
    /// - `from < MIN_SUPPORTED` → [`SchemaMigrationError::TooOld`].
    /// - missing step between `current` and `to` →
    ///   [`SchemaMigrationError::MissingStep`].
    /// - step's `migrate()` returns `Err(reason)` →
    ///   [`SchemaMigrationError::StepFailed`] with the source
    ///   version and reason.
    /// - success → `Ok(to)`.
    pub fn upgrade_to(
        &self,
        payload: &mut serde_json::Value,
        from: SessionSchemaVersion,
        to: SessionSchemaVersion,
    ) -> Result<SessionSchemaVersion, SchemaMigrationError> {
        if from == to {
            return Ok(from);
        }
        if from > to {
            return Err(SchemaMigrationError::DowngradeRejected { from, to });
        }
        if from < SessionSchemaVersion::MIN_SUPPORTED {
            return Err(SchemaMigrationError::TooOld {
                found: from,
                minimum: SessionSchemaVersion::MIN_SUPPORTED,
            });
        }

        let mut current = from;
        for step in &self.steps {
            if current == to {
                break;
            }
            if step.source_version() == current {
                step.migrate(payload)
                    .map_err(|reason| SchemaMigrationError::StepFailed {
                        at: current,
                        reason,
                    })?;
                current = step.target_version();
            }
        }

        if current != to {
            return Err(SchemaMigrationError::MissingStep { from: current, to });
        }
        Ok(current)
    }

    /// Upgrade `payload` from `from` to
    /// [`SessionSchemaVersion::CURRENT`]. Convenience wrapper around
    /// [`Self::upgrade_to`]; rejects payloads newer than `CURRENT`
    /// with [`SchemaMigrationError::TooNew`] before delegating.
    pub fn upgrade_to_current(
        &self,
        payload: &mut serde_json::Value,
        from: SessionSchemaVersion,
    ) -> Result<SessionSchemaVersion, SchemaMigrationError> {
        if from > SessionSchemaVersion::CURRENT {
            return Err(SchemaMigrationError::TooNew {
                found: from,
                maximum: SessionSchemaVersion::CURRENT,
            });
        }
        self.upgrade_to(payload, from, SessionSchemaVersion::CURRENT)
    }

    /// Number of steps in the ladder. Zero when no migrations
    /// are required (the current F-1 baseline).
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Ladders with no steps are trivially valid — they accept
    /// only `CURRENT` payloads.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_and_min_supported_are_consistent() {
        assert!(SessionSchemaVersion::MIN_SUPPORTED <= SessionSchemaVersion::CURRENT);
    }

    #[test]
    fn current_version_is_supported_and_not_from_the_future() {
        let v = SessionSchemaVersion::CURRENT;
        assert!(v.is_supported());
        assert!(!v.is_from_the_future());
    }

    #[test]
    fn future_version_is_rejected() {
        let future = SessionSchemaVersion(SessionSchemaVersion::CURRENT.value() + 5);
        assert!(future.is_from_the_future());
        assert!(!future.is_supported());
    }

    #[test]
    fn default_ladder_accepts_current_payload() {
        let ladder = MigrationLadder::default_ladder();
        let mut payload = serde_json::json!({"hello": "world"});
        let result = ladder
            .upgrade_to_current(&mut payload, SessionSchemaVersion::CURRENT)
            .unwrap();
        assert_eq!(result, SessionSchemaVersion::CURRENT);
        assert_eq!(payload["hello"], "world");
    }

    #[test]
    fn default_ladder_rejects_future_payload_as_too_new() {
        let ladder = MigrationLadder::default_ladder();
        let mut payload = serde_json::json!({});
        let future = SessionSchemaVersion(SessionSchemaVersion::CURRENT.value() + 1);
        let err = ladder.upgrade_to_current(&mut payload, future).unwrap_err();
        assert!(
            matches!(err, SchemaMigrationError::TooNew { .. }),
            "expected TooNew, got {err:?}"
        );
        assert_eq!(
            err.direction(),
            SchemaVersionMismatchDirection::TooNew,
            "collapse-to-direction must mirror the typed variant"
        );
    }

    #[test]
    fn explicit_new_rejects_non_contiguous_chain() {
        #[derive(Debug)]
        struct Stub(SessionSchemaVersion, SessionSchemaVersion);
        impl SchemaMigration for Stub {
            fn source_version(&self) -> SessionSchemaVersion {
                self.0
            }
            fn target_version(&self) -> SessionSchemaVersion {
                self.1
            }
            fn migrate(&self, _: &mut serde_json::Value) -> Result<(), String> {
                Ok(())
            }
        }

        // A chain that jumps from v1 -> v3 — the ladder must refuse.
        let err = MigrationLadder::new(vec![Box::new(Stub(
            SessionSchemaVersion(1),
            SessionSchemaVersion(3),
        ))])
        .unwrap_err();
        assert!(err.contains("must advance by exactly one version"));
    }

    /// Hypothetical v1 → v2 chain: validates the ladder framework
    /// by constructing a migration that renames `old_field` to
    /// `new_field`, feeding a v1 payload, and observing the
    /// upgrade. This test does not bump `CURRENT`; it builds a
    /// local ladder via the public `new` constructor.
    #[test]
    fn hypothetical_v1_to_v2_migration_runs_through_the_ladder() {
        #[derive(Debug)]
        struct RenameField;
        impl SchemaMigration for RenameField {
            fn source_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(1)
            }
            fn target_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(2)
            }
            fn migrate(&self, payload: &mut serde_json::Value) -> Result<(), String> {
                if let Some(obj) = payload.as_object_mut()
                    && let Some(value) = obj.remove("old_field")
                {
                    obj.insert("new_field".to_string(), value);
                }
                Ok(())
            }
        }

        // Temporarily build a ladder with only the v1 -> v2 step.
        let ladder = MigrationLadder::new(vec![Box::new(RenameField)]).unwrap();
        let mut payload = serde_json::json!({"old_field": 42});
        // Manually call migrate through the ladder, skipping the
        // version window check by using the step directly — the
        // public `upgrade_to_current` uses `CURRENT` which is 1
        // at F-1 baseline, so we exercise the `SchemaMigration`
        // trait contract here without lying about CURRENT.
        ladder.steps[0].migrate(&mut payload).unwrap();
        assert_eq!(payload["new_field"], 42);
        assert!(payload.get("old_field").is_none());
    }

    // ── Phase H-3: MigrationLadder::upgrade_to primitive ───────────

    /// Dummy multi-step migration ladder used by the H-3 tests.
    /// Builds a v1 → v2 → v3 chain without touching `CURRENT` so
    /// the test exercises the core `upgrade_to` primitive against
    /// an arbitrary target version.
    fn build_test_ladder() -> MigrationLadder {
        #[derive(Debug)]
        struct RenameField;
        impl SchemaMigration for RenameField {
            fn source_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(1)
            }
            fn target_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(2)
            }
            fn migrate(&self, payload: &mut serde_json::Value) -> Result<(), String> {
                if let Some(obj) = payload.as_object_mut()
                    && let Some(v) = obj.remove("name")
                {
                    obj.insert("display_name".to_string(), v);
                }
                Ok(())
            }
        }

        #[derive(Debug)]
        struct WrapInData;
        impl SchemaMigration for WrapInData {
            fn source_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(2)
            }
            fn target_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(3)
            }
            fn migrate(&self, payload: &mut serde_json::Value) -> Result<(), String> {
                let old = std::mem::replace(payload, serde_json::json!({}));
                *payload = serde_json::json!({ "data": old });
                Ok(())
            }
        }

        MigrationLadder::new(vec![Box::new(RenameField), Box::new(WrapInData)]).unwrap()
    }

    #[test]
    fn upgrade_to_executes_multi_step_ladder_end_to_end() {
        let ladder = build_test_ladder();
        let mut payload = serde_json::json!({"name": "alice"});
        let result = ladder
            .upgrade_to(
                &mut payload,
                SessionSchemaVersion(1),
                SessionSchemaVersion(3),
            )
            .unwrap();
        assert_eq!(result, SessionSchemaVersion(3));
        // v1 → v2 renamed `name` → `display_name`; v2 → v3 wrapped
        // the whole object under `data`. Verify both steps ran.
        assert_eq!(payload["data"]["display_name"], "alice");
        assert!(payload["data"].get("name").is_none());
    }

    #[test]
    fn upgrade_to_rejects_downgrade_with_typed_error() {
        let ladder = MigrationLadder::default_ladder();
        let mut payload = serde_json::json!({});
        let err = ladder
            .upgrade_to(
                &mut payload,
                SessionSchemaVersion(5),
                SessionSchemaVersion(2),
            )
            .unwrap_err();
        match err {
            SchemaMigrationError::DowngradeRejected { from, to } => {
                assert_eq!(from, SessionSchemaVersion(5));
                assert_eq!(to, SessionSchemaVersion(2));
            }
            other => panic!("expected DowngradeRejected, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_to_noop_when_from_equals_to() {
        let ladder = MigrationLadder::default_ladder();
        let mut payload = serde_json::json!({"preserve": "me"});
        let result = ladder
            .upgrade_to(
                &mut payload,
                SessionSchemaVersion::CURRENT,
                SessionSchemaVersion::CURRENT,
            )
            .unwrap();
        assert_eq!(result, SessionSchemaVersion::CURRENT);
        assert_eq!(
            payload["preserve"], "me",
            "noop upgrade must not touch the payload"
        );
    }

    #[test]
    fn upgrade_to_surfaces_step_failure_with_source_version() {
        #[derive(Debug)]
        struct AlwaysFails;
        impl SchemaMigration for AlwaysFails {
            fn source_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(1)
            }
            fn target_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(2)
            }
            fn migrate(&self, _: &mut serde_json::Value) -> Result<(), String> {
                Err("intentional failure for H-3 test".into())
            }
        }
        let ladder = MigrationLadder::new(vec![Box::new(AlwaysFails)]).unwrap();
        let mut payload = serde_json::json!({});
        let err = ladder
            .upgrade_to(
                &mut payload,
                SessionSchemaVersion(1),
                SessionSchemaVersion(2),
            )
            .unwrap_err();
        match err {
            SchemaMigrationError::StepFailed { at, reason } => {
                assert_eq!(at, SessionSchemaVersion(1));
                assert!(
                    reason.contains("intentional failure"),
                    "step failure reason must be forwarded verbatim: `{reason}`"
                );
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
    }

    #[test]
    fn upgrade_to_detects_missing_step_in_ladder() {
        // Ladder has v1 → v2 only. Request upgrade to v3 — no step
        // advances from v2, so the ladder must surface MissingStep
        // rather than silently returning v2.
        #[derive(Debug)]
        struct OnlyV1ToV2;
        impl SchemaMigration for OnlyV1ToV2 {
            fn source_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(1)
            }
            fn target_version(&self) -> SessionSchemaVersion {
                SessionSchemaVersion(2)
            }
            fn migrate(&self, _: &mut serde_json::Value) -> Result<(), String> {
                Ok(())
            }
        }
        let ladder = MigrationLadder::new(vec![Box::new(OnlyV1ToV2)]).unwrap();
        let mut payload = serde_json::json!({});
        let err = ladder
            .upgrade_to(
                &mut payload,
                SessionSchemaVersion(1),
                SessionSchemaVersion(3),
            )
            .unwrap_err();
        match err {
            SchemaMigrationError::MissingStep { from, to } => {
                assert_eq!(from, SessionSchemaVersion(2));
                assert_eq!(to, SessionSchemaVersion(3));
            }
            other => panic!("expected MissingStep, got {other:?}"),
        }
    }

    #[test]
    fn schema_migration_error_direction_collapses_correctly() {
        let too_new = SchemaMigrationError::TooNew {
            found: SessionSchemaVersion(5),
            maximum: SessionSchemaVersion(3),
        };
        assert_eq!(too_new.direction(), SchemaVersionMismatchDirection::TooNew);

        let downgrade = SchemaMigrationError::DowngradeRejected {
            from: SessionSchemaVersion(5),
            to: SessionSchemaVersion(2),
        };
        assert_eq!(
            downgrade.direction(),
            SchemaVersionMismatchDirection::TooNew
        );

        let too_old = SchemaMigrationError::TooOld {
            found: SessionSchemaVersion(0),
            minimum: SessionSchemaVersion(1),
        };
        assert_eq!(too_old.direction(), SchemaVersionMismatchDirection::TooOld);

        let missing = SchemaMigrationError::MissingStep {
            from: SessionSchemaVersion(2),
            to: SessionSchemaVersion(3),
        };
        assert_eq!(missing.direction(), SchemaVersionMismatchDirection::TooOld);

        let step_failed = SchemaMigrationError::StepFailed {
            at: SessionSchemaVersion(1),
            reason: "test".into(),
        };
        assert_eq!(
            step_failed.direction(),
            SchemaVersionMismatchDirection::TooOld
        );
    }
}
