# Langfuse Trace Extraction, TUI Viewer & Web Navigation — design

**Status:** Draft, 2026-11-01.
**Request:** "if langfuse is active, extract the tracing from there using the langfuse api, create a specific screen for this, create an option to launch the langfuse web from agent-mux"
**Builds on:**
- `docs/superpowers/specs/2026-08-30-langfuse-integration-design.md` (OTLP push exporter)
- `docs/superpowers/specs/2026-09-04-langfuse-backend-design.md` (per-launch sink routing, `ResolvedLangfuse`, `scores::export`)
- `docs/superpowers/specs/2026-10-11-consistent-keyboard-ux-design.md` (action registry, keymap conventions)
- `docs/superpowers/specs/2026-10-28-workspace-explorer-design.md` (dedicated screen layout and async inspection)

---

## 1. Executive Summary & Why

`agent-mux` supports Langfuse as an export destination (`backend = "langfuse"` or `backend = "both"`), pushing session turns, LLM generations, and tool calls as OTLP spans to `POST {host}/api/public/otel/v1/traces`. However, the flow is strictly one-directional:
1. **Blind export when running `backend = "langfuse"`**: sessions traced exclusively to Langfuse leave no trace rows in the local SQLite store (`~/.agent-mux/traces.db`), making them completely invisible in the built-in Trace Browser (`T`).
2. **No visibility into remote traces**: multi-user, CI/CD, or past workspace traces stored in Langfuse cannot be browsed, searched, or audited from inside `agent-mux`.
3. **No deep linking to Langfuse Web**: viewing a trace or session in the rich Langfuse Web UI requires switching to a browser, finding the project, and searching for the trace ID or session ID manually.

This specification introduces:
1. **Active Langfuse Detection & Health Probe**: dynamic detection of whether Langfuse is configured, reachable, and authenticated.
2. **Langfuse REST API Extraction Client (`src/tracing/langfuse/api.rs`)**: background, non-blocking ingestion of traces, observations (spans, generations, events), and sessions directly from Langfuse's public REST API (`GET /api/public/traces`, `GET /api/public/traces/{id}`, `GET /api/public/sessions`).
3. **Dedicated Langfuse Screen (`Mode::Langfuse`, `src/app/langfuse_view.rs`, `src/ui/langfuse.rs`)**: a responsive, 3-pane TUI viewer (Traces/Sessions list, Observation Hierarchy tree, and formatted Detail pane) with live search, filters, and token/cost roll-ups.
4. **Instant Web Launch (`OpenUrl` integration & CLI)**: seamless browser navigation to the project dashboard, specific traces, or sessions via `w`/`o` in the TUI, F1 command palette, and `agent-mux langfuse web` CLI.

---

## 2. Core Decisions & Invariants

### Decision 1: Respect the Non-Blocking TUI Invariant
The TUI must never freeze, stutter, or drop keystrokes due to network latency, HTTP timeouts, or DNS resolution against Langfuse Cloud or a self-hosted instance.
- All HTTP calls to the Langfuse REST API execute on a background worker thread or bounded task pool.
- Data arrives over an internal channel (`mpsc` or Tokio channel) into `LangfuseView`, transitioning through explicit states: `Unchecked` → `Loading` → `Ready(data)` or `Error(msg)`.
- UI interactions remain 60 FPS responsive even if Langfuse is down or experiencing a 429 rate limit.

### Decision 2: "Active" Definition & Graceful Fallback
Langfuse is considered **Active** when:
1. Credentials resolve (`ResolvedLangfuse`: `host`, `public_key`, `secret_key`), either via `[tracing.langfuse]` in configuration or via environment variables (`LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY`, `LANGFUSE_HOST`).
2. A lightweight background probe (`GET /api/public/traces?limit=1` or `/api/public/health`) validates reachability and authentication.

When Langfuse is **Inactive / Not Configured**:
- The F1 Command Overlay lists "Open Langfuse traces" with `Availability::Disabled("Langfuse credentials not configured")`.
- If opened directly, the screen displays a friendly setup guide showing the required environment variables and `profiles.toml` snippet, plus a one-key shortcut to open the documentation.

### Decision 3: Automatic Project ID Discovery for Deep Linking
Langfuse Web URLs require a `projectId` (e.g. `https://cloud.langfuse.com/project/{projectId}/traces/{traceId}`).
- Users often only configure `public_key` (`pk-lf-...`) and `secret_key` (`sk-lf-...`), neither of which is the project ID.
- The extraction client automatically extracts and caches `projectId` from the first returned trace object in `GET /api/public/traces`.
- An optional `project_id = "..."` field is supported in `[tracing.langfuse]` as an explicit override.
- Fallback: if no traces exist and `project_id` is unknown, browser navigation safely targets `{host}` (which auto-redirects to the active project when logged into Langfuse).

### Decision 4: Read-Through In-Memory Cache
- Browsing traces shouldn't re-fetch from the network repeatedly.
- `LangfuseCache` holds recent trace summaries and fetched trace observation trees with an LRU policy (default: up to 100 full trace trees).
- `Ctrl+R` clears the cache and triggers a fresh sync from the API.

### Decision 5: Unified Platform Opener
Browser launches reuse `App::open_url` and `(self.link_opener)(url)`, maintaining consistency across macOS (`open`), Linux (`xdg-open`), and Windows (`cmd /c start`), respecting test harnesses where browser calls are intercepted.

---

## 3. Architecture & Data Flow

```
                                  ┌───────────────────────────────┐
                                  │      Langfuse Cloud / Host    │
                                  │   (https://cloud.langfuse.com)│
                                  └───────▲───────────────┬───────┘
                                          │               │
                            OTLP Push     │               │ REST API Pull
                     POST /otel/v1/traces │               │ GET /api/public/traces
                                          │               │ GET /api/public/traces/{id}
                                          │               │
┌─────────────────────────┐      ┌────────┴─────────┐     │
│   agent-mux Sessions    │─────►│ Langfuse Exporter│     │
│ (Claude, Codex, AGY, ...)│      │   (Worker Sink)  │     │
└─────────────────────────┘      └──────────────────┘     │
                                                          ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ agent-mux TUI Engine                                                        │
│                                                                             │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │ Langfuse API Client (src/tracing/langfuse/api.rs)                   │   │
│   │   • Basic Auth: base64(pk:sk)                                       │   │
│   │   • Background thread runner with ureq                              │   │
│   │   • LRU Cache + Project ID Auto-detection                           │   │
│   └──────────────────────────────────┬──────────────────────────────────┘   │
│                                      │                                      │
│                                      ▼ MPSC events                          │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │ LangfuseView (Mode::Langfuse)                                       │   │
│   │   ┌───────────────────┬────────────────────┬────────────────────┐   │   │
│   │   │  Traces / Sessions│ Observation Tree   │ Detail & Inspector │   │   │
│   │   │  (Timestamp, Name,│ (Spans, Gens,      │ (Input, Output,    │   │   │
│   │   │   Tokens, Cost, $)│  Tools, Subagents) │  Usage, Metadata)  │   │   │
│   │   └───────────────────┴────────────────────┴────────────────────┘   │   │
│   │      ▲                                                           │   │
│   │      │ 'w' / 'o'                                                 │   │
│   │      └──────────────┐                                            │   │
│   └─────────────────────┼────────────────────────────────────────────┘   │
└─────────────────────────┼───────────────────────────────────────────────────┘
                          │
                          ▼
             App::open_url(deep_link)
                          │
                          ▼
            Default Browser opens trace:
   https://cloud.langfuse.com/project/<proj>/traces/<trace_id>
```

---

## 4. Langfuse REST API Client (`src/tracing/langfuse/api.rs`)

### 4.1 Endpoints Used

> **Superseded (2026-11-01, after implementation).** The table below is the
> original plan. Langfuse Cloud removes the v1 trace reads on 2026-11-16 and
> the current OpenAPI spec no longer lists `GET /api/public/traces` or
> `GET /api/public/traces/{id}`; `/api/public/health` answers without
> credentials, so it cannot check a key. The client now uses
> `GET /api/public/projects` for the credential check and
> `GET /api/public/v2/observations` (root observations for the list, all of a
> trace's observations for the detail, cursor paging) for reads. See the
> module docs of `src/tracing/langfuse/api.rs` and `docs/tracing.md`.

| Endpoint | Method | Purpose | Query Parameters |
|---|---|---|---|
| `/api/public/health` | `GET` | Instant connectivity & health verification | None |
| `/api/public/traces` | `GET` | List traces with pagination & filters | `page`, `limit` (max 100), `sessionId`, `name`, `userId`, `fromTimestamp`, `toTimestamp`, `tags`, `orderBy` (`timestamp.desc`) |
| `/api/public/traces/{traceId}` | `GET` | Fetch full trace detail + observation tree | None |
| `/api/public/sessions` | `GET` | List tracked sessions | `page`, `limit` |
| `/api/public/sessions/{sessionId}` | `GET` | Fetch session details | None |

### 4.2 Authentication & Request Construction
All requests use HTTP Basic Authentication:
```http
Authorization: Basic <base64(public_key:secret_key)>
Content-Type: application/json
User-Agent: agent-mux/<version>
x-langfuse-sdk-name: agent-mux
x-langfuse-sdk-version: <version>
```
Using the existing `crate::tracing::langfuse::basic_auth(pk, sk)` helper.

### 4.3 Data Structures

```rust
// In src/tracing/langfuse/api.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LangfuseTraceSummary {
    pub id: String,
    pub timestamp: String,
    pub name: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "userId")]
    pub user_id: Option<String>,
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    pub release: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub public: Option<bool>,
    pub bookmarked: Option<bool>,
    pub latency: Option<f64>,
    #[serde(rename = "totalCost")]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub scores: Vec<LangfuseScoreSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LangfuseTraceDetail {
    pub id: String,
    pub timestamp: String,
    pub name: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "projectId")]
    pub project_id: Option<String>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub latency: Option<f64>,
    #[serde(rename = "totalCost")]
    pub total_cost: Option<f64>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub observations: Vec<LangfuseObservation>,
    #[serde(default)]
    pub scores: Vec<LangfuseScoreSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LangfuseObservation {
    pub id: String,
    #[serde(rename = "traceId")]
    pub trace_id: Option<String>,
    #[serde(rename = "type")]
    pub observation_type: String, // "GENERATION", "SPAN", "EVENT"
    pub name: Option<String>,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "modelParameters")]
    pub model_parameters: Option<serde_json::Value>,
    pub input: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    #[serde(rename = "parentObservationId")]
    pub parent_observation_id: Option<String>,
    pub level: Option<String>, // "DEFAULT", "WARNING", "ERROR"
    #[serde(rename = "statusMessage")]
    pub status_message: Option<String>,
    pub usage: Option<LangfuseUsage>,
    #[serde(rename = "calculatedTotalCost")]
    pub calculated_total_cost: Option<f64>,
    pub latency: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LangfuseUsage {
    #[serde(rename = "promptTokens")]
    pub prompt_tokens: Option<u64>,
    #[serde(rename = "completionTokens")]
    pub completion_tokens: Option<u64>,
    #[serde(rename = "totalTokens")]
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LangfuseScoreSummary {
    pub id: String,
    pub name: String,
    pub value: Option<f64>,
    #[serde(rename = "stringValue")]
    pub string_value: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PaginatedTracesResponse {
    pub data: Vec<LangfuseTraceSummary>,
    pub meta: PaginationMeta,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PaginationMeta {
    pub page: u64,
    pub limit: u64,
    #[serde(rename = "totalItems")]
    pub total_items: Option<u64>,
    #[serde(rename = "totalPages")]
    pub total_pages: Option<u64>,
}
```

### 4.4 Background Async Worker Channel
To guarantee non-blocking TUI operation:
```rust
pub enum LangfuseApiRequest {
    CheckHealth,
    ListTraces { page: u64, limit: u64, query: Option<String>, session_id: Option<String> },
    GetTraceDetail { trace_id: String },
}

pub enum LangfuseApiResponse {
    Health { ok: bool, message: Option<String> },
    TracesLoaded { page: u64, traces: Vec<LangfuseTraceSummary>, total: Option<u64>, project_id: Option<String> },
    TraceDetailLoaded { detail: Box<LangfuseTraceDetail> },
    Error { action: &'static str, message: String },
}
```

A dedicated worker loop communicates with `LangfuseView` via standard Rust `std::sync::mpsc` channels, pumped inside the main event loop (`AppEvent`).

---

## 5. The Langfuse Screen (`Mode::Langfuse`)

### 5.1 Entry Points & Action Registry
- **F1 Command Palette**: `"Open Langfuse traces"` (Action ID: `ActionId::Langfuse`).
- **Global Keybinding**: `F1` menu and searchable via `Commands`.
- **Cross-link from Trace Browser (`T`)**: pressing `L` in the local Trace Browser smoothly transitions to `Mode::Langfuse` with the current session pre-filtered.
- **Availability Rule**:
  - `Enabled` when `ResolvedLangfuse` is present.
  - `Disabled("Langfuse credentials not configured")` otherwise.

### 5.2 Screen Layout Wireframe

```
┌ ◆ Langfuse Traces ─── cloud.langfuse.com ── pk-lf-8a... (proj: proj_c8f3) ── ● Connected ── 148 traces ── [Ctrl+R] Refresh ┐
│ Filter: [session: claude:s1                ]  Mode: [All Traces]  Time: [Last 24h]                        [w] Open in Web   │
├─ TRACES (Page 1/3) ───────────────────────┬─ OBSERVATION HIERARCHY ────────────────────────┬─ DETAIL INSPECTION ────────────┤
│ ● 10:42:15 claude:s1      4.2s  1.2k $0.02│ ▼ Trace: run tests (claude:s1)                 │ Type: GENERATION               │
│   tag: test, fix                          │   ├─ ⚙ Tool: bash "cargo test" [1.2s]          │ Name: claude-3-5-sonnet        │
│ ● 10:38:02 codex:feat-auth 8.1s  3.4k $0.05│   │  └─ ● Span: execute child [1.1s]           │ Model: claude-3-5-sonnet-202410│
│ ✗ 10:15:20 agy:refactor   1.0s   412 $0.00│   └─ ★ Gen: claude-3-5-sonnet [2.8s]           │ Duration: 2.84s                │
│   error: rate_limit                       │        tokens: 380 in / 840 out                │ Tokens: 380 prompt, 840 compl  │
│ ● 09:55:10 opencode:main  12.0s 5.1k $0.08│                                                │ Cost: $0.0182 USD              │
│                                           │                                                │                                │
│                                           │                                                │ Input:                         │
│                                           │                                                │ "Please fix the failing test..."│
│                                           │                                                │ Output:                        │
│                                           │                                                │ "I have updated the assertion..."│
│                                           │                                                │                                │
│                                           │                                                │ Metadata:                      │
│                                           │                                                │ { "temperature": 0.2 }         │
├───────────────────────────────────────────┴────────────────────────────────────────────────┴────────────────────────────────┤
│ ↑/↓: Navigate  •  Tab: Switch Pane  •  Enter: Select/Expand  •  /: Filter  •  w: Open Web  •  z: Zoom  •  q/Esc: Back        │
└─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### 5.3 Panes & Interactions

#### Pane 1: Traces & Sessions List
- Displays traces descending by timestamp.
- Columns:
  - Status Glyph: `●` Success (Green), `✗` Error (Red), `▲` Warning (Yellow).
  - Timestamp (HH:MM:SS / relative).
  - Session ID / Name with harness badges (`claude`, `codex`, `agy`, `opencode`).
  - Duration / Latency.
  - Token count and cost in USD.
  - Tag pills and score marks (e.g. `★ +1.0`).
- Navigation: `j`/`k`, `g`/`G`, `PgUp`/`PgDn`.
- Selecting a trace immediately queues detail & hierarchy loading in Pane 2 and Pane 3.

#### Pane 2: Observation Tree
- Visualizes the execution hierarchy of the selected trace:
  - Root Trace span.
  - LLM Generations (`★ Gen: <model>`).
  - Tool calls (`⚙ Tool: <tool_name>`).
  - Sub-agent delegations (`◆ Agent: <persona>`).
  - Custom spans & events.
- Nodes show durations, tokens, and error indicators.
- Expand / collapse nodes with `Space` or `Enter`.

#### Pane 3: Detail Inspection
- Shows full attributes of the selected observation:
  - Model, parameters, prompt tokens, completion tokens, cached tokens, and exact cost.
  - Formatted `Input` and `Output` payloads with syntax highlighting and line wrapping.
  - JSON metadata inspector.
  - Scores and feedback with user verdicts and evaluator comments.
- Supports zooming to full-screen width with `z`.
- In-pane text search with `/`.

### 5.4 Keybindings Table (Strictly adhering to `src/keymap.rs`)

| Key | Action | Scope | Description |
|---|---|---|---|
| `Tab` / `Shift+Tab` | Cycle Pane Focus | Local | Switch focus between Traces, Hierarchy, and Detail panes |
| `↑` / `↓` (`k` / `j`) | Move Selection | Local | Move selected trace or observation in the focused pane |
| `Enter` | Select / Expand | Local | Select trace or toggle collapse in tree |
| `w` or `o` | **Open Langfuse Web** | Local | Open the selected trace or session in the browser |
| `Ctrl+R` | Reload / Rescan | Local | Invalidate cache and fetch fresh traces from API |
| `/` | Filter / Search | Local | Open inline search bar for session, name, or tags |
| `Esc` | Clear / Back | Local | Clear active filter; if cleared, close screen (`Mode::Main`) |
| `q` | Close Screen | Local | Exit Langfuse view back to session |
| `z` | Zoom Pane | Local | Maximize the focused pane to full window width |
| `n` / `p` | Pagination | Local | Next / Previous page of traces |

---

## 6. Launching Langfuse Web from agent-mux

### 6.1 URL Resolution Logic

The browser destination is resolved dynamically based on current context and detected project ID:

```rust
pub fn resolve_langfuse_web_url(
    host: &str,
    project_id: Option<&str>,
    trace_id: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let clean_host = host.trim().trim_end_matches('/');
    match (project_id, trace_id, session_id) {
        (Some(proj), Some(trace), _) => {
            format!("{clean_host}/project/{proj}/traces/{trace}", proj = urlencoding::encode(proj), trace = urlencoding::encode(trace))
        }
        (Some(proj), None, Some(session)) => {
            format!("{clean_host}/project/{proj}/sessions/{session}", proj = urlencoding::encode(proj), session = urlencoding::encode(session))
        }
        (Some(proj), None, None) => {
            format!("{clean_host}/project/{proj}", proj = urlencoding::encode(proj))
        }
        (None, _, _) => clean_host.to_string(),
    }
}
```

### 6.2 Context-Aware Web Launch Options

1. **Inside the Langfuse Screen (`Mode::Langfuse`)**:
   - Pressing `w` or `o` opens the browser directly to the currently selected trace (`/project/{proj}/traces/{trace_id}`).
   - If a session is highlighted, opens `/project/{proj}/sessions/{session_id}`.
2. **Inside Trace Browser (`Mode::TraceBrowser`)**:
   - Pressing `w` checks if the current session was exported to Langfuse; if so, opens the Langfuse trace URL directly.
3. **From Active Session / Main View**:
   - `F1` Command Overlay offers: `"Open Langfuse Web"`.
   - Opens the project dashboard or current session in Langfuse Web.
4. **CLI Command**:
   - `agent-mux langfuse web [--session <id>] [--trace <id>] [--project <id>]`
   - Example:
     ```bash
     agent-mux langfuse web
     agent-mux langfuse web --session claude:s1
     agent-mux langfuse web --trace 7b249...
     ```

---

## 7. Configuration & Profiles Integration

In `profiles.toml` or `~/.agent-mux/config.toml`:

```toml
[tracing.langfuse]
host = "https://cloud.langfuse.com"
public_key = "pk-lf-..."
secret_key = "sk-lf-..."
project_id = "proj_..."           # Optional: explicit project ID for direct web URLs
fetch_limit = 50                  # Optional: default page size (default 50, max 100)
auto_open_web = false             # Optional: auto-launch web on trace click
```

Environment variable precedence:
1. `LANGFUSE_PROJECT_ID` (if set)
2. Auto-discovered `projectId` from API responses
3. `project_id` in config
4. Fallback to base host

---

## 8. Implementation Plan & Affected Components

| Component / File | Nature | Responsibilities |
|---|---|---|
| `src/tracing/langfuse/api.rs` | **New** | REST API client for Langfuse: models, request builder, authentication, pagination, and background worker loop. |
| `src/tracing/langfuse/mod.rs` | Edit | Export `api` module, add `project_id` to `ResolvedLangfuse`, implement health verification function. |
| `src/config.rs` | Edit | Parse optional `project_id` in `LangfuseConfig` and `ResolvedLangfuse`. |
| `src/app/langfuse_view.rs` | **New** | State container (`LangfuseViewState`) for traces, observations, hierarchy tree, filters, async channels, and pagination. |
| `src/ui/langfuse.rs` | **New** | Ratatui rendering engine: 3-pane layout, observation tree widgets, status badges, syntax formatting, and hints. |
| `src/app.rs` | Edit | Register `Mode::Langfuse`, route key events to `langfuse_key`, handle `OpenUrl` outcomes. |
| `src/keymap.rs` | Edit | Add `ActionId::Langfuse` and `ActionId::OpenLangfuseWeb` to `ACTIONS`. |
| `src/app/commands.rs` | Edit | Command availability checks for `ActionId::Langfuse` and `ActionId::OpenLangfuseWeb`. |
| `src/tracing/cli.rs` | Edit | Add `agent-mux langfuse web` and `agent-mux trace langfuse` subcommands. |

---

## 9. Verification & Testing Strategy

1. **Unit Tests (`src/tracing/langfuse/api.rs`)**:
   - Serde JSON deserialization of complex real-world Langfuse responses (traces with multiple generations, tool spans, empty metadata, null usage, custom scores).
   - URL encoding and deep link generation for all combinations of project, trace, and session.
   - Basic Auth header formatting test.
2. **Mock Server Tests (`tests/langfuse_api_mock.rs`)**:
   - Spin up a local HTTP stub server (`ureq` / `tiny_http`).
   - Test healthy trace listing, pagination, error handling on 401/403/429/500 responses.
   - Verify non-blocking behavior: verify worker thread terminates cleanly and respects timeouts.
3. **TUI State & Interaction Tests (`tests/langfuse_view.rs`)**:
   - State navigation test: moving between traces updates the observation tree.
   - Key event test: `Tab` cycles panes, `/` opens search bar, `w` emits `Outcome::OpenUrl`.
   - Disabled state test: launching when credentials are missing shows guidance banner.
4. **Manual Verification**:
   - Run `cargo test` and `cargo clippy --all-targets`.
   - Connect to a live Langfuse Cloud account, extract real traces from previous sessions, verify tree rendering in the TUI, and test `w` opening the correct trace in the browser.
