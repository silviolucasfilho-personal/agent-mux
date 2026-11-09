// agent-mux's OpenCode plugin: OpenCode 2 has plugins, not hooks, so this
// module turns its plugin hooks into the Claude-shaped hook payloads that
// `agent-mux trace hook opencode` reads (probed on OpenCode 2.0.32).
//
// It is compiled into agent-mux (`src/opencode.rs`), written to
// `~/.agent-mux/opencode-plugin/` and named per launch in
// `OPENCODE_CONFIG_CONTENT` (`plugins: [{ package, options }]`), so nothing
// lands in the user's OpenCode configuration. `options` carries the binary,
// the launch and which events to send.
//
// - `tool.execute.before` → PreToolUse, synchronously: agent-mux answers
//   with Claude's `hookSpecificOutput`; a `deny` throws, which OpenCode
//   turns into a failed tool call the model sees, and `additionalContext`
//   (urgent bus messages) is added to the system prompt of the session's
//   next model request.
// - `tool.execute.after` → PostToolUse / PostToolUseFailure.
// - `session.hook("context")` → SessionStart (first request of a session)
//   and UserPromptSubmit (a user message not seen before).
//
// A subagent's session reports under its root session, with `agent_id`
// naming the subagent's own session, the way Claude reports a subagent's
// tool calls. Every failure is swallowed: the plugin never breaks a session.

import { spawn, spawnSync } from "node:child_process";

const PRE_TOOL_TIMEOUT_MS = 2000;

function textOf(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((p) => p && p.type === "text" && typeof p.text === "string")
    .map((p) => p.text)
    .join("\n");
}

function errorText(e) {
  if (e == null) return undefined;
  if (typeof e === "string") return e;
  if (typeof e.message === "string") return e.message;
  if (e.error) return errorText(e.error);
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

export default {
  id: "agent-mux",
  setup: (ctx) => {
    const o = (ctx && ctx.options) || {};
    if (!o.exe || !Array.isArray(o.args)) return;
    const events = new Set(Array.isArray(o.events) ? o.events : []);
    const cwd = o.cwd || process.cwd();
    const seenSessions = new Set();
    const seenPrompts = new Set();
    const pendingContext = new Map();
    const roots = new Map();

    // The root session of `id` (a subagent's parent chain), cached.
    const rootOf = async (id) => {
      if (!id) return id;
      if (roots.has(id)) return roots.get(id);
      let cur = id;
      try {
        for (let i = 0; i < 8; i++) {
          const info = await ctx.session.get({ sessionID: cur });
          const parent = info && (info.parentID || info.parentId);
          if (!parent) break;
          cur = parent;
        }
      } catch {
        // unknown: report under the session itself
      }
      roots.set(id, cur);
      return cur;
    };

    const payloadFor = async (event, sessionID, extra) => {
      const root = await rootOf(sessionID);
      const p = { hook_event_name: event, session_id: root, cwd, ...extra };
      if (root && sessionID && root !== sessionID) p.agent_id = sessionID;
      return p;
    };

    const sendAsync = (payload) => {
      try {
        const child = spawn(o.exe, o.args, {
          stdio: ["pipe", "ignore", "ignore"],
          detached: true,
        });
        child.on("error", () => {});
        child.stdin.on("error", () => {});
        child.stdin.end(JSON.stringify(payload));
        child.unref();
      } catch {
        // fail open
      }
    };

    const sendSync = (payload, flags) => {
      try {
        const r = spawnSync(o.exe, [...o.args, ...flags], {
          input: JSON.stringify(payload),
          encoding: "utf8",
          timeout: PRE_TOOL_TIMEOUT_MS,
          stdio: ["pipe", "pipe", "ignore"],
        });
        const out = (r.stdout || "").trim();
        return out ? JSON.parse(out) : null;
      } catch {
        return null;
      }
    };

    if (events.has("PreToolUse")) {
      void ctx.tool.hook("execute.before", async (h) => {
        const payload = await payloadFor("PreToolUse", h.sessionID, {
          tool_name: h.tool,
          tool_use_id: h.id,
          tool_input: h.input,
          agent_type: h.agent,
        });
        const flags = [];
        if (o.guard) flags.push("--guard");
        if (o.loop) flags.push("--loop");
        const answer = sendSync(payload, flags);
        const out = answer && answer.hookSpecificOutput;
        if (!out) return;
        if (out.permissionDecision === "deny") {
          throw new Error(out.permissionDecisionReason || "refused by agent-mux");
        }
        if (typeof out.additionalContext === "string" && out.additionalContext) {
          const list = pendingContext.get(h.sessionID) || [];
          list.push(out.additionalContext);
          pendingContext.set(h.sessionID, list);
        }
      });
    }

    if (events.has("PostToolUse")) {
      void ctx.tool.hook("execute.after", async (h) => {
        const failed = h.status === "error";
        const extra = {
          tool_name: h.tool,
          tool_use_id: h.id,
          tool_input: h.input,
          agent_type: h.agent,
          tool_response: h.result,
        };
        if (failed) extra.error = errorText(h.error);
        sendAsync(
          await payloadFor(failed ? "PostToolUseFailure" : "PostToolUse", h.sessionID, extra),
        );
      });
    }

    void ctx.session.hook("context", async (h) => {
      const id = h.sessionID;
      if (id && !seenSessions.has(id)) {
        seenSessions.add(id);
        if (events.has("SessionStart") && (await rootOf(id)) === id) {
          sendAsync(await payloadFor("SessionStart", id, { source: "startup" }));
        }
      }
      const last = Array.isArray(h.messages) ? h.messages[h.messages.length - 1] : undefined;
      if (
        events.has("UserPromptSubmit") &&
        last &&
        last.role === "user" &&
        last.id &&
        !seenPrompts.has(last.id)
      ) {
        seenPrompts.add(last.id);
        sendAsync(
          await payloadFor("UserPromptSubmit", id, {
            prompt: textOf(last.content),
            prompt_id: last.id,
          }),
        );
      }
      const pending = pendingContext.get(id);
      if (pending && pending.length && Array.isArray(h.system)) {
        pendingContext.delete(id);
        for (const text of pending) {
          h.system.push({ type: "text", text: `<system-reminder>\n${text}\n</system-reminder>` });
        }
      }
    });
  },
};
