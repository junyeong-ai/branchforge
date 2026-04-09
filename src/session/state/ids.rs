//! Session and message identifiers.

crate::uuid_id!(SessionId);
crate::string_id!(MessageId);

impl MessageId {
    /// Create a new random message ID (v4 UUID string).
    pub fn random() -> Self {
        Self::new(uuid::Uuid::new_v4().to_string())
    }

    /// Parse a string into a MessageId, keeping whatever value is given.
    /// Alias for `MessageId::new()` kept for backward compatibility.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_generation() {
        let id1 = SessionId::new();
        let id2 = SessionId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_session_id_parse() {
        let id = SessionId::new();
        let parsed = SessionId::parse(&id.to_string());
        assert_eq!(parsed, Some(id));
    }

    #[test]
    fn test_session_id_try_from_invalid_value() {
        let parsed = SessionId::try_from("not-a-uuid");
        assert!(parsed.is_err());
    }

    #[test]
    fn test_message_id_generation() {
        let id1 = MessageId::random();
        let id2 = MessageId::random();
        assert_ne!(id1, id2);
    }
}
