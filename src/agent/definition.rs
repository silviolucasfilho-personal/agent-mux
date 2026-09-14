use crate::harness::Harness;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Error encountered when parsing or validating an agent definition package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionError {
    pub file: PathBuf,
    pub field: Option<String>,
    pub message: String,
}

impl DefinitionError {
    pub fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            file: path.to_path_buf(),
            field: None,
            message: message.into(),
        }
    }

    pub fn field(path: &Path, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            file: path.to_path_buf(),
            field: Some(field.into()),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.field {
            Some(field) => write!(
                f,
                "Error in '{}' [field '{}']: {}",
                self.file.display(),
                field,
                self.message
            ),
            None => write!(f, "Error in '{}': {}", self.file.display(), self.message),
        }
    }
}

impl std::error::Error for DefinitionError {}

/// Definition of an agent discovered on disk or bundled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub description: String,
    pub harnesses: Vec<Harness>,
    pub default_harness: Harness,
    pub capabilities: Vec<String>,
    pub startup_task: Option<String>,
    pub mcp_servers: Vec<String>,
    pub instructions: String,
    pub source_hash: String,
    pub file_path: Option<PathBuf>,
    pub is_builtin: bool,
    pub triggers: Option<serde_yaml::Value>,
}

impl Default for AgentDefinition {
    fn default() -> Self {
        Self {
            id: "heimdall".into(),
            name: "Heimdall".into(),
            icon: Some("⚡".into()),
            description: "Omniscient monitor, executive morning briefings, and skill optimizer".into(),
            harnesses: Harness::ALL.to_vec(),
            default_harness: Harness::Antigravity,
            capabilities: vec!["trace.read".to_string()],
            startup_task: Some(
                "Read the current session briefing and report progress, blockers and evidence coverage."
                    .into(),
            ),
            mcp_servers: vec!["agent-mux".into()],
            instructions: String::new(),
            source_hash: String::new(),
            file_path: None,
            is_builtin: true,
            triggers: None,
        }
    }
}

impl AgentDefinition {
    /// Compatibility parser for legacy flat `.agent-mux/agents/*.md` files.
    pub fn parse_markdown(content: &str, file_path: Option<&Path>) -> Self {
        parse_legacy_definition(content, file_path).unwrap_or_else(|_| {
            let fallback_id = file_path
                .and_then(|p| p.file_stem())
                .and_then(|s| s.to_str())
                .unwrap_or("agent");
            AgentDefinition {
                id: fallback_id.to_string(),
                name: capitalize_name(fallback_id),
                icon: None,
                description: String::new(),
                harnesses: Harness::ALL.to_vec(),
                default_harness: Harness::Antigravity,
                capabilities: Vec::new(),
                startup_task: None,
                mcp_servers: Vec::new(),
                instructions: content.to_string(),
                source_hash: String::new(),
                file_path: file_path.map(|p| p.to_path_buf()),
                is_builtin: false,
                triggers: None,
            }
        })
    }
}

#[derive(Debug, serde::Deserialize)]
struct FrontmatterRaw {
    id: Option<String>,
    name: Option<String>,
    icon: Option<String>,
    description: Option<String>,
    harnesses: Option<Vec<Harness>>,
    default_harness: Option<Harness>,
    capabilities: Option<Vec<String>>,
    startup_task: Option<String>,
    mcp_servers: Option<Vec<String>>,
    triggers: Option<serde_yaml::Value>,
    #[serde(flatten)]
    unknown: BTreeMap<String, serde_yaml::Value>,
}

/// Validates whether an agent identifier matches `^[a-z0-9][a-z0-9_-]*$`.
pub fn is_valid_agent_id(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    for c in chars {
        if !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_' && c != '-' {
            return false;
        }
    }
    true
}

/// Parses and validates a canonical `AGENTS.md` package.
pub fn parse_definition(source: &str, path: &Path) -> Result<AgentDefinition, DefinitionError> {
    let trimmed = source.trim_start();
    if !trimmed.starts_with("---") {
        return Err(DefinitionError::new(
            path,
            "missing required YAML frontmatter starting with '---'",
        ));
    }

    let end_idx = trimmed[3..].find("---").ok_or_else(|| {
        DefinitionError::new(path, "unterminated YAML frontmatter: missing closing '---'")
    })?;

    let frontmatter_str = &trimmed[3..3 + end_idx];
    let body_str = trimmed[3 + end_idx + 3..].trim();

    if body_str.is_empty() {
        return Err(DefinitionError::field(
            path,
            "instructions",
            "instructions body must not be empty",
        ));
    }

    let raw: FrontmatterRaw = serde_yaml::from_str(frontmatter_str).map_err(|e| {
        DefinitionError::field(
            path,
            "frontmatter",
            format!("failed to parse YAML frontmatter: {e}"),
        )
    })?;

    for key in raw.unknown.keys() {
        eprintln!(
            "Warning: unknown frontmatter field '{}' in agent package '{}'",
            key,
            path.display()
        );
    }

    // Validate ID
    let fallback_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("agent");

    let id = raw.id.unwrap_or_else(|| fallback_id.to_string());
    if !is_valid_agent_id(&id) {
        return Err(DefinitionError::field(
            path,
            "id",
            format!(
                "invalid agent id '{id}': must be non-empty and match ^[a-z0-9][a-z0-9_-]*$"
            ),
        ));
    }

    // Validate harnesses
    let harnesses = match raw.harnesses {
        Some(h) if !h.is_empty() => h,
        _ => {
            return Err(DefinitionError::field(
                path,
                "harnesses",
                "at least one supported harness must be declared in 'harnesses'",
            ));
        }
    };

    // Check duplicate harnesses
    let mut seen_harnesses = HashSet::new();
    for h in &harnesses {
        if !seen_harnesses.insert(*h) {
            return Err(DefinitionError::field(
                path,
                "harnesses",
                format!("duplicate harness declared in 'harnesses': '{h}'"),
            ));
        }
    }

    // Validate default_harness
    let default_harness = match raw.default_harness {
        Some(dh) => {
            if !harnesses.contains(&dh) {
                return Err(DefinitionError::field(
                    path,
                    "default_harness",
                    format!("default_harness '{dh}' must be declared in 'harnesses'"),
                ));
            }
            dh
        }
        None => harnesses[0],
    };

    // Compute deterministic SHA-256 hash of canonical source bytes
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let source_hash = format!("{:x}", hasher.finalize());

    let name = raw.name.unwrap_or_else(|| capitalize_name(&id));
    let description = raw.description.unwrap_or_default();
    let capabilities = raw.capabilities.unwrap_or_default();
    let startup_task = raw.startup_task;
    let mcp_servers = raw.mcp_servers.unwrap_or_default();

    Ok(AgentDefinition {
        id,
        name,
        icon: raw.icon,
        description,
        harnesses,
        default_harness,
        capabilities,
        startup_task,
        mcp_servers,
        instructions: body_str.to_string(),
        source_hash,
        file_path: Some(path.to_path_buf()),
        is_builtin: false,
        triggers: raw.triggers,
    })
}

/// Compatibility parser for legacy flat `.agent-mux/agents/*.md` files.
pub fn parse_legacy_definition(
    source: &str,
    file_path: Option<&Path>,
) -> Result<AgentDefinition, DefinitionError> {
    let fallback_id = file_path
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("agent")
        .to_string();

    let trimmed = source.trim_start();
    if trimmed.starts_with("---") {
        if let Some(end_idx) = trimmed[3..].find("---") {
            let frontmatter_str = &trimmed[3..3 + end_idx];
            let body_str = trimmed[3 + end_idx + 3..].trim();

            if let Ok(raw) = serde_yaml::from_str::<FrontmatterRaw>(frontmatter_str) {
                let id = raw.id.unwrap_or_else(|| fallback_id.clone());
                let harnesses = raw.harnesses.unwrap_or_else(|| Harness::ALL.to_vec());
                let default_harness = raw
                    .default_harness
                    .filter(|dh| harnesses.contains(dh))
                    .or_else(|| harnesses.first().copied())
                    .unwrap_or(Harness::Antigravity);

                let mut hasher = Sha256::new();
                hasher.update(source.as_bytes());
                let source_hash = format!("{:x}", hasher.finalize());

                return Ok(AgentDefinition {
                    id: id.clone(),
                    name: raw.name.unwrap_or_else(|| capitalize_name(&id)),
                    icon: raw.icon,
                    description: raw.description.unwrap_or_default(),
                    harnesses,
                    default_harness,
                    capabilities: raw.capabilities.unwrap_or_default(),
                    startup_task: raw.startup_task,
                    mcp_servers: raw.mcp_servers.unwrap_or_default(),
                    instructions: body_str.to_string(),
                    source_hash,
                    file_path: file_path.map(|p| p.to_path_buf()),
                    is_builtin: false,
                    triggers: raw.triggers,
                });
            }
        }
    }

    // Unadorned markdown
    let body = trimmed;
    let mut description = String::new();
    for line in body.lines() {
        let l = line.trim();
        if !l.is_empty() && !l.starts_with('#') {
            description = l.to_string();
            break;
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let source_hash = format!("{:x}", hasher.finalize());

    Ok(AgentDefinition {
        id: fallback_id.clone(),
        name: capitalize_name(&fallback_id),
        icon: None,
        description,
        harnesses: Harness::ALL.to_vec(),
        default_harness: Harness::Antigravity,
        capabilities: Vec::new(),
        startup_task: None,
        mcp_servers: Vec::new(),
        instructions: body.to_string(),
        source_hash,
        file_path: file_path.map(|p| p.to_path_buf()),
        is_builtin: false,
        triggers: None,
    })
}

pub(crate) fn capitalize_name(s: &str) -> String {
    let mut parts = Vec::new();
    for word in s.replace(['-', '_'], " ").split_whitespace() {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            parts.push(format!("{}{}", first.to_uppercase(), chars.as_str()));
        }
    }
    if parts.is_empty() {
        s.to_string()
    } else {
        parts.join(" ")
    }
}
