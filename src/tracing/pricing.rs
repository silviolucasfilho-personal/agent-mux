//! Model matching and cost computation — the part of Langfuse's server that
//! has no server to live in any more. A wide price row per model (USD per
//! 1M tokens), exact-or-prefix matching over normalized model names, and a
//! four-bucket cost formula over `NormalizedUsage`.

use crate::config::ModelPriceConfig;
use crate::tracing::usage::NormalizedUsage;
use serde::Deserialize;

pub const BUILTIN_PRICING_TOML: &str = include_str!("pricing.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Builtin,
    Config,
    User,
}

impl PriceSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PriceSource::Builtin => "builtin",
            PriceSource::Config => "config",
            PriceSource::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPrice {
    pub id: String,
    pub provider: String,
    /// Lowercase patterns; trailing `*` = prefix match.
    pub matches: Vec<String>,
    pub input_per_m: f64,
    pub output_per_m: f64,
    /// `None` = 0.
    pub cache_read_per_m: Option<f64>,
    /// 5-minute TTL rate; `None` = 0.
    pub cache_write_per_m: Option<f64>,
    /// `None` = same as `cache_write_per_m`.
    pub cache_write_1h_per_m: Option<f64>,
    /// `None` = reasoning tokens are billed inside output.
    pub reasoning_per_m: Option<f64>,
    pub source: PriceSource,
    pub updated_at: String,
}

fn guess_provider(id: &str) -> &'static str {
    let id = id.to_ascii_lowercase();
    if id.starts_with("claude") {
        "anthropic"
    } else if id.starts_with("gpt")
        || id.starts_with("o1")
        || id.starts_with("o3")
        || id.starts_with("o4")
        || id.contains("codex")
    {
        "openai"
    } else if id.starts_with("gemini") {
        "google"
    } else {
        "other"
    }
}

impl ModelPrice {
    pub fn from_config(c: &ModelPriceConfig, source: PriceSource, updated_at: &str) -> ModelPrice {
        let matches: Vec<String> = if c.matches.is_empty() {
            vec![c.id.trim().to_ascii_lowercase()]
        } else {
            c.matches
                .iter()
                .map(|m| m.trim().to_ascii_lowercase())
                .filter(|m| !m.is_empty())
                .collect()
        };
        ModelPrice {
            id: c.id.trim().to_string(),
            provider: c
                .provider
                .clone()
                .unwrap_or_else(|| guess_provider(&c.id).to_string()),
            matches,
            input_per_m: c.input,
            output_per_m: c.output,
            cache_read_per_m: c.cache_read,
            cache_write_per_m: c.cache_write,
            cache_write_1h_per_m: c.cache_write_1h,
            reasoning_per_m: c.reasoning,
            source,
            updated_at: updated_at.to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PricingFile {
    updated_at: String,
    #[serde(default)]
    models: Vec<ModelPriceConfig>,
}

/// Per-observation cost, USD.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cost {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub total: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct PriceTable {
    models: Vec<ModelPrice>,
}

impl PriceTable {
    pub fn empty() -> PriceTable {
        PriceTable { models: Vec::new() }
    }

    /// The bundled table. A malformed bundle is a build defect, pinned by
    /// `builtin_table_parses`; at runtime it degrades to an empty table.
    pub fn builtin() -> PriceTable {
        Self::parse_toml(BUILTIN_PRICING_TOML, PriceSource::Builtin).unwrap_or_default()
    }

    pub fn parse_toml(text: &str, source: PriceSource) -> Result<PriceTable, String> {
        let file: PricingFile = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(PriceTable {
            models: file
                .models
                .iter()
                .map(|c| ModelPrice::from_config(c, source, &file.updated_at))
                .collect(),
        })
    }

    pub fn from_models(models: Vec<ModelPrice>) -> PriceTable {
        PriceTable { models }
    }

    /// Overlays `[[tracing.models]]` rows: same id replaces, new id appends.
    pub fn with_overrides(mut self, overrides: &[ModelPriceConfig]) -> PriceTable {
        for c in overrides {
            let row = ModelPrice::from_config(c, PriceSource::Config, "config");
            if let Some(existing) = self.models.iter_mut().find(|m| m.id == row.id) {
                *existing = row;
            } else {
                self.models.push(row);
            }
        }
        self
    }

    pub fn models(&self) -> &[ModelPrice] {
        &self.models
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Exact pattern match on the normalized name first, then the longest
    /// matching prefix pattern. `<synthetic>` and empty names never match.
    pub fn find(&self, model: &str) -> Option<&ModelPrice> {
        let name = normalize_model_name(model);
        if name.is_empty() || name.starts_with('<') {
            return None;
        }
        if let Some(exact) = self
            .models
            .iter()
            .find(|m| m.matches.iter().any(|p| !p.ends_with('*') && *p == name))
        {
            return Some(exact);
        }
        let mut best: Option<(&ModelPrice, usize)> = None;
        for m in &self.models {
            for p in &m.matches {
                if let Some(prefix) = p.strip_suffix('*')
                    && name.starts_with(prefix)
                    && best.is_none_or(|(_, len)| prefix.len() > len)
                {
                    best = Some((m, prefix.len()));
                }
            }
        }
        best.map(|(m, _)| m)
    }

    /// Patterns claimed by more than one model id (a doctor warning).
    pub fn overlapping_patterns(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (i, a) in self.models.iter().enumerate() {
            for b in &self.models[i + 1..] {
                for p in &a.matches {
                    if b.matches.contains(p) {
                        out.push((p.clone(), a.id.clone(), b.id.clone()));
                    }
                }
            }
        }
        out
    }
}

/// Strips a trailing `-yyyymmdd`, `@yyyymmdd`, or `-yyyy-mm-dd`.
fn strip_date_suffix(s: &str) -> &str {
    let b = s.as_bytes();
    let all_digits = |slice: &[u8]| !slice.is_empty() && slice.iter().all(u8::is_ascii_digit);
    if b.len() > 9
        && (b[b.len() - 9] == b'-' || b[b.len() - 9] == b'@')
        && all_digits(&b[b.len() - 8..])
    {
        return &s[..s.len() - 9];
    }
    if b.len() > 11 {
        let tail = &b[b.len() - 11..];
        if tail[0] == b'-'
            && all_digits(&tail[1..5])
            && tail[5] == b'-'
            && all_digits(&tail[6..8])
            && tail[8] == b'-'
            && all_digits(&tail[9..11])
        {
            return &s[..s.len() - 11];
        }
    }
    s
}

/// Lowercase; provider prefixes, `[1m]`, Bedrock `-v1:0`, and date
/// suffixes stripped.
pub fn normalize_model_name(name: &str) -> String {
    let mut s = name.trim().to_ascii_lowercase();
    for prefix in ["anthropic/", "openai/", "google/", "models/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
        }
    }
    for region in ["us.", "eu.", "apac.", "au.", "jp.", "global."] {
        if let Some(rest) = s.strip_prefix(region) {
            s = rest.to_string();
            break;
        }
    }
    if let Some(rest) = s.strip_prefix("anthropic.") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_suffix("[1m]") {
        s = rest.to_string();
    }
    for suffix in ["-v1:0", "-v1"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.to_string();
            break;
        }
    }
    let stripped = strip_date_suffix(&s).to_string();
    // a second pass covers `name-20250929-v1:0`-style orderings
    match stripped
        .strip_suffix("-v1:0")
        .or_else(|| stripped.strip_suffix("-v1"))
    {
        Some(rest) => strip_date_suffix(rest).to_string(),
        None => stripped,
    }
}

fn per_m(tokens: i64, rate: f64) -> f64 {
    tokens as f64 * rate / 1_000_000.0
}

/// The four-bucket formula from the spec.
pub fn cost_for(price: &ModelPrice, usage: &NormalizedUsage) -> Cost {
    let input = usage.input.map(|t| per_m(t, price.input_per_m));
    let cache_read = usage
        .cache_read
        .map(|t| per_m(t, price.cache_read_per_m.unwrap_or(0.0)));
    let cache_write = usage.cache_write.map(|total| {
        let w1h = usage.cache_write_1h.unwrap_or(0).clamp(0, total.max(0));
        let w5 = total - w1h;
        let rate5 = price.cache_write_per_m.unwrap_or(0.0);
        let rate1h = price.cache_write_1h_per_m.unwrap_or(rate5);
        per_m(w5, rate5) + per_m(w1h, rate1h)
    });
    let output = usage.output.map(|out| match price.reasoning_per_m {
        None => per_m(out, price.output_per_m),
        Some(rr) => {
            let reasoning = usage.reasoning.unwrap_or(0).clamp(0, out.max(0));
            per_m(out - reasoning, price.output_per_m) + per_m(reasoning, rr)
        }
    });
    let parts = [input, output, cache_read, cache_write];
    let total = if parts.iter().all(Option::is_none) {
        None
    } else {
        Some(parts.iter().flatten().sum())
    };
    Cost {
        input,
        output,
        cache_read,
        cache_write,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn builtin_table_parses_and_has_no_overlaps() {
        let table = PriceTable::parse_toml(BUILTIN_PRICING_TOML, PriceSource::Builtin).unwrap();
        assert!(table.models().len() >= 10);
        assert!(
            table.overlapping_patterns().is_empty(),
            "{:?}",
            table.overlapping_patterns()
        );
        assert!(
            table
                .models()
                .iter()
                .all(|m| m.source == PriceSource::Builtin)
        );
    }

    #[test]
    fn name_normalization_vectors() {
        for (input, want) in [
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("anthropic/claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            (
                "us.anthropic.claude-opus-4-1-20250805-v1:0",
                "claude-opus-4-1",
            ),
            ("claude-opus-4-6[1m]", "claude-opus-4-6"),
            ("claude-sonnet-4-5@20250929", "claude-sonnet-4-5"),
            ("Claude-Opus-5", "claude-opus-5"),
            ("openai/gpt-5.3-codex", "gpt-5.3-codex"),
            ("claude-3-5-sonnet-2024-10-22", "claude-3-5-sonnet"),
            ("<synthetic>", "<synthetic>"),
        ] {
            assert_eq!(normalize_model_name(input), want, "{input}");
        }
    }

    #[test]
    fn exact_beats_prefix_and_synthetic_never_matches() {
        let table = PriceTable::builtin();
        assert_eq!(
            table.find("claude-sonnet-4-5-20250929").unwrap().id,
            "claude-sonnet-4-5"
        );
        assert_eq!(
            table.find("claude-sonnet-4-20250514").unwrap().id,
            "claude-sonnet-4"
        );
        assert_eq!(
            table.find("claude-opus-4-1-20250805").unwrap().id,
            "claude-opus-4-1"
        );
        assert_eq!(table.find("claude-opus-5").unwrap().id, "claude-opus-5");
        assert_eq!(
            table.find("claude-fable-5-1").unwrap().id,
            "claude-fable-5-1"
        );
        assert_eq!(table.find("gpt-5.3-codex").unwrap().id, "gpt-5.3-codex");
        assert!(table.find("<synthetic>").is_none());
        assert!(table.find("").is_none());
        assert!(table.find("totally-unknown-model").is_none());
    }

    #[test]
    fn cost_vectors() {
        let table = PriceTable::builtin();
        let sonnet = table.find("claude-sonnet-4-5").unwrap();
        let usage = NormalizedUsage {
            input: Some(1_000_000),
            output: Some(100_000),
            cache_read: Some(2_000_000),
            cache_write: Some(200_000),
            cache_write_1h: Some(100_000),
            reasoning: None,
            total: Some(3_300_000),
        };
        let cost = cost_for(sonnet, &usage);
        assert!(approx(cost.input, 3.0));
        assert!(approx(cost.output, 1.5));
        assert!(approx(cost.cache_read, 0.6));
        // 100k at 3.75 + 100k at 6.00
        assert!(approx(cost.cache_write, 0.375 + 0.6), "{cost:?}");
        assert!(approx(cost.total, 3.0 + 1.5 + 0.6 + 0.975));
        // reasoning priced separately when configured
        let mut priced = sonnet.clone();
        priced.reasoning_per_m = Some(30.0);
        let usage = NormalizedUsage {
            output: Some(1_000_000),
            reasoning: Some(400_000),
            ..Default::default()
        };
        let cost = cost_for(&priced, &usage);
        assert!(approx(cost.output, 0.6 * 15.0 + 0.4 * 30.0));
        assert!(cost.input.is_none());
        // empty usage: no cost
        assert_eq!(cost_for(sonnet, &NormalizedUsage::default()).total, None);
    }

    #[test]
    fn config_overrides_builtin_by_id_and_appends_new() {
        let over = vec![
            ModelPriceConfig {
                id: "claude-sonnet-5".into(),
                provider: None,
                matches: vec![],
                input: 9.0,
                output: 9.0,
                cache_read: None,
                cache_write: None,
                cache_write_1h: None,
                reasoning: None,
            },
            ModelPriceConfig {
                id: "my-local".into(),
                provider: Some("other".into()),
                matches: vec!["my-local*".into()],
                input: 0.0,
                output: 0.0,
                cache_read: None,
                cache_write: None,
                cache_write_1h: None,
                reasoning: None,
            },
        ];
        let before = PriceTable::builtin().models().len();
        let table = PriceTable::builtin().with_overrides(&over);
        assert_eq!(table.models().len(), before + 1);
        let sonnet = table.find("claude-sonnet-5").unwrap();
        assert_eq!(sonnet.input_per_m, 9.0);
        assert_eq!(sonnet.source, PriceSource::Config);
        assert_eq!(sonnet.provider, "anthropic");
        assert_eq!(table.find("my-local-7b").unwrap().id, "my-local");
    }
}
