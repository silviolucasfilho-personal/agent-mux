//! Stdio MCP server for agent-mux trace analytics.

use super::schema::tool_definitions;
use super::tools::tool_names;
use crate::tracing::analysis::scope::Scope;
use crate::tracing::analysis::service::{Limits, Request, ServiceConfig, TraceService};
use crate::tracing::analysis::{default_snapshot_dir, default_trace_db_path};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Runs the stdio MCP server with given command line arguments.
pub async fn run(args: &[String]) -> Result<()> {
    let mut db_path: Option<PathBuf> = None;
    let mut workspace_path: Option<PathBuf> = None;
    let mut all_workspaces = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "serve" => {
                i += 1;
            }
            "--stdio" => {
                i += 1;
            }
            "--db" => {
                if i + 1 < args.len() {
                    db_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    anyhow::bail!("--db requires a path argument");
                }
            }
            "--workspace" => {
                if i + 1 < args.len() {
                    workspace_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    anyhow::bail!("--workspace requires a directory argument");
                }
            }
            "--all-workspaces" => {
                all_workspaces = true;
                i += 1;
            }
            other => {
                anyhow::bail!("unknown argument for mcp serve: {other}");
            }
        }
    }

    let resolved_db = db_path.unwrap_or_else(default_trace_db_path);

    let scope = if all_workspaces {
        Scope::all_workspaces()
    } else if let Some(ws) = workspace_path {
        Scope::workspace(&ws).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        let cur = std::env::current_dir().context("cannot determine current working directory")?;
        Scope::workspace(&cur).map_err(|e| anyhow::anyhow!("{e}"))?
    };

    let config = ServiceConfig {
        db_path: resolved_db,
        scope,
        snapshot_dir: Some(default_snapshot_dir()),
        limits: Limits::default(),
        admission_hook: None,
    };

    let service = Arc::new(TraceService::new(config).map_err(|e| anyhow::anyhow!("{e}"))?);

    serve_stdio(service).await
}

async fn serve_stdio(service: Arc<TraceService>) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": "Parse error"
                    }
                });
                write_json_line(&mut stdout, &err_resp).await?;
                continue;
            }
        };

        let id = parsed.get("id").cloned();
        let method = parsed
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = parsed.get("params").cloned().unwrap_or(json!({}));

        // Notification handling (no response if id is null or absent)
        if id.is_none() || id == Some(Value::Null) {
            if method == "notifications/cancelled" {
                // Future work or task cancellation hook
            }
            continue;
        }

        let req_id = id.unwrap();

        match method {
            "initialize" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "agent-mux",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                });
                write_json_line(&mut stdout, &resp).await?;
            }
            "ping" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {}
                });
                write_json_line(&mut stdout, &resp).await?;
            }
            "tools/list" => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "tools": tool_definitions()
                    }
                });
                write_json_line(&mut stdout, &resp).await?;
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let supported_names = tool_names();
                if !supported_names.contains(&name) {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": -32601,
                            "message": format!("Unknown tool: {name}")
                        }
                    });
                    write_json_line(&mut stdout, &resp).await?;
                    continue;
                }

                let req_result = Request::from_tool_call(name, arguments);
                let request = match req_result {
                    Ok(r) => r,
                    Err(err) => {
                        let resp = json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": format!("{err}")
                                    }
                                ],
                                "isError": true
                            }
                        });
                        write_json_line(&mut stdout, &resp).await?;
                        continue;
                    }
                };

                let svc = Arc::clone(&service);
                let exec_result = tokio::task::spawn_blocking(move || svc.execute(request)).await?;

                let resp = match exec_result {
                    Ok(envelope) => {
                        let text = serde_json::to_string(&envelope).unwrap_or_default();
                        let structured = serde_json::to_value(&envelope).unwrap_or(Value::Null);
                        json!({
                            "jsonrpc": "2.0",
                            "id": req_id,
                            "result": {
                                "content": [
                                    {
                                        "type": "text",
                                        "text": text
                                    }
                                ],
                                "structuredContent": structured,
                                "isError": false
                            }
                        })
                    }
                    Err(err) => json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "content": [
                                {
                                    "type": "text",
                                    "text": format!("{err}")
                                }
                            ],
                            "isError": true
                        }
                    }),
                };
                write_json_line(&mut stdout, &resp).await?;
            }
            _ => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {
                        "code": -32601,
                        "message": format!("Method not found: {method}")
                    }
                });
                write_json_line(&mut stdout, &resp).await?;
            }
        }
    }

    Ok(())
}

async fn write_json_line(stdout: &mut tokio::io::Stdout, value: &Value) -> Result<()> {
    let mut serialized = serde_json::to_string(value)?;
    serialized.push('\n');
    stdout.write_all(serialized.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
