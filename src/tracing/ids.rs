//! Deterministic ids (UUIDv5 under a fixed namespace). The vectors pinned
//! in the tests must never change: they are what makes re-imports and
//! re-parses converge on the same rows across agent-mux releases.

use uuid::Uuid;

/// Fixed namespace for all deterministic agent-mux ids. Never change this
/// constant.
pub const AMX_NS: Uuid = Uuid::from_u128(0x8f2f_1c65_9a3d_4e8b_b1a4_7c5d_2e90_66aa_u128);

/// Deterministic 16-byte trace id: UUIDv5 (SHA-1) of `key` under `AMX_NS`.
pub fn trace_id_for(key: &str) -> [u8; 16] {
    *Uuid::new_v5(&AMX_NS, key.as_bytes()).as_bytes()
}

/// Deterministic 8-byte observation id: first half of a UUIDv5 of `key`.
pub fn span_id_for(key: &str) -> [u8; 8] {
    let bytes = Uuid::new_v5(&AMX_NS, key.as_bytes()).into_bytes();
    let mut id = [0u8; 8];
    id.copy_from_slice(&bytes[..8]);
    id
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// 32 lowercase hex chars.
pub fn trace_id_hex(key: &str) -> String {
    hex(&trace_id_for(key))
}

/// 16 lowercase hex chars.
pub fn span_id_hex(key: &str) -> String {
    hex(&span_id_for(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_derivation_is_stable_across_releases() {
        // Pinned vectors: these exact values must NEVER change — they are
        // what makes re-imports idempotent across agent-mux versions.
        assert_eq!(AMX_NS.to_string(), "8f2f1c65-9a3d-4e8b-b1a4-7c5d2e9066aa");
        assert_eq!(
            trace_id_hex("amx1|claude|abc|turn|1"),
            "f2be4d548b6e5bd3872f4c81004daee1"
        );
        assert_eq!(span_id_hex("amx1|claude|abc|turn|1"), "f2be4d548b6e5bd3");
        assert_eq!(trace_id_for("x"), trace_id_for("x"));
        assert_ne!(trace_id_for("x"), trace_id_for("y"));
        assert_eq!(span_id_for("x")[..], trace_id_for("x")[..8]);
    }
}
