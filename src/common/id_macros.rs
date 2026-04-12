//! Macros for generating strongly-typed ID newtypes.
//!
//! - `uuid_id!` — newtype over `uuid::Uuid` (private field).
//! - `string_id!` — newtype over `String` (private field).
//!
//! Both macros generate a full trait surface so the newtypes are
//! drop-in replacements for the raw types they wrap, while preventing
//! accidental mixing of semantically distinct identifiers.

#![allow(missing_docs)]

/// Generate a newtype wrapper around `uuid::Uuid` with a private field
/// and a complete trait surface.
///
/// ```ignore
/// uuid_id!(SessionId);
/// ```
///
/// Produces:
/// - `pub struct SessionId(uuid::Uuid)` — **private** inner field
/// - `new()`, `nil()`, `from_uuid(Uuid)`, `parse(&str) -> Option<Self>`
/// - `as_uuid()`, `into_inner()`
/// - Standard derives: Copy, Clone, Debug, Default, PartialEq, Eq,
///   PartialOrd, Ord, Hash, Serialize, Deserialize (transparent)
/// - Conversions: `From<Uuid>`, `From<Self> for Uuid`, `Display`, `FromStr`,
///   `TryFrom<&str>`, `TryFrom<String>`
#[macro_export]
macro_rules! uuid_id {
    ($Name:ident) => {
        #[derive(
            Copy,
            Clone,
            Debug,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $Name(uuid::Uuid);

        impl $Name {
            /// Create a new random (v4) identifier.
            #[inline]
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            /// The nil (all-zeros) identifier.
            #[inline]
            pub fn nil() -> Self {
                Self(uuid::Uuid::nil())
            }

            /// Wrap an existing `Uuid`.
            #[inline]
            pub fn from_uuid(uuid: uuid::Uuid) -> Self {
                Self(uuid)
            }

            /// Try to parse a UUID string, returning `None` on failure.
            #[inline]
            pub fn parse(s: &str) -> Option<Self> {
                uuid::Uuid::parse_str(s).ok().map(Self)
            }

            /// Borrow the inner `Uuid`.
            #[inline]
            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }

            /// Consume self and return the inner `Uuid`.
            #[inline]
            pub fn into_inner(self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $Name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $Name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                uuid::Uuid::parse_str(s).map(Self)
            }
        }

        impl From<uuid::Uuid> for $Name {
            fn from(uuid: uuid::Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$Name> for uuid::Uuid {
            fn from(id: $Name) -> uuid::Uuid {
                id.0
            }
        }

        impl TryFrom<&str> for $Name {
            type Error = uuid::Error;

            fn try_from(s: &str) -> Result<Self, Self::Error> {
                std::str::FromStr::from_str(s)
            }
        }

        impl TryFrom<String> for $Name {
            type Error = uuid::Error;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                std::str::FromStr::from_str(&s)
            }
        }
    };
}

/// Generate a newtype wrapper around `String` with a private field
/// and a complete trait surface.
///
/// ```ignore
/// string_id!(ModelId);
/// ```
///
/// Produces:
/// - `pub struct ModelId(String)` — **private** inner field
/// - `new(impl Into<String>)`, `as_str()`, `into_inner()`, `is_empty()`
/// - Standard derives: Clone, Debug, Default, PartialEq, Eq,
///   PartialOrd, Ord, Hash, Serialize, Deserialize (transparent)
/// - Conversions: `From<String>`, `From<&str>`, `From<Self> for String`,
///   `Display`, `FromStr` (infallible), `Borrow<str>`, `AsRef<str>`
#[macro_export]
macro_rules! string_id {
    ($Name:ident) => {
        #[derive(
            Clone,
            Debug,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        #[allow(missing_docs)]
        pub struct $Name(String);

        impl $Name {
            /// Create from any value that converts to `String`.
            #[inline]
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrow the inner string.
            #[inline]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume self and return the inner `String`.
            #[inline]
            pub fn into_inner(self) -> String {
                self.0
            }

            /// Whether the inner string is empty.
            #[inline]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl std::fmt::Display for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $Name {
            type Err = std::convert::Infallible;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_string()))
            }
        }

        impl From<String> for $Name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $Name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<$Name> for String {
            fn from(id: $Name) -> String {
                id.0
            }
        }

        impl std::borrow::Borrow<str> for $Name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $Name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $Name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $Name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $Name {
            fn eq(&self, other: &String) -> bool {
                self.0 == *other
            }
        }
    };
}

#[cfg(test)]
mod tests {
    uuid_id!(TestUuidId);
    string_id!(TestStringId);

    // ── uuid_id! tests ─────────────────────────────────────────────

    #[test]
    fn uuid_round_trip() {
        let id = TestUuidId::new();
        let s = id.to_string();
        let parsed: TestUuidId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn uuid_serde_round_trip() {
        let id = TestUuidId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: TestUuidId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        // transparent: should be a plain UUID string
        assert!(json.starts_with('"'));
    }

    #[test]
    fn uuid_nil() {
        let nil = TestUuidId::nil();
        assert_eq!(nil.as_uuid(), uuid::Uuid::nil());
        assert_eq!(nil.to_string(), "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn uuid_from_uuid_conversion() {
        let raw = uuid::Uuid::new_v4();
        let id = TestUuidId::from_uuid(raw);
        assert_eq!(id.as_uuid(), raw);
        let back: uuid::Uuid = id.into();
        assert_eq!(back, raw);
    }

    #[test]
    fn uuid_try_from() {
        let id = TestUuidId::new();
        let s = id.to_string();
        let from_str: TestUuidId = TryFrom::try_from(s.as_str()).unwrap();
        let from_string: TestUuidId = TryFrom::try_from(s.clone()).unwrap();
        assert_eq!(from_str, id);
        assert_eq!(from_string, id);
        assert!(TestUuidId::try_from("bad").is_err());
    }

    #[test]
    fn uuid_parse() {
        let id = TestUuidId::new();
        let parsed = TestUuidId::parse(&id.to_string());
        assert_eq!(parsed, Some(id));
        assert_eq!(TestUuidId::parse("bad"), None);
    }

    #[test]
    fn uuid_hashmap_lookup() {
        let id = TestUuidId::new();
        let mut map = std::collections::HashMap::new();
        map.insert(id, "value");
        assert_eq!(map.get(&id), Some(&"value"));
    }

    // ── string_id! tests ───────────────────────────────────────────

    #[test]
    fn string_round_trip() {
        let id = TestStringId::new("hello");
        assert_eq!(id.as_str(), "hello");
        assert_eq!(id.to_string(), "hello");
        let parsed: TestStringId = "hello".parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn string_serde_round_trip() {
        let id = TestStringId::new("world");
        let json = serde_json::to_string(&id).unwrap();
        let back: TestStringId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(json, "\"world\"");
    }

    #[test]
    fn string_borrow_and_hashmap() {
        use std::borrow::Borrow;
        use std::collections::HashMap;

        let id = TestStringId::new("key");
        let s: &str = id.borrow();
        assert_eq!(s, "key");

        let mut map = HashMap::new();
        map.insert(id.clone(), 42);
        // Look up by &str via Borrow<str>
        assert_eq!(map.get("key"), Some(&42));
    }

    #[test]
    fn string_from_conversions() {
        let from_str: TestStringId = "abc".into();
        let from_string: TestStringId = String::from("abc").into();
        assert_eq!(from_str, from_string);
        let back: String = from_str.into();
        assert_eq!(back, "abc");
    }
}
