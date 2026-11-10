// agent-mux's Pi extension: Pi (0.73.1) has no MCP client by design, so
// this extension is one, for one server. It is compiled into agent-mux
// (`src/pi.rs`), written to `~/.agent-mux/pi-extension/`, and loaded per
// launch with `pi -e <file>`; the server to start travels in
// `AGENT_MUX_PI_MCP` (`{"name", "command", "args"}`), so nothing lands in
// the user's Pi settings.
//
// At load (an async factory: Pi waits for it before the session starts)
// it spawns the server over stdio, runs `initialize` and `tools/list`, and
// registers each tool under its own name (agent-mux's are already
// `agent_mux_*`) with the server's JSON Schema as its parameters, which
// Pi validates as plain JSON Schema. A call is `tools/call`; an `isError`
// result throws, which is how Pi marks a failed tool call. The server is
// stopped on `session_shutdown` (Pi reloads extensions per session, so the
// next instance starts its own). Every startup failure is swallowed: the
// session runs without the tools rather than not at all.

import { spawn } from "node:child_process";

const ENV = "AGENT_MUX_PI_MCP";
const PROTOCOL_VERSION = "2025-11-25";
const START_TIMEOUT_MS = 10000;

class Client {
  constructor(spec) {
    this.next = 1;
    this.pending = new Map();
    this.buffer = "";
    this.child = spawn(spec.command, spec.args || [], {
      stdio: ["pipe", "pipe", "ignore"],
      env: process.env,
    });
    this.child.on("error", (e) => this.failAll(e));
    this.child.on("exit", () => this.failAll(new Error("the MCP server exited")));
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => this.read(chunk));
  }

  read(chunk) {
    this.buffer += chunk;
    let nl;
    while ((nl = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, nl).trim();
      this.buffer = this.buffer.slice(nl + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      const waiter = msg && msg.id !== undefined ? this.pending.get(msg.id) : undefined;
      if (!waiter) continue;
      this.pending.delete(msg.id);
      if (msg.error) waiter.reject(new Error(msg.error.message || "MCP error"));
      else waiter.resolve(msg.result);
    }
  }

  failAll(err) {
    for (const w of this.pending.values()) w.reject(err);
    this.pending.clear();
    this.closed = true;
  }

  send(obj) {
    if (this.closed) throw new Error("the MCP server is not running");
    this.child.stdin.write(JSON.stringify(obj) + "\n");
  }

  request(method, params, timeoutMs) {
    const id = this.next++;
    return new Promise((resolve, reject) => {
      let timer;
      const done = (f) => (v) => {
        if (timer) clearTimeout(timer);
        f(v);
      };
      this.pending.set(id, { resolve: done(resolve), reject: done(reject) });
      if (timeoutMs) {
        timer = setTimeout(() => {
          this.pending.delete(id);
          reject(new Error(`${method} timed out`));
        }, timeoutMs);
      }
      try {
        this.send({ jsonrpc: "2.0", id, method, params });
      } catch (e) {
        this.pending.delete(id);
        done(reject)(e);
      }
    });
  }

  notify(method, params) {
    try {
      this.send({ jsonrpc: "2.0", method, params });
    } catch {}
  }

  close() {
    this.closed = true;
    try {
      this.child.stdin.end();
      this.child.kill();
    } catch {}
  }
}

function resultContent(result) {
  const out = [];
  for (const c of (result && result.content) || []) {
    if (c && c.type === "text") out.push({ type: "text", text: c.text });
    else if (c && c.type === "image") out.push({ type: "image", data: c.data, mimeType: c.mimeType });
    else if (c) out.push({ type: "text", text: JSON.stringify(c) });
  }
  if (out.length === 0 && result && result.structuredContent !== undefined) {
    out.push({ type: "text", text: JSON.stringify(result.structuredContent) });
  }
  return out;
}

export default async function agentMuxMcp(pi) {
  let spec;
  try {
    spec = JSON.parse(process.env[ENV] || "null");
  } catch {
    spec = null;
  }
  if (!spec || !spec.command) return;
  let client;
  try {
    client = new Client(spec);
    await client.request(
      "initialize",
      {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: "agent-mux-pi", version: "1" },
      },
      START_TIMEOUT_MS,
    );
    client.notify("notifications/initialized", {});
    const listed = await client.request("tools/list", {}, START_TIMEOUT_MS);
    for (const tool of (listed && listed.tools) || []) {
      pi.registerTool({
        name: tool.name,
        label: tool.title || tool.name,
        description: tool.description || tool.name,
        parameters: tool.inputSchema || { type: "object", properties: {} },
        async execute(_id, params) {
          const result = await client.request("tools/call", {
            name: tool.name,
            arguments: params || {},
          });
          const content = resultContent(result);
          if (result && result.isError) {
            throw new Error(content.map((c) => c.text || "").join("\n") || "tool failed");
          }
          return { content, details: {} };
        },
      });
    }
  } catch {
    if (client) client.close();
    return;
  }
  pi.on("session_shutdown", async () => client.close());
}
