//! Capability-token access control for local HTTP servers.
//!
//! Binding to loopback only stops *remote* access; any other local process or
//! user on the same machine can still reach a bound TCP port, and a
//! `Host`-header check only stops a browser-driven DNS-rebinding attack, not a
//! script that constructs its own request. This is the same class of bug as
//! classic unauthenticated Jupyter servers, so callers mint a random
//! capability per server instance and require it on every sensitive request
//! instead of relying on network topology alone.
//!
//! First used by `enwiro-daemon`'s host-side Claude auth proxy (issue #540);
//! also used by `enw-gui` to gate its own local API.

use std::collections::HashSet;
use std::sync::Mutex;

use subtle::ConstantTimeEq;

/// Length, in bytes, of a minted capability token before hex-encoding (32
/// bytes = 256 bits, comfortably above the ~128-bit floor for a bearer secret).
pub const CAPABILITY_BYTES: usize = 32;

/// The set of currently-valid capability tokens for one server instance.
/// In-memory only: no persistence, no cross-restart carryover.
pub struct CapabilitySet {
    tokens: Mutex<HashSet<String>>,
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self {
            tokens: Mutex::new(HashSet::new()),
        }
    }

    /// Mint a fresh random capability token, register it as valid, and
    /// return it. Never returns a token that isn't also recorded as valid.
    pub fn mint(&self) -> String {
        let mut bytes = [0u8; CAPABILITY_BYTES];
        rand::fill(&mut bytes);
        let token = hex_encode(&bytes);
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token.clone());
        token
    }

    /// True iff `candidate` matches one of this set's valid tokens. Compares
    /// in constant time (no early-exit on the first differing byte, which
    /// would otherwise leak timing information about how much of a guess
    /// matched) and checks every registered token rather than
    /// short-circuiting on the first candidate.
    pub fn contains(&self, candidate: &str) -> bool {
        let candidate = candidate.as_bytes();
        let tokens = self
            .tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut valid = subtle::Choice::from(0u8);
        for token in tokens.iter() {
            // A length mismatch alone isn't secret, so it's fine to check
            // (and skip) before the constant-time byte comparison.
            if token.len() == candidate.len() {
                valid |= token.as_bytes().ct_eq(candidate);
            }
        }
        valid.into()
    }

    /// True iff `headers` carries `Authorization: Bearer <token>` for a
    /// valid token.
    pub fn is_authorized(&self, headers: &http::HeaderMap) -> bool {
        bearer_token(headers).is_some_and(|token| self.contains(token))
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Extract the bearer value from an `Authorization: Bearer <token>` header,
/// or `None` if absent/malformed.
fn bearer_token(headers: &http::HeaderMap) -> Option<&str> {
    headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer_headers(token: &str) -> http::HeaderMap {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn minted_capability_is_hex_and_correct_length() {
        let set = CapabilitySet::new();
        let token = set.mint();
        assert_eq!(token.len(), CAPABILITY_BYTES * 2, "{token}");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
    }

    #[test]
    fn mint_produces_distinct_tokens() {
        let set = CapabilitySet::new();
        assert_ne!(set.mint(), set.mint());
    }

    #[test]
    fn a_minted_capability_authorizes() {
        let set = CapabilitySet::new();
        let token = set.mint();
        assert!(set.contains(&token));
        assert!(set.is_authorized(&bearer_headers(&token)));
    }

    #[test]
    fn an_unminted_token_does_not_authorize() {
        let set = CapabilitySet::new();
        set.mint();
        assert!(!set.contains("not-a-real-capability"));
    }

    #[test]
    fn missing_authorization_header_does_not_authorize() {
        let set = CapabilitySet::new();
        set.mint();
        assert!(!set.is_authorized(&http::HeaderMap::new()));
    }

    #[test]
    fn non_bearer_authorization_does_not_authorize() {
        let set = CapabilitySet::new();
        set.mint();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        assert!(!set.is_authorized(&headers));
    }

    #[test]
    fn tokens_from_different_sets_do_not_cross_authorize() {
        let a = CapabilitySet::new();
        let b = CapabilitySet::new();
        let token = a.mint();
        assert!(!b.contains(&token));
    }
}
