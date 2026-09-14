//! Authenticated cursor tokens for pagination and filter binding.

use super::scope::Scope;
use super::service::ServiceError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Decoded cursor payload containing schema, validity window, filter digest and pagination position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPayload {
    pub schema_version: u32,
    pub issued_at_ns: i64,
    pub expires_at_ns: i64,
    pub filters_hash: String,
    pub upper_bound_ns: i64,
    pub sort_key: String,
    pub offset: usize,
}

/// Secret-keyed encoder and validator for opaque cursors.
#[derive(Debug, Clone)]
pub struct CursorCodec {
    key: [u8; 32],
}

impl CursorCodec {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Computes an authentication HMAC/hash for given payload bytes.
    fn compute_tag(&self, payload_bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.key);
        hasher.update(b"cursor_auth_v1");
        hasher.update(payload_bytes);
        let result = hasher.finalize();
        result.into()
    }

    /// Encodes and signs a cursor payload into an opaque token string.
    pub fn encode(&self, payload: &CursorPayload) -> String {
        let json = serde_json::to_vec(payload).expect("cursor payload serialization cannot fail");
        let tag = self.compute_tag(&json);
        format!("{}.{}", hex_encode(&json), hex_encode(&tag))
    }

    /// Validates signature, filter hash, and expiry time, returning the decoded payload.
    pub fn decode(
        &self,
        token: &str,
        expected_filters_hash: &str,
        now_ns: i64,
    ) -> Result<CursorPayload, ServiceError> {
        let (payload_hex, tag_hex) = token
            .split_once('.')
            .ok_or_else(|| ServiceError::InvalidArgument("invalid cursor format".into()))?;

        let payload_bytes = hex_decode(payload_hex)
            .ok_or_else(|| ServiceError::InvalidArgument("invalid cursor hex encoding".into()))?;
        let tag_bytes = hex_decode(tag_hex)
            .ok_or_else(|| ServiceError::InvalidArgument("invalid cursor tag hex encoding".into()))?;

        let expected_tag = self.compute_tag(&payload_bytes);
        if tag_bytes.as_slice() != expected_tag.as_slice() {
            return Err(ServiceError::InvalidArgument("tampered or invalid cursor".into()));
        }

        let payload: CursorPayload = serde_json::from_slice(&payload_bytes)
            .map_err(|e| ServiceError::InvalidArgument(format!("corrupt cursor payload: {e}")))?;

        if now_ns > payload.expires_at_ns {
            return Err(ServiceError::CursorExpired(
                "cursor expired; please refresh query".into(),
            ));
        }

        if payload.filters_hash != expected_filters_hash {
            return Err(ServiceError::InvalidArgument(
                "cursor filter parameters mismatch".into(),
            ));
        }

        Ok(payload)
    }
}

/// Computes a stable hash for a tool call's filter criteria to bind to the cursor.
pub fn compute_filters_hash(tool_name: &str, scope: &Scope, filter_details: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_name.as_bytes());
    hasher.update(b":");
    if scope.is_all_workspaces() {
        hasher.update(b"all_workspaces");
    } else if let Some(ws) = scope.workspace_path() {
        hasher.update(ws.to_string_lossy().as_bytes());
    }
    hasher.update(b":");
    hasher.update(filter_details.as_bytes());
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        write!(&mut s, "{:02x}", b).unwrap();
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
