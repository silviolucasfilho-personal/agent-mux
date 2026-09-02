//! Hand-built OTLP/JSON encoding for `POST {host}/api/public/otel/v1/traces`.
//!
//! The sharp edges this module owns (pinned by the golden tests below):
//! trace/span ids are lowercase hex strings (the OTLP special case that
//! overrides proto3-JSON base64 for bytes), 64-bit nanos are decimal
//! *strings*, field names are lowerCamelCase, enum fields (status code) are
//! integers, and attribute values use the `AnyValue` wrapper shape.

use serde::Serialize;
use uuid::Uuid;

/// Fixed namespace for all deterministic agent-mux ids. Never change this
/// constant: trace/span ids for re-exported data would stop lining up.
pub const AMX_NS: Uuid = Uuid::from_u128(0x8f2f_1c65_9a3d_4e8b_b1a4_7c5d_2e90_66aa_u128);

/// Deterministic 16-byte trace id: UUIDv5 (SHA-1) of `key` under `AMX_NS`.
pub fn trace_id_for(key: &str) -> [u8; 16] {
    *Uuid::new_v5(&AMX_NS, key.as_bytes()).as_bytes()
}

/// Deterministic 8-byte span id: first half of a UUIDv5 of `key`.
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

/// RFC 4648 standard base64 with padding, encode-only.
pub fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// `Authorization` header value for Langfuse basic auth.
pub fn basic_auth(public_key: &str, secret_key: &str) -> String {
    format!(
        "Basic {}",
        base64_encode(format!("{public_key}:{secret_key}").as_bytes())
    )
}

/// OTLP AnyValue. Serialized as `{"stringValue": ...}` etc.
#[derive(Debug, Clone, PartialEq)]
pub enum AnyValue {
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    Array(Vec<AnyValue>),
}

impl Serialize for AnyValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            // proto3 JSON: int64 as decimal string
            AnyValue::Str(s) => map.serialize_entry("stringValue", s)?,
            AnyValue::Int(n) => map.serialize_entry("intValue", &n.to_string())?,
            AnyValue::Double(d) => map.serialize_entry("doubleValue", d)?,
            AnyValue::Bool(b) => map.serialize_entry("boolValue", b)?,
            AnyValue::Array(values) => {
                #[derive(Serialize)]
                struct ArrayValue<'a> {
                    values: &'a [AnyValue],
                }
                map.serialize_entry("arrayValue", &ArrayValue { values })?
            }
        }
        map.end()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyValue {
    pub key: String,
    pub value: AnyValue,
}

pub fn attr(key: &str, value: AnyValue) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value,
    }
}

pub fn str_attr(key: &str, value: impl Into<String>) -> KeyValue {
    attr(key, AnyValue::Str(value.into()))
}

pub const STATUS_UNSET: i32 = 0;
pub const STATUS_ERROR: i32 = 2;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpanStatus {
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    /// Unix epoch nanoseconds. i128 to hold `time`'s full range; clamped to
    /// u64 on the wire (post-1970 timestamps always fit).
    pub start_nanos: i128,
    pub end_nanos: i128,
    pub attributes: Vec<KeyValue>,
    pub status_code: i32,
    pub status_message: Option<String>,
}

fn nanos_str(nanos: i128) -> String {
    u64::try_from(nanos).unwrap_or(0).to_string()
}

impl Serialize for Span {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("traceId", &hex(&self.trace_id))?;
        map.serialize_entry("spanId", &hex(&self.span_id))?;
        if let Some(parent) = &self.parent_span_id {
            map.serialize_entry("parentSpanId", &hex(parent))?;
        }
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("startTimeUnixNano", &nanos_str(self.start_nanos))?;
        map.serialize_entry("endTimeUnixNano", &nanos_str(self.end_nanos))?;
        map.serialize_entry("attributes", &self.attributes)?;
        map.serialize_entry(
            "status",
            &SpanStatus {
                code: self.status_code,
                message: self.status_message.clone(),
            },
        )?;
        map.end()
    }
}

/// Serializes a batch of spans as a full `ExportTraceServiceRequest` body.
pub fn build_request(spans: &[Span]) -> String {
    #[derive(Serialize)]
    struct Resource {
        attributes: Vec<KeyValue>,
    }
    #[derive(Serialize)]
    struct Scope {
        name: &'static str,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ScopeSpans<'a> {
        scope: Scope,
        spans: &'a [Span],
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ResourceSpans<'a> {
        resource: Resource,
        scope_spans: [ScopeSpans<'a>; 1],
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Request<'a> {
        resource_spans: [ResourceSpans<'a>; 1],
    }
    let request = Request {
        resource_spans: [ResourceSpans {
            resource: Resource {
                attributes: vec![
                    str_attr("service.name", "agent-mux"),
                    str_attr("service.version", env!("CARGO_PKG_VERSION")),
                ],
            },
            scope_spans: [ScopeSpans {
                scope: Scope { name: "agent-mux" },
                spans,
            }],
        }],
    };
    serde_json::to_string(&request).unwrap_or_else(|_| "{}".to_string())
}

/// Rough byte-size estimate used for batch splitting.
pub fn estimated_size(span: &Span) -> usize {
    // ids + name + fixed overhead, plus attribute payloads
    128 + span.name.len()
        + span
            .attributes
            .iter()
            .map(|a| a.key.len() + any_value_size(&a.value) + 32)
            .sum::<usize>()
}

fn any_value_size(v: &AnyValue) -> usize {
    match v {
        AnyValue::Str(s) => s.len(),
        AnyValue::Int(_) | AnyValue::Double(_) | AnyValue::Bool(_) => 8,
        AnyValue::Array(items) => items.iter().map(any_value_size).sum::<usize>() + 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_rfc4648_vectors() {
        // RFC 4648 §10 test vectors
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn basic_auth_encodes_pk_colon_sk() {
        assert_eq!(basic_auth("pk", "sk"), format!("Basic {}", base64_encode(b"pk:sk")));
    }

    #[test]
    fn id_derivation_is_stable_across_releases() {
        // Pinned vectors: these exact values must NEVER change — they are
        // what makes re-exports idempotent across agent-mux versions.
        assert_eq!(AMX_NS.to_string(), "8f2f1c65-9a3d-4e8b-b1a4-7c5d2e9066aa");
        // literal pins, cross-checked against Python's uuid.uuid5 — a
        // self-referential assertion would pin nothing
        assert_eq!(
            hex(&trace_id_for("amx1|claude|abc|turn|1")),
            "f2be4d548b6e5bd3872f4c81004daee1"
        );
        assert_eq!(hex(&span_id_for("amx1|claude|abc|turn|1")), "f2be4d548b6e5bd3");
        // determinism + distinctness
        assert_eq!(trace_id_for("x"), trace_id_for("x"));
        assert_ne!(trace_id_for("x"), trace_id_for("y"));
        assert_eq!(span_id_for("x"), span_id_for("x"));
        assert_eq!(span_id_for("x")[..], trace_id_for("x")[..8]);
    }

    #[test]
    fn golden_serialized_request() {
        let span = Span {
            trace_id: [0xab; 16],
            span_id: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            parent_span_id: Some([0xff; 8]),
            name: "turn 1".into(),
            start_nanos: 1_788_118_098_806_000_000,
            end_nanos: 1_788_118_100_000_000_000,
            attributes: vec![
                str_attr("langfuse.session.id", "sess-1"),
                attr("gen_ai.usage.input_tokens", AnyValue::Int(100)),
                attr("ok", AnyValue::Bool(true)),
                attr(
                    "langfuse.trace.tags",
                    AnyValue::Array(vec![AnyValue::Str("agent-mux".into())]),
                ),
            ],
            status_code: STATUS_ERROR,
            status_message: Some("boom".into()),
        };
        let body = build_request(std::slice::from_ref(&span));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let s = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        // hex ids: 32 / 16 lowercase hex chars, not base64
        assert_eq!(s["traceId"], "abababababababababababababababab");
        assert_eq!(s["spanId"], "0102030405060708");
        assert_eq!(s["parentSpanId"], "ffffffffffffffff");
        // 64-bit nanos as decimal strings
        assert_eq!(s["startTimeUnixNano"], "1788118098806000000");
        assert_eq!(s["endTimeUnixNano"], "1788118100000000000");
        // AnyValue wrapper shapes; intValue is a string per proto3 JSON
        assert_eq!(s["attributes"][0]["key"], "langfuse.session.id");
        assert_eq!(s["attributes"][0]["value"]["stringValue"], "sess-1");
        assert_eq!(s["attributes"][1]["value"]["intValue"], "100");
        assert_eq!(s["attributes"][2]["value"]["boolValue"], true);
        assert_eq!(
            s["attributes"][3]["value"]["arrayValue"]["values"][0]["stringValue"],
            "agent-mux"
        );
        // status code is an integer enum
        assert_eq!(s["status"]["code"], 2);
        assert_eq!(s["status"]["message"], "boom");
        // resource block
        let res = &v["resourceSpans"][0]["resource"]["attributes"];
        assert_eq!(res[0]["key"], "service.name");
        assert_eq!(res[0]["value"]["stringValue"], "agent-mux");
        // no parent -> field omitted entirely
        let orphan = Span {
            parent_span_id: None,
            status_code: STATUS_UNSET,
            status_message: None,
            ..span
        };
        let body = build_request(&[orphan]);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let s = &v["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert!(s.get("parentSpanId").is_none());
        assert!(s["status"].get("message").is_none());
        assert_eq!(s["status"]["code"], 0);
    }
}
