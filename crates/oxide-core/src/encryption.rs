//! Passwords and encryption methods.
//!
//! Passwords are held in zeroizing memory and never appear in `Debug`
//! output, so they cannot leak through logs or panic messages.

use std::fmt;

use serde::Serialize;
use zeroize::Zeroizing;

/// A secret archive password. Wiped from memory when dropped.
#[derive(Clone)]
pub struct Password(Zeroizing<String>);

impl Password {
    /// Wraps a password string.
    pub fn new(secret: impl Into<String>) -> Self {
        Password(Zeroizing::new(secret.into()))
    }

    /// Returns the password text. Callers must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True if the password is the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

impl From<&str> for Password {
    fn from(value: &str) -> Self {
        Password::new(value)
    }
}

impl From<String> for Password {
    fn from(value: String) -> Self {
        Password::new(value)
    }
}

/// Encryption schemes Oxide can write.
///
/// Reading also accepts legacy ZipCrypto archives, but Oxide never creates
/// them because the scheme is cryptographically broken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncryptionMethod {
    /// WinZip AES-128 (ZIP only).
    Aes128,
    /// WinZip AES-256 for ZIP, or 7z's AES-256 + SHA-256 key derivation.
    Aes256,
}

impl EncryptionMethod {
    /// Display name.
    pub fn name(self) -> &'static str {
        match self {
            EncryptionMethod::Aes128 => "AES-128",
            EncryptionMethod::Aes256 => "AES-256",
        }
    }
}

impl fmt::Display for EncryptionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_password() {
        let pw = Password::new("hunter2");
        let shown = format!("{pw:?}");
        assert!(!shown.contains("hunter2"));
        assert_eq!(pw.expose(), "hunter2");
    }
}
