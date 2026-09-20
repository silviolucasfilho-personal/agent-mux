//! The remote control's bearer token.
//!
//! One token per run, held in memory only unless `[remote] token` pins it.
//! It is the single thing standing between a TCP port and every session's
//! keyboard, so: 256 bits of randomness, a constant-time compare, and a
//! `Debug` that never prints it.

/// A bearer token. Compared with [`Token::verify`], never with `==`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// 32 random bytes as base64url (43 chars, no padding, URL-clean so it
    /// survives a query string without percent-encoding).
    pub fn generate() -> Token {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Token(base64_url_nopad(&bytes))
    }

    /// A token from configuration. Rejects anything too short to resist
    /// guessing; the caller falls back to [`Token::generate`].
    pub fn from_config(value: &str) -> Result<Token, String> {
        let t = value.trim();
        if t.chars().count() < 16 {
            return Err("token must be at least 16 characters".into());
        }
        Ok(Token(t.to_string()))
    }

    /// The token as it goes into a URL. Deliberately not `Display`: every
    /// caller has to name what it is doing with the secret.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Constant-time equality against a candidate from the wire.
    pub fn verify(&self, candidate: &str) -> bool {
        ct_eq(self.0.as_bytes(), candidate.as_bytes())
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(****)")
    }
}

/// Constant-time byte comparison. Length is not secret (the token's length
/// is fixed and public), so an early return on a length mismatch is fine;
/// the content comparison is what must not short-circuit.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding. The crate's `base64` dependency would do it
/// too; this keeps the token module free of any engine configuration that
/// a future edit could get wrong.
fn base64_url_nopad(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18) as usize & 63] as char);
        out.push(B64URL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[n as usize & 63] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_long_url_safe_and_unique() {
        let a = Token::generate();
        let b = Token::generate();
        assert_eq!(a.expose().len(), 43);
        assert_ne!(a.expose(), b.expose());
        assert!(
            a.expose()
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "not url-safe: {}",
            a.expose()
        );
    }

    #[test]
    fn verify_accepts_only_the_exact_token() {
        let t = Token::from_config("a-token-long-enough").unwrap();
        assert!(t.verify("a-token-long-enough"));
        assert!(!t.verify("a-token-long-enougH"));
        assert!(!t.verify("a-token-long-enough "));
        assert!(!t.verify(""));
    }

    #[test]
    fn ct_eq_matches_normal_equality() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn from_config_rejects_a_short_token() {
        assert!(Token::from_config("short").is_err());
        assert!(Token::from_config("0123456789abcdef").is_ok());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let t = Token::from_config("super-secret-value").unwrap();
        assert_eq!(format!("{t:?}"), "Token(****)");
        assert!(!format!("{t:?}").contains("secret"));
    }

    #[test]
    fn base64_url_matches_known_vectors() {
        assert_eq!(base64_url_nopad(b""), "");
        assert_eq!(base64_url_nopad(b"f"), "Zg");
        assert_eq!(base64_url_nopad(b"fo"), "Zm8");
        assert_eq!(base64_url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_url_nopad(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_url_nopad(&[0xfb, 0xff, 0xfe]), "-__-");
    }
}
