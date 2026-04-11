//! Type-keyed heterogeneous storage for carrying feature-gated context into
//! generic execution paths.
//!
//! `Extensions` is a `TypeId`-keyed container that stores at most one instance
//! of each distinct type. It is the mechanism by which upper-layer concerns
//! (workspace path, security policy, git state, telemetry sink, tenant id, …)
//! attach themselves to Layer 1 objects like [`crate::tools::ExecutionContext`]
//! and [`crate::agent::AgentConfig`] without polluting those types with
//! hard-coded Layer 2+ fields.
//!
//! # Design
//!
//! The API mirrors [`http::Extensions`](https://docs.rs/http/latest/http/struct.Extensions.html)
//! and the pattern used by `axum::Extension`, `tower` middleware, and
//! `reqwest` request extensions. The key benefits:
//!
//! - **Type safety**: each extension lives under its own type key; retrieval
//!   is `get::<T>() -> Option<&T>`, there are no stringly-typed lookups.
//! - **Zero cost when empty**: the inner `HashMap` is lazily allocated on
//!   first insert; a context with no extensions carries a single `None`.
//! - **Layering discipline**: Layer 1 code never mentions Layer 2 types; it
//!   only exposes `extensions()` and `extensions_mut()`. Layer 2 (coding,
//!   cloud providers, telemetry, multi-tenant) inserts its own extension
//!   types and reads them back where needed. Third-party crates can do the
//!   same without any upstream change.
//!
//! # Example
//!
//! ```
//! use branchforge::common::Extensions;
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct TenantId(String);
//!
//! let mut ext = Extensions::new();
//! ext.insert(TenantId("acme".into()));
//!
//! assert_eq!(ext.get::<TenantId>(), Some(&TenantId("acme".into())));
//! assert_eq!(ext.get::<u32>(), None);
//! ```
//!
//! # Concurrency
//!
//! `Extensions` is not itself thread-safe; it is meant to be owned by a
//! short-lived per-turn object (like [`ExecutionContext`][crate::tools::ExecutionContext])
//! and cloned across tasks as needed. Extension values must be `Send + Sync`
//! so that the owning context can cross `.await` points and thread boundaries.
//!
//! Each extension must also be `Clone` because `ExecutionContext` itself is
//! `Clone` (agent runtime clones it per tool call). If you need shared
//! mutability, wrap your extension in `Arc<Mutex<_>>` or `Arc<RwLock<_>>`
//! before inserting it.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;

/// A type-keyed heterogeneous storage container.
///
/// `Extensions` stores at most one value per distinct type and retrieves it
/// by that type as the key. It is the mechanism by which upper-layer
/// concerns (workspace path, security policy, git state, telemetry sink,
/// tenant id, …) attach themselves to Layer 1 objects like
/// [`crate::tools::ExecutionContext`] and [`crate::agent::AgentConfig`]
/// without polluting those types with hard-coded Layer 2+ fields.
///
/// The API mirrors [`http::Extensions`](https://docs.rs/http) and follows
/// the pattern used by `axum::Extension`, `tower` middleware, and `reqwest`
/// request extensions. See also `docs/architecture/layering.md` §4.
///
/// # Example
///
/// ```
/// use branchforge::common::Extensions;
///
/// #[derive(Clone, Debug, PartialEq)]
/// struct TenantId(String);
///
/// let mut ext = Extensions::new();
/// ext.insert(TenantId("acme".into()));
/// assert_eq!(ext.get::<TenantId>(), Some(&TenantId("acme".into())));
/// assert_eq!(ext.get::<u32>(), None);
/// ```
///
/// # Concurrency
///
/// `Extensions` is not itself thread-safe; it is meant to be owned by a
/// short-lived per-turn object (like `ExecutionContext`) and cloned across
/// tasks as needed. Values must be `Send + Sync + Clone + 'static` because
/// the owning context crosses `.await` points and is itself `Clone`.
#[derive(Default, Clone)]
pub struct Extensions {
    // Lazily allocated: an empty Extensions carries no HashMap, only None.
    // This matters because every ExecutionContext clone in the hot path
    // pays for this field.
    map: Option<HashMap<TypeId, BoxedExtension>>,
}

impl Extensions {
    /// Create a new, empty extensions container.
    ///
    /// No allocation is performed until the first [`insert`][Self::insert].
    pub const fn new() -> Self {
        Self { map: None }
    }

    /// Insert a value into the container.
    ///
    /// If a value of the same type was already present, it is replaced and
    /// the previous value is returned.
    ///
    /// Extensions must be `Send + Sync + Clone + 'static`. The `Clone` bound
    /// is required because `Extensions` itself is `Clone`.
    pub fn insert<T>(&mut self, value: T) -> Option<T>
    where
        T: Clone + Send + Sync + 'static,
    {
        let map = self.map.get_or_insert_with(HashMap::new);
        map.insert(TypeId::of::<T>(), BoxedExtension::new(value))
            .and_then(BoxedExtension::into_inner::<T>)
    }

    /// Get a shared reference to a value by its type.
    ///
    /// Returns `None` if no value of type `T` has been inserted.
    pub fn get<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.map
            .as_ref()?
            .get(&TypeId::of::<T>())
            .and_then(BoxedExtension::downcast_ref::<T>)
    }

    /// Get a mutable reference to a value by its type.
    ///
    /// Returns `None` if no value of type `T` has been inserted.
    pub fn get_mut<T>(&mut self) -> Option<&mut T>
    where
        T: Send + Sync + 'static,
    {
        self.map
            .as_mut()?
            .get_mut(&TypeId::of::<T>())
            .and_then(BoxedExtension::downcast_mut::<T>)
    }

    /// Remove a value from the container, returning it if present.
    pub fn remove<T>(&mut self) -> Option<T>
    where
        T: Send + Sync + 'static,
    {
        self.map
            .as_mut()?
            .remove(&TypeId::of::<T>())
            .and_then(BoxedExtension::into_inner::<T>)
    }

    /// Returns `true` if the container has no extensions.
    pub fn is_empty(&self) -> bool {
        self.map.as_ref().is_none_or(HashMap::is_empty)
    }

    /// Returns the number of extensions stored.
    pub fn len(&self) -> usize {
        self.map.as_ref().map_or(0, HashMap::len)
    }

    /// Returns `true` if an extension of type `T` is present.
    pub fn contains<T>(&self) -> bool
    where
        T: Send + Sync + 'static,
    {
        self.map
            .as_ref()
            .is_some_and(|m| m.contains_key(&TypeId::of::<T>()))
    }

    /// Remove all extensions, releasing the backing allocation.
    pub fn clear(&mut self) {
        self.map = None;
    }

    /// Merge another container's extensions into this one.
    ///
    /// Values from `other` overwrite values of the same type already present.
    pub fn extend(&mut self, other: Extensions) {
        let Some(other_map) = other.map else {
            return;
        };
        let map = self.map.get_or_insert_with(HashMap::new);
        for (key, value) in other_map {
            map.insert(key, value);
        }
    }
}

impl fmt::Debug for Extensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extensions")
            .field("len", &self.len())
            .finish()
    }
}

// =============================================================================
// BoxedExtension — internal erased+cloneable wrapper
// =============================================================================
//
// We cannot put `Box<dyn Any + Send + Sync>` into a container that implements
// `Clone`, because `dyn Any` is not `Clone`. So we carry a vtable-like clone
// function alongside the erased value.

struct BoxedExtension {
    value: Box<dyn Any + Send + Sync>,
    clone_fn: fn(&(dyn Any + Send + Sync)) -> Box<dyn Any + Send + Sync>,
}

impl BoxedExtension {
    fn new<T: Clone + Send + Sync + 'static>(value: T) -> Self {
        Self {
            value: Box::new(value),
            clone_fn: |any| {
                // SAFETY: the function pointer is instantiated for a specific
                // T at the call site, and is only ever invoked with that same
                // T, enforced by the construction-time type parameter.
                let concrete = any
                    .downcast_ref::<T>()
                    .expect("BoxedExtension clone_fn called with wrong type");
                Box::new(concrete.clone())
            },
        }
    }

    fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.value.downcast_ref::<T>()
    }

    fn downcast_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.value.downcast_mut::<T>()
    }

    fn into_inner<T: 'static>(self) -> Option<T> {
        self.value.downcast::<T>().ok().map(|b| *b)
    }
}

impl Clone for BoxedExtension {
    fn clone(&self) -> Self {
        Self {
            value: (self.clone_fn)(&*self.value),
            clone_fn: self.clone_fn,
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct TenantId(String);

    #[derive(Clone, Debug, PartialEq)]
    struct TraceId(u64);

    #[derive(Clone, Debug, PartialEq)]
    struct UserRoles(Vec<String>);

    #[test]
    fn new_is_empty() {
        let ext = Extensions::new();
        assert!(ext.is_empty());
        assert_eq!(ext.len(), 0);
        assert!(ext.map.is_none(), "empty Extensions should not allocate");
    }

    #[test]
    fn insert_and_get() {
        let mut ext = Extensions::new();
        ext.insert(TenantId("acme".into()));
        assert_eq!(ext.get::<TenantId>(), Some(&TenantId("acme".into())));
        assert_eq!(ext.get::<TraceId>(), None);
    }

    #[test]
    fn insert_replaces_existing() {
        let mut ext = Extensions::new();
        assert_eq!(ext.insert(TraceId(1)), None);
        assert_eq!(ext.insert(TraceId(2)), Some(TraceId(1)));
        assert_eq!(ext.get::<TraceId>(), Some(&TraceId(2)));
    }

    #[test]
    fn get_mut_allows_mutation() {
        let mut ext = Extensions::new();
        ext.insert(UserRoles(vec!["viewer".into()]));
        ext.get_mut::<UserRoles>().unwrap().0.push("editor".into());
        assert_eq!(
            ext.get::<UserRoles>().unwrap().0,
            vec!["viewer".to_string(), "editor".into()]
        );
    }

    #[test]
    fn remove_returns_value() {
        let mut ext = Extensions::new();
        ext.insert(TenantId("acme".into()));
        assert_eq!(ext.remove::<TenantId>(), Some(TenantId("acme".into())));
        assert_eq!(ext.get::<TenantId>(), None);
        assert!(ext.is_empty());
    }

    #[test]
    fn contains_reports_presence() {
        let mut ext = Extensions::new();
        assert!(!ext.contains::<TenantId>());
        ext.insert(TenantId("acme".into()));
        assert!(ext.contains::<TenantId>());
        assert!(!ext.contains::<TraceId>());
    }

    #[test]
    fn multiple_distinct_types_coexist() {
        let mut ext = Extensions::new();
        ext.insert(TenantId("acme".into()));
        ext.insert(TraceId(42));
        ext.insert(UserRoles(vec!["admin".into()]));

        assert_eq!(ext.len(), 3);
        assert_eq!(ext.get::<TenantId>(), Some(&TenantId("acme".into())));
        assert_eq!(ext.get::<TraceId>(), Some(&TraceId(42)));
        assert_eq!(ext.get::<UserRoles>().unwrap().0, vec!["admin".to_string()]);
    }

    #[test]
    fn clone_is_deep_and_independent() {
        let mut ext = Extensions::new();
        ext.insert(UserRoles(vec!["a".into()]));

        let mut cloned = ext.clone();
        cloned.get_mut::<UserRoles>().unwrap().0.push("b".into());

        // Original must be unchanged.
        assert_eq!(ext.get::<UserRoles>().unwrap().0, vec!["a".to_string()]);
        assert_eq!(
            cloned.get::<UserRoles>().unwrap().0,
            vec!["a".to_string(), "b".into()]
        );
    }

    #[test]
    fn clear_releases_allocation() {
        let mut ext = Extensions::new();
        ext.insert(TraceId(1));
        ext.clear();
        assert!(ext.is_empty());
        assert!(ext.map.is_none());
    }

    #[test]
    fn extend_merges_and_overwrites() {
        let mut a = Extensions::new();
        a.insert(TenantId("acme".into()));
        a.insert(TraceId(1));

        let mut b = Extensions::new();
        b.insert(TraceId(2));
        b.insert(UserRoles(vec!["admin".into()]));

        a.extend(b);

        assert_eq!(a.len(), 3);
        assert_eq!(a.get::<TenantId>(), Some(&TenantId("acme".into())));
        assert_eq!(a.get::<TraceId>(), Some(&TraceId(2))); // overwritten
        assert_eq!(a.get::<UserRoles>().unwrap().0, vec!["admin".to_string()]);
    }

    #[test]
    fn extend_from_empty_is_noop() {
        let mut a = Extensions::new();
        a.insert(TraceId(1));
        a.extend(Extensions::new());
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn extensions_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Extensions>();
    }

    #[test]
    fn debug_reports_length_not_contents() {
        let mut ext = Extensions::new();
        ext.insert(TenantId("secret".into()));
        let s = format!("{:?}", ext);
        assert!(s.contains("len: 1"));
        assert!(!s.contains("secret"));
    }
}
