//! Provider-aware token usage normalization into DISJOINT billable buckets
//! whose sum is `total`.
//!
//! - Claude (`message.usage`): `input_tokens` already EXCLUDES cache
//!   tokens; `cache_read_input_tokens` / `cache_creation_input_tokens` are
//!   additive. The nested `cache_creation.ephemeral_{5m,1h}_input_tokens`
//!   breakdown (flattened by the parser into dotted keys) splits writes by
//!   TTL.
//! - Codex (`token_count.last_token_usage`): `input_tokens` INCLUDES
//!   `cached_input_tokens`, `output_tokens` INCLUDES
//!   `reasoning_output_tokens` (OpenAI semantics).
//! - Antigravity: interactive transcripts carry no usage.

use crate::transcript::Provider;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedUsage {
    /// Uncached, billable input tokens.
    pub input: Option<i64>,
    /// Output tokens (reasoning included where the provider counts it so).
    pub output: Option<i64>,
    pub cache_read: Option<i64>,
    /// All cache writes, every TTL.
    pub cache_write: Option<i64>,
    /// Subset of `cache_write` billed at the 1-hour rate (Claude).
    pub cache_write_1h: Option<i64>,
    /// Informational subset of `output` (Codex).
    pub reasoning: Option<i64>,
    pub total: Option<i64>,
}

impl NormalizedUsage {
    pub fn is_empty(&self) -> bool {
        self.input.is_none()
            && self.output.is_none()
            && self.cache_read.is_none()
            && self.cache_write.is_none()
            && self.reasoning.is_none()
            && self.total.is_none()
    }
}

fn get(raw: &[(String, i64)], key: &str) -> Option<i64> {
    raw.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}

fn sum_present(parts: [Option<i64>; 4]) -> Option<i64> {
    if parts.iter().all(Option::is_none) {
        None
    } else {
        Some(parts.iter().flatten().sum())
    }
}

/// Normalizes the raw integer usage keys of one API call.
pub fn normalize(provider: Provider, raw: &[(String, i64)]) -> NormalizedUsage {
    if raw.is_empty() {
        return NormalizedUsage::default();
    }
    match provider {
        Provider::Codex => {
            let raw_input = get(raw, "input_tokens");
            let cached = get(raw, "cached_input_tokens");
            let input = raw_input.map(|i| (i - cached.unwrap_or(0)).max(0));
            let cache_write = get(raw, "cache_write_input_tokens");
            let output = get(raw, "output_tokens");
            let reasoning = get(raw, "reasoning_output_tokens");
            let total = get(raw, "total_tokens")
                .or_else(|| sum_present([input, cached, cache_write, output]));
            NormalizedUsage {
                input,
                output,
                cache_read: cached,
                cache_write,
                cache_write_1h: None,
                reasoning,
                total,
            }
        }
        // agy's per-request record (see `agy_usage`): `prompt_tokens` is
        // the uncached input, `context_tokens` the whole context, so the
        // difference is what the prefix cache served.
        Provider::Antigravity
            if get(raw, "prompt_tokens").is_some() || get(raw, "context_tokens").is_some() =>
        {
            let input = get(raw, "prompt_tokens");
            let output = get(raw, "output_tokens");
            let reasoning = get(raw, "thoughts_tokens");
            let cache_read = match (get(raw, "context_tokens"), input) {
                (Some(ctx), Some(p)) => Some((ctx - p).max(0)),
                _ => None,
            };
            let total = sum_present([input, cache_read, None, output]);
            NormalizedUsage {
                input,
                output,
                cache_read,
                cache_write: None,
                cache_write_1h: None,
                reasoning,
                total,
            }
        }
        // Anthropic semantics; Antigravity `-p` shapes are not modeled.
        Provider::Claude | Provider::Antigravity => {
            let input = get(raw, "input_tokens");
            let cache_read = get(raw, "cache_read_input_tokens");
            let w5 = get(raw, "cache_creation.ephemeral_5m_input_tokens");
            let w1h = get(raw, "cache_creation.ephemeral_1h_input_tokens");
            let cache_write = get(raw, "cache_creation_input_tokens").or_else(|| {
                if w5.is_none() && w1h.is_none() {
                    None
                } else {
                    Some(w5.unwrap_or(0) + w1h.unwrap_or(0))
                }
            });
            let output = get(raw, "output_tokens");
            let total = sum_present([input, cache_read, cache_write, output]);
            NormalizedUsage {
                input,
                output,
                cache_read,
                cache_write,
                cache_write_1h: w1h,
                reasoning: None,
                total,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn claude_keeps_uncached_input_and_adds_cache_buckets() {
        let u = normalize(
            Provider::Claude,
            &raw(&[
                ("input_tokens", 3),
                ("cache_read_input_tokens", 45_000),
                ("cache_creation_input_tokens", 1_200),
                ("cache_creation.ephemeral_5m_input_tokens", 1_000),
                ("cache_creation.ephemeral_1h_input_tokens", 200),
                ("output_tokens", 250),
            ]),
        );
        // NOT max(3 - 45000 - 1200, 0): Anthropic input excludes cache
        assert_eq!(u.input, Some(3));
        assert_eq!(u.cache_read, Some(45_000));
        assert_eq!(u.cache_write, Some(1_200));
        assert_eq!(u.cache_write_1h, Some(200));
        assert_eq!(u.output, Some(250));
        assert_eq!(u.reasoning, None);
        assert_eq!(u.total, Some(3 + 45_000 + 1_200 + 250));
    }

    #[test]
    fn claude_cache_write_falls_back_to_ttl_breakdown() {
        let u = normalize(
            Provider::Claude,
            &raw(&[
                ("input_tokens", 10),
                ("cache_creation.ephemeral_5m_input_tokens", 700),
                ("output_tokens", 5),
            ]),
        );
        assert_eq!(u.cache_write, Some(700));
        assert_eq!(u.cache_write_1h, None);
        assert_eq!(u.total, Some(715));
    }

    #[test]
    fn codex_subtracts_cached_from_input_and_keeps_reasoning_inside_output() {
        let u = normalize(
            Provider::Codex,
            &raw(&[
                ("input_tokens", 200),
                ("cached_input_tokens", 50),
                ("cache_write_input_tokens", 0),
                ("output_tokens", 20),
                ("reasoning_output_tokens", 5),
                ("total_tokens", 220),
            ]),
        );
        assert_eq!(u.input, Some(150));
        assert_eq!(u.cache_read, Some(50));
        assert_eq!(u.cache_write, Some(0));
        assert_eq!(u.output, Some(20));
        assert_eq!(u.reasoning, Some(5));
        assert_eq!(u.total, Some(220));
        // missing total: sum of the disjoint buckets
        let u = normalize(
            Provider::Codex,
            &raw(&[
                ("input_tokens", 100),
                ("cached_input_tokens", 40),
                ("output_tokens", 10),
            ]),
        );
        assert_eq!(u.total, Some(60 + 40 + 10));
        // cached larger than input (should not happen) floors at 0
        let u = normalize(
            Provider::Codex,
            &raw(&[("input_tokens", 5), ("cached_input_tokens", 9)]),
        );
        assert_eq!(u.input, Some(0));
    }

    #[test]
    fn antigravity_conversation_db_keys_derive_cache_from_context() {
        let u = normalize(
            Provider::Antigravity,
            &raw(&[
                ("prompt_tokens", 5716),
                ("output_tokens", 88),
                ("thoughts_tokens", 26),
                ("text_tokens", 62),
                ("context_tokens", 28339),
                ("context_window", 256000),
            ]),
        );
        assert_eq!(u.input, Some(5716));
        assert_eq!(u.cache_read, Some(28339 - 5716));
        assert_eq!(u.output, Some(88));
        assert_eq!(u.reasoning, Some(26));
        assert_eq!(u.total, Some(28339 + 88));
        assert_eq!(u.cache_write, None);
    }

    #[test]
    fn empty_and_antigravity_are_empty() {
        assert!(normalize(Provider::Claude, &[]).is_empty());
        assert!(normalize(Provider::Antigravity, &[]).is_empty());
        // unknown keys only: nothing normalizes, total stays None
        let u = normalize(
            Provider::Claude,
            &raw(&[("server_tool_use.web_search_requests", 2)]),
        );
        assert!(u.is_empty());
    }
}
