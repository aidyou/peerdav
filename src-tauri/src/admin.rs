//! Admin credential validation, hashing, and the first-login password rule.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto;
use crate::error::DomainError;
use crate::types::{AdminRecord, MAX_PASSWORD_CHARS, MAX_USERNAME_CHARS, MIN_PASSWORD_CHARS};

pub(crate) fn validate_password(password: &str) -> Result<(), DomainError> {
    if password.trim().is_empty() {
        return Err(DomainError::WeakPassword { reason: "blank" });
    }

    let length = password.chars().count();
    if length < MIN_PASSWORD_CHARS {
        return Err(DomainError::WeakPassword {
            reason: "too_short",
        });
    }
    if length > MAX_PASSWORD_CHARS {
        return Err(DomainError::WeakPassword { reason: "too_long" });
    }
    if password.trim() != password {
        return Err(DomainError::WeakPassword {
            reason: "edge_whitespace",
        });
    }
    if password
        .chars()
        .any(|character| !(character.is_ascii_graphic() || character == ' '))
    {
        return Err(DomainError::WeakPassword {
            reason: "invalid_char",
        });
    }
    if !password
        .chars()
        .any(|character| character.is_ascii_alphabetic())
    {
        return Err(DomainError::WeakPassword {
            reason: "missing_letter",
        });
    }
    if !password.chars().any(|character| character.is_ascii_digit()) {
        return Err(DomainError::WeakPassword {
            reason: "missing_digit",
        });
    }

    Ok(())
}

/// Validates the administrator username chosen during first-run setup.
pub(crate) fn validate_username(username: &str) -> Result<(), DomainError> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err(DomainError::InvalidUsername { reason: "blank" });
    }
    if trimmed.chars().count() > MAX_USERNAME_CHARS {
        return Err(DomainError::InvalidUsername { reason: "too_long" });
    }
    if trimmed.chars().any(char::is_control) {
        return Err(DomainError::InvalidUsername {
            reason: "invalid_char",
        });
    }

    Ok(())
}

/// Builds the admin record from credentials the user chose during setup.
///
/// Only the Argon2id hash of the password is persisted. A user-chosen password
/// is not a generated one-time credential, so no forced change applies.
pub(crate) fn new_record(username: String, password: &str) -> Result<AdminRecord, DomainError> {
    let now = now_unix();
    Ok(AdminRecord {
        username,
        password_hash: crypto::hash_password(password)?,
        must_change_password: false,
        created_at: now,
        updated_at: now,
    })
}

pub(crate) fn encode(record: &AdminRecord) -> Result<Vec<u8>, DomainError> {
    serde_json::to_vec(record).map_err(|source| DomainError::Serialization {
        operation: "encode admin record",
        source,
    })
}

pub(crate) fn decode(plaintext: &[u8]) -> Result<AdminRecord, DomainError> {
    serde_json::from_slice(plaintext).map_err(|error| DomainError::Corrupted {
        detail: format!("admin record is not valid JSON: {error}"),
    })
}

/// Checks the username first, then verifies the Argon2id password hash.
pub(crate) fn authenticate(
    record: &AdminRecord,
    username: &str,
    password: &str,
) -> Result<bool, DomainError> {
    if record.username != username.trim() {
        return Ok(false);
    }
    crypto::verify_password(&record.password_hash, password)
}

/// Replaces the stored password hash after verifying the current password and
/// clearing the forced-password-change flag.
pub(crate) fn update_password(
    record: &mut AdminRecord,
    new_password: &str,
) -> Result<(), DomainError> {
    record.password_hash = crypto::hash_password(new_password)?;
    record.must_change_password = false;
    record.updated_at = now_unix();
    Ok(())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::validate_password;
    use crate::error::DomainError;

    fn reason(password: &str) -> Option<&'static str> {
        match validate_password(password) {
            Ok(()) => None,
            Err(DomainError::WeakPassword { reason }) => Some(reason),
            Err(error) => panic!("unexpected validation error: {error}"),
        }
    }

    #[test]
    fn password_policy_requires_letters_and_digits() {
        assert_eq!(reason("abc123"), None);
        assert_eq!(reason("Abc 123"), None);
        assert_eq!(reason("a!1b2c"), None);
        assert_eq!(reason("abc12"), Some("too_short"));
        assert_eq!(reason("abcdef"), Some("missing_digit"));
        assert_eq!(reason("123456"), Some("missing_letter"));
        assert_eq!(reason("密码abc123"), Some("invalid_char"));
        assert_eq!(reason("abc\t123"), Some("invalid_char"));

        let too_long = format!("a1{}", "a".repeat(1023));
        assert_eq!(reason(&too_long), Some("too_long"));
    }

    #[test]
    fn password_policy_rejects_edge_whitespace() {
        assert_eq!(reason(" abc123"), Some("edge_whitespace"));
        assert_eq!(reason("abc123 "), Some("edge_whitespace"));
        assert_eq!(reason("      "), Some("blank"));
    }
}
