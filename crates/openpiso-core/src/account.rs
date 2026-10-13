use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Deleted,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UsernameError {
    #[error("username must be between 3 and 32 characters")]
    InvalidLength,
    #[error("username may contain only ASCII letters, digits, '.', '_', and '-'")]
    InvalidCharacters,
}

pub fn normalize_username(username: &str) -> Result<(String, String), UsernameError> {
    let display = username.trim();
    if !(3..=32).contains(&display.len()) {
        return Err(UsernameError::InvalidLength);
    }
    if !display
        .bytes()
        .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'.' | b'_' | b'-'))
    {
        return Err(UsernameError::InvalidCharacters);
    }
    Ok((display.to_owned(), display.to_ascii_lowercase()))
}

/// Lenient normaliser for user-facing input (login, search).
///
/// Trims whitespace, collapses internal whitespace runs to `_`, then applies
/// the strict [`normalize_username`] rule. This makes typed spaces unambiguous
/// — they always map to `_` — so a customer can type their Username with a
/// space at login and still reach their Account, and the owner can search the
/// same way.
///
/// Everywhere else (signup, account creation, transfer recipient lookup)
/// keeps the strict rule, so no stored Username ever contains a space.
pub fn normalize_username_lenient(username: &str) -> Result<(String, String), UsernameError> {
    let collapsed: String = username.split_whitespace().collect::<Vec<_>>().join("_");
    normalize_username(&collapsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_identity_is_trimmed_and_case_insensitive() {
        assert_eq!(
            normalize_username("  Juan_01 ").expect("valid username"),
            ("Juan_01".to_owned(), "juan_01".to_owned())
        );
    }

    #[test]
    fn username_rejects_ambiguous_characters() {
        assert_eq!(
            normalize_username("Juan Smith"),
            Err(UsernameError::InvalidCharacters)
        );
    }
}
