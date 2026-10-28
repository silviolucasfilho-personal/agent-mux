//! Bounded workspace directory scanner and deterministic project inspector.
//!
//! Reads manifests (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`,
//! etc.), documentation leads (`README.md`, `AGENTS.md`, `CLAUDE.md`), local
//! agent configurations (`.agent-mux/agents/*.toml`, `.claude`, `.codex`,
//! `.gemini`, `gate.yaml`, `loop.md`) and `.git/HEAD` without spawning child
//! processes during the tree walk.

use super::{
    CustomizationInfo, FolderActivity, GitInfo, GitKind, ProjectPersona, ProjectSummary,
    SCAN_MAX_VISITED, SummarySource, TechStack, WorkspaceNode, summary_cache,
};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Directories the scanner never descends into as workspace rows.
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    "venv",
    ".venv",
    "vendor",
    "Pods",
    ".gradle",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    "coverage",
    "tmp",
];

/// Scans `root` up to `max_depth` levels and returns the ordered tree of
/// [`WorkspaceNode`]s (root at index 0, followed by discovered folders in
/// alphabetical depth-first tree order).
pub fn scan_workspace(
    root: &Path,
    max_depth: usize,
    library_root: Option<&Path>,
    active_dirs: &HashSet<PathBuf>,
) -> Vec<WorkspaceNode> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut visited = 0usize;
    let mut raw_nodes: Vec<WorkspaceNode> = Vec::new();

    let (stack, git, customization, mut summary, is_project) =
        inspect_folder(&root_canon, library_root);
    let root_name = root_canon
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| root_canon.display().to_string());
    if matches!(summary.source, SummarySource::Synthesized) && !is_project {
        summary.text = "Workspace root directory.".into();
    }
    raw_nodes.push(WorkspaceNode {
        path: root_canon.clone(),
        name: root_name,
        rel_path: ".".into(),
        depth: 0,
        is_project,
        child_project_count: 0,
        stack,
        git,
        customization,
        summary,
        activity: FolderActivity::default(),
    });

    // When `root` itself is already a project folder, stop here instead of
    // exploring its internal subdirectories.
    if !is_project {
        walk_children(
            &root_canon,
            &root_canon,
            1,
            max_depth,
            library_root,
            active_dirs,
            &mut visited,
            &mut raw_nodes,
        );
    }

    // Populate `child_project_count` and refine synthesized summaries for
    // parent folders that hold multiple sub-projects.
    for i in 0..raw_nodes.len() {
        let parent_path = raw_nodes[i].path.clone();
        let mut child_projects = 0usize;
        let mut child_langs: Vec<String> = Vec::new();
        for other in raw_nodes.iter().skip(i + 1) {
            if other.path.starts_with(&parent_path) {
                if other.is_project {
                    child_projects += 1;
                    for l in &other.stack.languages {
                        if !child_langs.contains(l) {
                            child_langs.push(l.clone());
                        }
                    }
                }
            } else {
                break;
            }
        }
        raw_nodes[i].child_project_count = child_projects;
        if matches!(raw_nodes[i].summary.source, SummarySource::Synthesized)
            && raw_nodes[i].stack.is_empty()
            && child_projects > 0
        {
            let langs_note = if child_langs.is_empty() {
                String::new()
            } else {
                format!(" ({})", child_langs.join(", "))
            };
            raw_nodes[i].summary.text = format!(
                "Contains {child_projects} sub-project{}{langs_note}.",
                if child_projects == 1 { "" } else { "s" }
            );
        }
    }

    raw_nodes
}

#[allow(clippy::too_many_arguments)]
fn walk_children(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    library_root: Option<&Path>,
    active_dirs: &HashSet<PathBuf>,
    visited: &mut usize,
    out: &mut Vec<WorkspaceNode>,
) {
    if depth > max_depth || *visited >= SCAN_MAX_VISITED {
        return;
    }
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    let mut subdirs: Vec<PathBuf> = read_dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let ft = e.file_type().ok()?;
            if !ft.is_dir() {
                return None;
            }
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                return None;
            }
            Some(e.path())
        })
        .collect();
    subdirs.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .cmp(&b.file_name().unwrap_or_default().to_ascii_lowercase())
    });

    for dir in subdirs {
        if *visited >= SCAN_MAX_VISITED {
            break;
        }
        *visited += 1;
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let (stack, git, customization, summary, is_project) = inspect_folder(&canon, library_root);
        let has_activity = active_dirs
            .iter()
            .any(|a| a == &canon || a.starts_with(&canon));

        // Stop descending as soon as we find a project folder.
        let mut children_nodes: Vec<WorkspaceNode> = Vec::new();
        if !is_project {
            walk_children(
                root,
                &canon,
                depth + 1,
                max_depth,
                library_root,
                active_dirs,
                visited,
                &mut children_nodes,
            );
        }

        let include_self = is_project || has_activity || !children_nodes.is_empty();
        if include_self {
            let name = canon
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| canon.display().to_string());
            let rel_path = canon
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.clone());
            out.push(WorkspaceNode {
                path: canon,
                name,
                rel_path,
                depth,
                is_project,
                child_project_count: 0,
                stack,
                git,
                customization,
                summary,
                activity: FolderActivity::default(),
            });
            out.extend(children_nodes);
        }
    }
}

/// Inspects a single folder for its technology stack, git info, agent
/// customizations, and summary.
pub fn inspect_folder(
    dir: &Path,
    library_root: Option<&Path>,
) -> (TechStack, GitInfo, CustomizationInfo, ProjectSummary, bool) {
    let stack = detect_tech_stack(dir);
    let git = inspect_git(dir);
    let customization = inspect_customizations(dir, library_root);
    let is_project = !stack.is_empty()
        || matches!(git.kind, GitKind::RepoRoot | GitKind::Worktree)
        || customization.has_any();
    let summary = build_summary(dir, &stack, &customization, library_root);
    (stack, git, customization, summary, is_project)
}

pub fn detect_tech_stack(dir: &Path) -> TechStack {
    let mut stack = TechStack::default();

    // 1. Rust (Cargo.toml)
    let cargo_toml = dir.join("Cargo.toml");
    if cargo_toml.is_file() {
        push_unique(&mut stack.languages, "Rust");
        push_unique(&mut stack.tools, "Cargo");
        if let Ok(raw) = fs::read_to_string(&cargo_toml)
            && let Ok(val) = toml::from_str::<toml::Value>(&raw)
        {
            if let Some(pkg) = val.get("package").and_then(|v| v.as_table()) {
                if stack.package_name.is_none() {
                    stack.package_name =
                        pkg.get("name").and_then(|v| v.as_str()).map(str::to_string);
                }
                if stack.edition_or_version.is_none() {
                    stack.edition_or_version = pkg
                        .get("edition")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                if stack.manifest_description.is_none() {
                    stack.manifest_description = pkg
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
            }
            if let Some(ws) = val.get("workspace").and_then(|v| v.as_table()) {
                if let Some(members) = ws.get("members").and_then(|v| v.as_array()) {
                    for m in members.iter().filter_map(|x| x.as_str()) {
                        stack.workspace_members.push(m.to_string());
                    }
                }
                if stack.edition_or_version.is_none() {
                    stack.edition_or_version = ws
                        .get("package")
                        .and_then(|p| p.get("edition"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            let mut dep_names: HashSet<String> = HashSet::new();
            for section in [
                val.get("dependencies"),
                val.get("dev-dependencies"),
                val.get("workspace").and_then(|w| w.get("dependencies")),
            ]
            .into_iter()
            .flatten()
            {
                if let Some(tbl) = section.as_table() {
                    for k in tbl.keys() {
                        dep_names.insert(k.to_ascii_lowercase());
                    }
                }
            }
            for (crate_name, label) in [
                ("ratatui", "ratatui"),
                ("tokio", "tokio"),
                ("rusqlite", "SQLite"),
                ("sqlx", "SQLx"),
                ("axum", "Axum"),
                ("actix-web", "Actix"),
                ("tauri", "Tauri"),
                ("leptos", "Leptos"),
                ("dioxus", "Dioxus"),
                ("bevy", "Bevy"),
                ("tonic", "gRPC"),
                ("wasm-bindgen", "Wasm"),
            ] {
                if dep_names.contains(crate_name) {
                    push_unique(&mut stack.frameworks, label);
                }
            }
        }
    }

    // 2. Node / TypeScript / JavaScript (package.json, tsconfig.json)
    let package_json = dir.join("package.json");
    let has_tsconfig = dir.join("tsconfig.json").is_file();
    if has_tsconfig {
        push_unique(&mut stack.languages, "TypeScript");
    }
    if package_json.is_file() {
        if !has_tsconfig {
            push_unique(&mut stack.languages, "JavaScript");
        }
        if let Ok(raw) = fs::read_to_string(&package_json)
            && let Ok(val) = serde_json::from_str::<JsonValue>(&raw)
        {
            if stack.package_name.is_none() {
                stack.package_name = val.get("name").and_then(|v| v.as_str()).map(str::to_string);
            }
            if stack.manifest_description.is_none() {
                stack.manifest_description = val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
            let mut deps: HashSet<String> = HashSet::new();
            for section in ["dependencies", "devDependencies", "peerDependencies"] {
                if let Some(obj) = val.get(section).and_then(|v| v.as_object()) {
                    for k in obj.keys() {
                        deps.insert(k.to_ascii_lowercase());
                    }
                }
            }
            if deps.contains("typescript") {
                push_unique(&mut stack.languages, "TypeScript");
            }
            for (pkg, label) in [
                ("next", "Next.js"),
                ("react", "React"),
                ("vue", "Vue"),
                ("svelte", "Svelte"),
                ("@angular/core", "Angular"),
                ("astro", "Astro"),
                ("vite", "Vite"),
                ("express", "Express"),
                ("@nestjs/core", "NestJS"),
                ("fastify", "Fastify"),
                ("hono", "Hono"),
                ("tailwindcss", "Tailwind"),
                ("prisma", "Prisma"),
                ("drizzle-orm", "Drizzle"),
                ("electron", "Electron"),
            ] {
                if deps.contains(pkg) {
                    push_unique(&mut stack.frameworks, label);
                }
            }
        }
        if dir.join("pnpm-lock.yaml").is_file() || dir.join("pnpm-workspace.yaml").is_file() {
            push_unique(&mut stack.tools, "pnpm");
        } else if dir.join("bun.lockb").is_file() || dir.join("bun.lock").is_file() {
            push_unique(&mut stack.tools, "Bun");
        } else if dir.join("yarn.lock").is_file() {
            push_unique(&mut stack.tools, "Yarn");
        } else {
            push_unique(&mut stack.tools, "npm");
        }
    }
    if dir.join("deno.json").is_file() || dir.join("deno.jsonc").is_file() {
        push_unique(&mut stack.languages, "TypeScript");
        push_unique(&mut stack.tools, "Deno");
    }

    // 3. Python (pyproject.toml, requirements.txt, setup.py)
    let pyproject = dir.join("pyproject.toml");
    let requirements = dir.join("requirements.txt");
    if pyproject.is_file() || requirements.is_file() || dir.join("setup.py").is_file() {
        push_unique(&mut stack.languages, "Python");
        if dir.join("uv.lock").is_file() {
            push_unique(&mut stack.tools, "uv");
        }
        if pyproject.is_file()
            && let Ok(raw) = fs::read_to_string(&pyproject)
        {
            if let Ok(val) = toml::from_str::<toml::Value>(&raw)
                && let Some(proj) = val.get("project").and_then(|v| v.as_table())
            {
                if stack.package_name.is_none() {
                    stack.package_name = proj
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                if stack.manifest_description.is_none() {
                    stack.manifest_description = proj
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
            }
            let lower = raw.to_ascii_lowercase();
            for (needle, label) in [
                ("fastapi", "FastAPI"),
                ("django", "Django"),
                ("flask", "Flask"),
                ("torch", "PyTorch"),
                ("pytest", "pytest"),
                ("langchain", "LangChain"),
                ("pydantic", "Pydantic"),
            ] {
                if lower.contains(needle) {
                    push_unique(&mut stack.frameworks, label);
                }
            }
        }
        if requirements.is_file()
            && let Ok(raw) = fs::read_to_string(&requirements)
        {
            let lower = raw.to_ascii_lowercase();
            for (needle, label) in [
                ("fastapi", "FastAPI"),
                ("django", "Django"),
                ("flask", "Flask"),
                ("torch", "PyTorch"),
            ] {
                if lower.contains(needle) {
                    push_unique(&mut stack.frameworks, label);
                }
            }
        }
    }

    // 4. Go (go.mod, go.work)
    let go_mod = dir.join("go.mod");
    if go_mod.is_file() || dir.join("go.work").is_file() {
        push_unique(&mut stack.languages, "Go");
        if let Ok(raw) = fs::read_to_string(&go_mod) {
            for line in raw.lines() {
                let line = line.trim();
                if let Some(m) = line.strip_prefix("module ")
                    && stack.package_name.is_none()
                {
                    stack.package_name = Some(m.trim().to_string());
                } else if let Some(v) = line.strip_prefix("go ")
                    && stack.edition_or_version.is_none()
                {
                    stack.edition_or_version = Some(v.trim().to_string());
                }
            }
        }
    }

    // 5. Other ecosystems
    if dir.join("pom.xml").is_file() {
        push_unique(&mut stack.languages, "Java");
        push_unique(&mut stack.tools, "Maven");
    }
    if dir.join("build.gradle").is_file() || dir.join("build.gradle.kts").is_file() {
        push_unique(&mut stack.languages, "JVM");
        push_unique(&mut stack.tools, "Gradle");
    }
    if dir.join("Gemfile").is_file() {
        push_unique(&mut stack.languages, "Ruby");
    }
    if dir.join("mix.exs").is_file() {
        push_unique(&mut stack.languages, "Elixir");
    }
    if dir.join("composer.json").is_file() {
        push_unique(&mut stack.languages, "PHP");
    }
    if dir.join("Package.swift").is_file() {
        push_unique(&mut stack.languages, "Swift");
    }
    if dir.join("CMakeLists.txt").is_file() {
        push_unique(&mut stack.languages, "C/C++");
        push_unique(&mut stack.tools, "CMake");
    }

    // 6. Infra & build helpers
    if dir.join("Dockerfile").is_file()
        || dir.join("docker-compose.yml").is_file()
        || dir.join("compose.yaml").is_file()
    {
        push_unique(&mut stack.tools, "Docker");
    }
    if dir.join("flake.nix").is_file() || dir.join("shell.nix").is_file() {
        push_unique(&mut stack.tools, "Nix");
    }
    if dir.join("justfile").is_file() || dir.join("Justfile").is_file() {
        push_unique(&mut stack.tools, "Just");
    } else if dir.join("Makefile").is_file() {
        push_unique(&mut stack.tools, "Make");
    }

    stack
}

fn push_unique(vec: &mut Vec<String>, item: &str) {
    if !vec.iter().any(|x| x.eq_ignore_ascii_case(item)) {
        vec.push(item.to_string());
    }
}

/// Pure-Rust git inspection of `.git` so scanning an entire workspace tree
/// needs zero subprocesses.
pub fn inspect_git(dir: &Path) -> GitInfo {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        let branch = read_head_branch(&dot_git.join("HEAD"));
        return GitInfo {
            kind: GitKind::RepoRoot,
            branch,
            dirty_files: 0,
        };
    }
    if dot_git.is_file() {
        // Git worktree or submodule: `.git` contains `gitdir: /path/to/gitdir`
        let branch = fs::read_to_string(&dot_git)
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("gitdir:"))
                    .map(|p| PathBuf::from(p.trim()))
            })
            .map(|gitdir| {
                if gitdir.is_absolute() {
                    gitdir.join("HEAD")
                } else {
                    dir.join(gitdir).join("HEAD")
                }
            })
            .and_then(|head| read_head_branch(&head));
        return GitInfo {
            kind: GitKind::Worktree,
            branch,
            dirty_files: 0,
        };
    }
    GitInfo::default()
}

fn read_head_branch(head_path: &Path) -> Option<String> {
    let raw = fs::read_to_string(head_path).ok()?;
    let line = raw.lines().next()?.trim();
    if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
        Some(rest.trim().to_string())
    } else if line.len() >= 7 {
        Some(line[..7].to_string())
    } else {
        None
    }
}

/// Inspects agent customization markers in `dir`.
pub fn inspect_customizations(dir: &Path, library_root: Option<&Path>) -> CustomizationInfo {
    let agents_md = dir.join("AGENTS.md");
    let claude_md = dir.join("CLAUDE.md");
    let readme_md = dir.join("README.md");

    let has_agents_md = agents_md.is_file();
    let has_claude_md = claude_md.is_file();
    let has_readme_md = readme_md.is_file();
    let primary_doc = if has_agents_md {
        Some(agents_md)
    } else if has_claude_md {
        Some(claude_md)
    } else if has_readme_md {
        Some(readme_md)
    } else {
        None
    };

    let mut project_personas = Vec::new();
    let agents_dir = dir.join(".agent-mux").join("agents");
    if agents_dir.is_dir() {
        let lib = library_root
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::assets::root);
        let catalog = crate::agents::Catalog::load(&lib, Some(dir));
        for entry in catalog.entries {
            if matches!(entry.source, crate::agents::Source::Workspace(_)) {
                let task_label = entry
                    .spec
                    .as_ref()
                    .and_then(|s| s.task.as_ref())
                    .map(|t| t.label())
                    .filter(|s| !s.is_empty());
                let description = entry
                    .spec
                    .as_ref()
                    .map(|s| s.description.clone())
                    .unwrap_or_default();
                project_personas.push(ProjectPersona {
                    name: entry.name,
                    description,
                    task_label,
                });
            }
        }
    }

    let mut harness_dirs = Vec::new();
    for marker in [".claude", ".codex", ".gemini"] {
        if dir.join(marker).is_dir() {
            harness_dirs.push(marker.to_string());
        }
    }

    let has_gate_yaml =
        dir.join("gate.yaml").is_file() || dir.join(".agent-mux/gate.yaml").is_file();
    let has_loop_md = dir.join("loop.md").is_file();

    CustomizationInfo {
        has_agents_md,
        has_claude_md,
        has_readme_md,
        primary_doc,
        project_personas,
        harness_dirs,
        has_gate_yaml,
        has_loop_md,
    }
}

/// Builds the folder's summary from:
/// 1. Cached LLM summary (`~/.agent-mux/workspaces/<hash>.json`) if present,
/// 2. Manifest description (`Cargo.toml`, `package.json`, `pyproject.toml`),
/// 3. Lead paragraph from `README.md`, `AGENTS.md`, or `CLAUDE.md`,
/// 4. Synthesized fallback from detected stack/customizations.
pub fn build_summary(
    dir: &Path,
    stack: &TechStack,
    customization: &CustomizationInfo,
    library_root: Option<&Path>,
) -> ProjectSummary {
    if let Some(lib) = library_root
        && let Some(cached) = summary_cache::load(lib, dir)
    {
        return cached;
    }

    if let Some(desc) = &stack.manifest_description {
        return ProjectSummary {
            text: truncate_summary(desc, 280),
            source: SummarySource::Manifest,
            architecture: None,
            entry_points: detect_entry_points(dir),
        };
    }

    for doc_name in ["README.md", "AGENTS.md", "CLAUDE.md"] {
        let p = dir.join(doc_name);
        if p.is_file()
            && let Some(lead) = extract_doc_lead(&p)
        {
            return ProjectSummary {
                text: truncate_summary(&lead, 280),
                source: SummarySource::DocLead {
                    file: doc_name.to_string(),
                },
                architecture: None,
                entry_points: detect_entry_points(dir),
            };
        }
    }

    let text = if !stack.is_empty() {
        let mut s = format!("{} project", stack.detail_line());
        if !stack.workspace_members.is_empty() {
            s.push_str(&format!(
                " with {} workspace member{}",
                stack.workspace_members.len(),
                if stack.workspace_members.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        s.push('.');
        s
    } else if customization.has_any() {
        format!(
            "Agent configuration folder ({}).",
            customization.summary_badges().join(", ")
        )
    } else {
        "Sub-folder with no detected project manifest.".into()
    };

    ProjectSummary {
        text,
        source: SummarySource::Synthesized,
        architecture: None,
        entry_points: detect_entry_points(dir),
    }
}

fn detect_entry_points(dir: &Path) -> Vec<String> {
    const CANDIDATES: &[&str] = &[
        "src/main.rs",
        "src/lib.rs",
        "src/index.ts",
        "src/index.tsx",
        "src/main.ts",
        "app/page.tsx",
        "main.go",
        "cmd/main.go",
        "main.py",
        "app.py",
        "SKILL.md",
    ];
    CANDIDATES
        .iter()
        .filter(|rel| dir.join(rel).is_file())
        .take(4)
        .map(|s| (*s).to_string())
        .collect()
}

/// Extracts the first prose paragraph from a Markdown file, skipping YAML
/// frontmatter, headings, HTML tags, badges and code blocks.
pub fn extract_doc_lead(path: &Path) -> Option<String> {
    let raw = fs::read_to_string(path).ok()?;
    let mut lines = raw.lines().peekable();

    // Skip YAML frontmatter if present
    if lines.peek().map(|l| l.trim()) == Some("---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }

    let mut in_code_block = false;
    let mut paragraph: Vec<String> = Vec::new();

    for line in lines.take(80) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        // Skip headings, badges, HTML, tables, blockquotes and horizontal rules
        if trimmed.starts_with('#')
            || trimmed.starts_with("![")
            || trimmed.starts_with("[![")
            || trimmed.starts_with('<')
            || trimmed.starts_with('|')
            || trimmed.starts_with("---")
            || trimmed.starts_with("===")
        {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        let cleaned = strip_inline_markdown(trimmed);
        if !cleaned.is_empty() {
            paragraph.push(cleaned);
        }
    }

    if paragraph.is_empty() {
        None
    } else {
        Some(paragraph.join(" "))
    }
}

fn strip_inline_markdown(s: &str) -> String {
    let s = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .unwrap_or(s);
    // Replace `[label](url)` with `label`
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
        {
            let label_end = i + 1 + close_bracket;
            if label_end + 1 < chars.len()
                && chars[label_end + 1] == '('
                && let Some(close_paren) = chars[label_end + 2..].iter().position(|&c| c == ')')
            {
                let label: String = chars[i + 1..label_end].iter().collect();
                out.push_str(&label);
                i = label_end + 2 + close_paren + 1;
                continue;
            }
        }
        if chars[i] != '*' && chars[i] != '`' {
            out.push(chars[i]);
        }
        i += 1;
    }
    out.trim().to_string()
}

fn truncate_summary(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        head.push('…');
        head
    }
}
