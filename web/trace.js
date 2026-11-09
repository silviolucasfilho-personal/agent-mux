// Trace store pages: Sessions, Session (detail + timeline), Search,
// Skills & Agents, Health. Every call is POST /api/trace/<tool>.
(function () {
  'use strict';
  const M = window.AgentMux;
  const { h, api, table, kv, cards, link, loading, envelopeNote } = M;

  const PROVIDERS = ['', 'claude', 'codex', 'antigravity', 'opencode'];

  function sessionLink(key, text) {
    if (!key) return h('span', { class: 'muted', text: text || '–' });
    return link('/sessions/' + encodeURIComponent(key), text || key);
  }

  function stateBadge(state) {
    const cls = state === 'working' ? 'badge working' : 'badge';
    return h('span', { class: cls, text: state || 'unknown' });
  }

  // ---- Sessions ------------------------------------------------------------

  M.registerPage({
    id: 'sessions',
    title: 'Sessions',
    path: '/sessions',
    live: true, // re-rendered by the shell when the store changes (M.live)
    async render(el, params) {
      const filters = { provider: params.provider || '', workspace: params.workspace || '', since: params.since || '', state: params.state || '' };
      const form = h('form', { class: 'filters' },
        h('label', null, 'harness', h('select', { name: 'provider' }, PROVIDERS.map((p) => h('option', { value: p, selected: p === filters.provider || null }, p || 'any')))),
        h('label', null, 'workspace contains', h('input', { name: 'workspace', value: filters.workspace, placeholder: 'path fragment' })),
        h('label', null, 'since', h('input', { name: 'since', value: filters.since, placeholder: '24h, 7d or RFC 3339' })),
        h('label', null, 'state', h('select', { name: 'state' }, ['', 'working', 'waiting_for_user', 'idle', 'exited', 'disconnected', 'unknown'].map((s) => h('option', { value: s, selected: s === filters.state || null }, s || 'any')))),
        h('button', { type: 'submit', class: 'primary' }, 'Apply'),
        h('button', { type: 'button', onclick: () => M.navigate('/sessions') }, 'Clear'),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const fd = new FormData(form);
        M.navigate('/sessions', { provider: fd.get('provider'), workspace: fd.get('workspace'), since: fd.get('since'), state: fd.get('state') });
      });
      el.appendChild(form);
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading());

      const body = { limit: 100 };
      if (filters.provider) body.provider = filters.provider;
      if (filters.since) body.since = M.sinceToRfc3339(filters.since);
      if (filters.state) body.runtime_state = filters.state;
      let env = await api('/api/trace/list_sessions', body);
      let rows = env.data.sessions || [];
      // page through a long list: the service caps a page at 100
      let cursor = env.next_cursor;
      let pages = 0;
      while (cursor && pages < 20) {
        const more = await api('/api/trace/list_sessions', Object.assign({}, body, { cursor }));
        rows = rows.concat(more.data.sessions || []);
        cursor = more.next_cursor;
        pages++;
      }
      if (filters.workspace) {
        const needle = filters.workspace.toLowerCase();
        rows = rows.filter((r) => (r.cwd || '').toLowerCase().includes(needle));
      }
      out.innerHTML = '';
      out.appendChild(h('div', { class: 'muted' }, rows.length + ' of ' + (env.data.total_matching || rows.length) + ' sessions' + (cursor ? ' (more not loaded)' : '')));
      const note = envelopeNote(env);
      if (note) out.appendChild(note);
      out.appendChild(table({
        sortKey: 'last_active', sortDesc: true,
        empty: 'No sessions in the store for these filters.',
        columns: [
          { key: 'session_key', label: 'session', render: (r) => sessionLink(r.session_key, r.session_key || r.launch_id), class: 'ellipsis' },
          { key: 'provider', label: 'harness' },
          { key: 'cwd', label: 'workspace', render: (r) => M.shortPath(r.cwd), title: (r) => r.cwd, class: 'ellipsis' },
          { key: 'runtime_state', label: 'state', render: (r) => stateBadge(r.runtime_state) },
          { key: 'turns', label: 'turns', num: true },
          { key: 'tools', label: 'tools', num: true },
          { key: 'tokens', label: 'tokens', num: true, render: (r) => M.fmtTokens(r.tokens) },
          { key: 'cost_usd', label: 'cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
          { key: 'first_seen', label: 'first seen', render: (r) => M.fmtTime(r.first_seen) },
          { key: 'last_active', label: 'last active', render: (r) => M.fmtTime(r.last_active) },
          { key: 'correlation_quality', label: 'correlation' },
        ],
        rows,
      }));
    },
  });

  // ---- Session -------------------------------------------------------------
  //
  // The session viewer: GET /api/session/<key>/transcript (header facts,
  // turns with every observation, files touched) beside get_session (the
  // runtime state and the card's facts). Live while the session is.

  function evidenceText(ev) {
    if (!ev) return null;
    if (typeof ev === 'string') return ev;
    return ev.value === null || ev.value === undefined ? null : ev.value;
  }

  const LIVE_STATES = ['working', 'waiting_for_user', 'idle'];
  const CLIP_CHARS = 1500; // characters shown before "show all"
  const CLIP_LINES = 24;

  // One-time styles of the session viewer (theme variables from app.css).
  function sessionStyles() {
    if (document.getElementById('session-viewer-style')) return;
    const css = `
.sv-grid { display: grid; grid-template-columns: 200px minmax(0, 1fr); gap: 16px; align-items: start; }
@media (max-width: 900px) { .sv-grid { grid-template-columns: minmax(0, 1fr); } .sv-outline { display: none; } }
.sv-outline { position: sticky; top: 68px; max-height: calc(100vh - 80px); overflow-y: auto; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px; background: var(--panel); font-size: 12px; }
.sv-outline a { display: flex; gap: 6px; align-items: center; padding: 2px 4px; border-radius: 4px; color: var(--fg); text-decoration: none; white-space: nowrap; overflow: hidden; }
.sv-outline a:hover { background: var(--row-hover); }
.sv-outline a.current { background: var(--row-selected); }
.sv-outline .n { font-family: var(--mono); color: var(--muted); min-width: 2.5em; text-align: right; }
.sv-outline .t { overflow: hidden; text-overflow: ellipsis; flex: 1; }
.sv-outline .bar { height: 4px; background: var(--c1); border-radius: 2px; flex: 0 0 auto; }
.sv-outline a.err .n { color: var(--error-fg); }
.sv-turn { border: 1px solid var(--line); border-radius: var(--radius); margin: 0 0 12px; background: var(--panel); scroll-margin-top: 72px; }
.sv-turn.current { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
.sv-turn > header { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; padding: 6px 12px; border-bottom: 1px solid var(--line); background: var(--bg-soft); font-size: 12px; color: var(--muted); border-radius: var(--radius) var(--radius) 0 0; }
.sv-turn > header b { color: var(--fg); font-family: var(--mono); }
.sv-turn > header a { color: inherit; }
.sv-body { padding: 8px 12px 10px; }
.sv-msg { margin: 6px 0; }
.sv-who { font-size: 11px; text-transform: uppercase; letter-spacing: .04em; color: var(--muted); margin-bottom: 2px; }
.sv-msg.user .sv-text { background: var(--accent-soft); border-radius: 6px; padding: 6px 10px; }
.sv-text { white-space: pre-wrap; overflow-wrap: anywhere; }
.sv-pre { white-space: pre-wrap; overflow-wrap: anywhere; font-family: var(--mono); font-size: 12px; background: var(--bg-soft); border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; margin: 2px 0 6px; }
.sv-more { font-size: 12px; margin: 0 0 6px; }
.sv-more button { font-size: 12px; padding: 1px 8px; }
.sv-cut { color: var(--muted); font-size: 12px; font-style: italic; }
details.sv-call, details.sv-think { border: 1px solid var(--line); border-radius: 6px; padding: 3px 8px; margin: 4px 0; }
details.sv-call > summary, details.sv-think > summary { cursor: pointer; font-size: 12.5px; font-family: var(--mono); }
details.sv-call > summary .muted { margin-left: 6px; }
details.sv-call.error { border-color: var(--error-fg); background: var(--error-bg); }
details.sv-think { border-style: dashed; }
.sv-agent { border-left: 3px solid var(--c4); margin: 6px 0 6px 2px; padding: 2px 0 2px 10px; }
.sv-agent > .sv-who { color: var(--c4); }
.badge.skill { color: var(--c4); border-color: var(--c4); }
.badge.live { color: var(--ok); border-color: var(--ok); }
.sv-banner { border: 1px solid var(--warn); color: var(--warn); border-radius: var(--radius); padding: 8px 12px; margin: 8px 0 12px; }
.sv-links a { margin-right: 10px; }
`;
    document.head.appendChild(h('style', { id: 'session-viewer-style', text: css }));
  }

  // `text` with long content clipped behind a "show all" button; `cutAt`
  // (the store's full length) says the server cut it too.
  function clipped(text, cls, cutAt) {
    const wrap = h('div');
    const el = h('div', { class: cls });
    wrap.appendChild(el);
    const lines = text.split('\n');
    const long = text.length > CLIP_CHARS || lines.length > CLIP_LINES;
    if (!long) {
      el.textContent = text;
    } else {
      let short = lines.slice(0, CLIP_LINES).join('\n');
      if (short.length > CLIP_CHARS) short = short.slice(0, CLIP_CHARS);
      el.textContent = short + ' …';
      const btn = h('button', { type: 'button', text: 'Show all (' + M.fmtNum(text.length) + ' chars)' });
      let open = false;
      btn.addEventListener('click', () => {
        open = !open;
        el.textContent = open ? text : short + ' …';
        btn.textContent = open ? 'Show less' : 'Show all (' + M.fmtNum(text.length) + ' chars)';
      });
      wrap.appendChild(h('div', { class: 'sv-more' }, btn));
    }
    if (cutAt) wrap.appendChild(h('div', { class: 'sv-cut', text: 'Cut by the viewer at ' + M.fmtNum(text.length) + ' of ' + M.fmtNum(cutAt) + ' characters; the HTML export keeps more.' }));
    return wrap;
  }

  // A tool input: pretty JSON when it parses.
  function prettyInput(s) {
    if (!s) return s;
    const t = s.trim();
    if (t[0] !== '{' && t[0] !== '[') return s;
    try { return JSON.stringify(JSON.parse(t), null, 2); } catch (_) { return s; }
  }

  // A one-line hint of what a call did: its path, command or first input field.
  function callHint(o) {
    if (o.path) return o.path;
    if (!o.input) return '';
    try {
      const v = JSON.parse(o.input);
      if (v && typeof v === 'object') {
        const k = ['command', 'cmd', 'file_path', 'path', 'pattern', 'query', 'url', 'description', 'prompt'].find((x) => typeof v[x] === 'string');
        if (k) return v[k].split('\n')[0].slice(0, 120);
      }
    } catch (_) { /* not JSON */ }
    return o.input.split('\n')[0].slice(0, 120);
  }

  function isSkill(o) { return o.kind === 'skill_load' || !!o.skill; }

  // Observations as a tree: a generation shows its text and holds its tool
  // calls; an agent invocation nests its sub-agent's turns.
  function obsTree(turn) {
    const ids = new Set(turn.observations.map((o) => o.id));
    const children = new Map();
    for (const o of turn.observations) {
      const p = o.parent_id && ids.has(o.parent_id) ? o.parent_id : '';
      if (!children.has(p)) children.set(p, []);
      children.get(p).push(o);
    }
    return children;
  }

  function renderObs(children, parent, out) {
    for (const o of children.get(parent) || []) {
      const cut = (f) => (o.lengths || {})[f];
      if (o.type === 'generation') {
        if (o.thinking && o.thinking.trim()) {
          out.appendChild(h('details', { class: 'sv-think' }, h('summary', { text: 'thinking' }), clipped(o.thinking, 'sv-text', cut('thinking'))));
        }
        if (o.output && o.output.trim()) {
          out.appendChild(h('div', { class: 'sv-msg assistant' },
            h('div', { class: 'sv-who', text: 'assistant' + (o.model ? ' · ' + o.model : '') }),
            clipped(o.output, 'sv-text', cut('output'))));
        }
        renderObs(children, o.id, out);
      } else if (o.type === 'agent') {
        const box = h('div', { class: 'sv-agent' },
          h('div', { class: 'sv-who' }, o.name, ' ', o.duration_ms != null ? h('span', { class: 'muted', text: M.fmtDuration(o.duration_ms) }) : null,
            o.error ? h('span', { class: 'badge error', text: 'error' }) : null));
        if (o.input && o.input.trim() && o.kind !== 'subagent_turn') {
          box.appendChild(h('details', { class: 'sv-call' }, h('summary', { text: 'prompt' }), clipped(prettyInput(o.input), 'sv-pre', cut('input'))));
        }
        renderObs(children, o.id, box);
        if (o.output && o.output.trim()) {
          box.appendChild(h('details', { class: 'sv-call' }, h('summary', { text: 'result' }), clipped(o.output, 'sv-pre', cut('output'))));
        }
        out.appendChild(box);
      } else if (o.type === 'event' && !o.input && !o.output) {
        out.appendChild(h('div', { class: 'sv-cut', text: '— ' + o.name + ' —' }));
      } else {
        const d = h('details', { class: 'sv-call' + (o.error ? ' error' : '') },
          h('summary', null, o.name,
            isSkill(o) ? h('span', { class: 'badge skill', text: 'skill' + (o.skill ? ' ' + o.skill : '') }) : null,
            o.error ? h('span', { class: 'badge error', text: 'error' }) : null,
            h('span', { class: 'muted', text: callHint(o) }),
            h('span', { class: 'muted', text: M.fmtDuration(o.duration_ms) })));
        if (o.error && o.status_message) d.appendChild(h('div', { class: 'error-text', style: 'color: var(--error-fg)', text: o.status_message }));
        if (o.input) d.appendChild(h('div', null, h('div', { class: 'sv-who', text: 'input' }), clipped(prettyInput(o.input), 'sv-pre', cut('input'))));
        if (o.output) d.appendChild(h('div', null, h('div', { class: 'sv-who', text: 'output' }), clipped(o.output, 'sv-pre', cut('output'))));
        if (!o.input && !o.output && !(o.error && o.status_message)) d.appendChild(h('div', { class: 'sv-cut', text: 'No input or output stored.' }));
        if (o.error) d.open = true;
        out.appendChild(d);
        renderObs(children, o.id, out);
      }
    }
  }

  function turnElement(turn, ctx) {
    const n = turn.index + 1;
    const skills = turn.skills || [];
    const head = h('header', null,
      h('b', { text: 'turn ' + n }),
      h('span', { text: M.fmtTime(turn.start_ns) }),
      h('span', { text: M.fmtDuration(turn.duration_ms) }),
      turn.total_tokens ? h('span', { text: M.fmtTokens(turn.total_tokens) + ' tokens' }) : null,
      turn.cost_usd ? h('span', { text: M.fmtCost(turn.cost_usd) }) : null,
      turn.status !== 'closed' ? h('span', { class: 'badge' + (turn.status === 'open' ? ' working' : ''), text: turn.status }) : null,
      skills.map((s) => h('span', { class: 'badge skill', text: 'skill ' + s })),
      turn.errors ? h('span', { class: 'badge error', text: turn.errors + ' error' + (turn.errors > 1 ? 's' : '') }) : null,
      h('a', { href: M.hrefFor('/sessions/' + encodeURIComponent(ctx.key), Object.assign({}, ctx.launchId ? { launch_id: ctx.launchId } : {}, { turn: turn.id })), title: 'Link to this turn', text: '#' }),
    );
    const body = h('div', { class: 'sv-body' });
    const cut = (f) => (turn.lengths || {})[f];
    if (turn.input) body.appendChild(h('div', { class: 'sv-msg user' }, h('div', { class: 'sv-who', text: 'user' }), clipped(turn.input, 'sv-text', cut('input'))));
    if (turn.thinking) body.appendChild(h('details', { class: 'sv-think' }, h('summary', { text: 'thinking' }), clipped(turn.thinking, 'sv-text', cut('thinking'))));
    renderObs(obsTree(turn), '', body);
    const genText = turn.observations.some((o) => o.type === 'generation' && o.output && o.output.trim());
    if (turn.output && !genText) body.appendChild(h('div', { class: 'sv-msg assistant' }, h('div', { class: 'sv-who', text: 'assistant' }), clipped(turn.output, 'sv-text', cut('output'))));
    if (!turn.input && !turn.output && !turn.observations.length) body.appendChild(h('div', { class: 'sv-cut', text: ctx.metadataOnly ? 'Metadata only: no content stored for this turn.' : 'Nothing recorded for this turn yet.' }));
    const el = h('section', { class: 'sv-turn', id: 'turn-' + n, 'data-turn': turn.id, 'data-index': String(turn.index) }, head, body);
    return el;
  }

  function launchLinks(l) {
    const bits = [];
    if (l.swarm) bits.push(link('/swarms/' + encodeURIComponent(l.swarm.id), 'swarm ' + l.swarm.id + (l.swarm.task_key ? ' · ' + l.swarm.task_key : l.swarm.role ? ' · ' + l.swarm.role : '')));
    if (l.loop_run) bits.push(link('/runs/loop/' + encodeURIComponent(l.loop_run.run_id), 'loop run ' + (l.loop_run.loop_id || l.loop_run.run_id)));
    if (l.workflow) bits.push(link('/runs/workflow/' + encodeURIComponent(l.workflow.run_id), 'workflow ' + (l.workflow.name || l.workflow.run_id) + (l.workflow.step ? ' · ' + l.workflow.step : '')));
    return bits;
  }

  function launchesNode(launches) {
    if (!launches.length) return null;
    return h('div', null, launches.map((l) => h('div', { class: 'sv-links' },
      h('code', { text: l.id }), ' ',
      h('span', { class: 'muted', text: [l.profile, l.content_mode === 'metadata' ? 'metadata only' : null, M.fmtTime(l.started_ns), l.termination].filter(Boolean).join(' · ') }), ' ',
      launchLinks(l))));
  }

  function charts(turns) {
    const labels = turns.map((t) => String(t.index + 1));
    const tokens = M.charts.bar({
      labels, stacked: true, format: M.fmtTokens, height: 180, empty: 'No token usage recorded.',
      series: [
        { name: 'input', values: turns.map((t) => t.input_tokens) },
        { name: 'cache read', values: turns.map((t) => t.cache_read_tokens) },
        { name: 'cache write', values: turns.map((t) => t.cache_write_tokens) },
        { name: 'output', values: turns.map((t) => t.output_tokens) },
      ],
      onClick: (i) => ctxGoTo(turns[i].index),
    });
    const cost = M.charts.bar({
      labels, format: M.fmtCost, height: 180, empty: 'No cost recorded.',
      series: [{ name: 'cost', values: turns.map((t) => t.cost_usd) }],
      onClick: (i) => ctxGoTo(turns[i].index),
    });
    // context growth: what each generation read (input plus cache)
    const gl = [];
    const gv = [];
    for (const t of turns) {
      let k = 0;
      for (const o of t.observations) {
        if (o.type !== 'generation') continue;
        const v = (o.input_tokens || 0) + (o.cache_read_tokens || 0) + (o.cache_write_tokens || 0);
        if (!v) continue;
        k++;
        gl.push((t.index + 1) + '.' + k);
        gv.push(v);
      }
    }
    const context = M.charts.line({
      labels: gl, area: true, format: M.fmtTokens, height: 180, empty: 'No generation usage recorded.',
      series: [{ name: 'context', values: gv }],
    });
    return h('div', { class: 'panels' },
      M.panel('Tokens per turn', tokens),
      M.panel('Cost per turn', cost),
      M.panel('Context per generation (input + cache)', context));
  }

  // set by the session page: jump to a turn by index
  let ctxGoTo = () => {};

  function filesPanel(files) {
    return M.panel('Files touched (' + files.length + ')', table({
      sortKey: 'last_ns', sortDesc: true,
      empty: 'No observation named a file.',
      columns: [
        { key: 'path', label: 'path', class: 'ellipsis', title: (f) => f.path, render: (f) => h('code', { text: f.path }) },
        { key: 'count', label: 'calls', num: true },
        { key: 'tools', label: 'tools', render: (f) => (f.tools || []).join(', '), sortable: false },
        { key: 'errors', label: 'errors', num: true, render: (f) => f.errors ? h('span', { class: 'badge error', text: String(f.errors) }) : '0' },
        { key: 'last_ns', label: 'last', render: (f) => M.fmtTime(f.last_ns) },
      ],
      rows: files,
    }));
  }

  M.registerPage({
    id: 'session',
    navId: 'sessions',
    nav: false,
    title: (p) => 'Session ' + p.key,
    path: '/sessions/:key',
    async render(el, params) {
      sessionStyles();
      const key = params.key;
      const launchId = params.launch_id || '';
      const q = launchId ? '?launch_id=' + encodeURIComponent(launchId) : '';
      const base = '/api/session/' + encodeURIComponent(key) + '/transcript';
      const exportHref = '/api/export/session/' + encodeURIComponent(key) + '.html' + q;
      const getJSON = async (url) => {
        const res = await fetch(url, { headers: { accept: 'application/json' } });
        const body = await res.json().catch(() => ({}));
        if (!res.ok || body.ok === false) {
          const err = new Error(body.error || ('HTTP ' + res.status));
          err.status = res.status;
          throw err;
        }
        return body;
      };
      const page = (from, extra) => getJSON(base + '?' + new URLSearchParams(Object.assign({ from_turn: String(from) }, launchId ? { launch_id: launchId } : {}, extra || {})).toString());

      el.appendChild(loading());
      const args = { session_key: key };
      if (launchId) args.launch_id = launchId;
      const [sessionEnv, first] = await Promise.all([
        api('/api/trace/get_session', args).catch((err) => ({ error: err })),
        page(0, params.turn ? { turn: params.turn } : null),
      ]);
      const state = { tx: first, turns: first.turns.slice(), card: sessionEnv.error ? null : sessionEnv.data.session, current: -1 };
      // a link to one turn (Search, Tools, a copied # link) loads up to it,
      // however far past the first page it is
      if (params.turn && first.turn_index != null) {
        let guard = 0;
        while (state.turns.length <= first.turn_index && state.turns.length < state.tx.total_turns && guard++ < 50) {
          const need = first.turn_index - state.turns.length + 20;
          const more = await page(state.turns.length, { limit: String(Math.min(2000, Math.max(200, need))) });
          if (!more.turns.length) break;
          state.turns = state.turns.concat(more.turns);
          state.tx = more;
        }
      }
      el.innerHTML = '';

      const ctx = { key, launchId, metadataOnly: first.content_mode === 'metadata' };
      const exportBtn = h('a', { class: 'button', href: exportHref + (q ? '&' : '?') + 'download=1', download: 'session-' + key.replace(/[^A-Za-z0-9._-]/g, '_') + '.html', title: 'Download the transcript as one self-contained HTML file' }, 'Download HTML');
      const liveBadge = h('span', { class: 'badge live', text: 'live', hidden: true });
      el.appendChild(h('div', { class: 'toolbar' },
        link('/sessions', '← Sessions'),
        h('span', { style: 'flex: 1' }),
        liveBadge,
        h('a', { class: 'button', href: exportHref, target: '_blank', rel: 'noopener', text: 'Open export' }),
        exportBtn));

      const headerBox = h('div');
      const chartsBox = h('div');
      const filesBox = h('div');
      const outline = h('nav', { class: 'sv-outline', 'aria-label': 'Turns' });
      const turnsBox = h('div', { class: 'sv-turns' });
      const moreBox = h('div');
      el.appendChild(headerBox);
      el.appendChild(chartsBox);
      el.appendChild(h('div', { class: 'sv-grid' }, outline, h('div', null,
        h('h2', { text: 'Transcript' }),
        h('div', { class: 'muted', text: 'j/k or ]/[ step through the turns.' }),
        turnsBox, moreBox)));
      el.appendChild(filesBox);

      function drawHeader() {
        const tx = state.tx;
        const card = state.card || {};
        const t = tx.totals;
        headerBox.innerHTML = '';
        if (tx.title) headerBox.appendChild(h('div', { class: 'muted', text: tx.title }));
        headerBox.appendChild(cards([
          ['duration', M.fmtDuration(tx.duration_ms)],
          ['turns', String(tx.total_turns) + (card.open_turns ? ' (' + card.open_turns + ' open)' : '')],
          ['tokens', M.fmtTokens(t.total_tokens), 'in ' + M.fmtTokens(t.input_tokens) + ' · out ' + M.fmtTokens(t.output_tokens) + ' · cache ' + M.fmtTokens(t.cache_read_tokens + t.cache_write_tokens)],
          ['cost', M.fmtCost(t.cost_usd)],
          ['lines', t.lines_added == null && t.lines_removed == null ? '–' : '+' + (t.lines_added || 0) + ' −' + (t.lines_removed || 0)],
          ['tool calls', String(t.tool_calls), t.errors ? t.errors + ' error' + (t.errors > 1 ? 's' : '') : null],
        ]));
        headerBox.appendChild(kv([
          ['session', tx.session_key],
          ['harness', tx.provider],
          ['model', (tx.models || []).join(', ') || null],
          ['workspace', tx.cwd],
          ['launch', launchesNode(tx.launches || [])],
          ['state', state.card ? stateBadge(card.runtime_state) : null],
          ['outcome', card.task_outcome],
          ['last active', card.last_active_ns ? M.fmtTime(card.last_active_ns) : null],
          ['initial goal', evidenceText(card.initial_goal)],
          ['current activity', evidenceText(card.current_activity)],
        ]));
        if (tx.content_mode === 'metadata') {
          headerBox.appendChild(h('div', { class: 'sv-banner', text: 'Metadata only: this session was traced with content_mode = metadata, so prompts, outputs and tool inputs and outputs were not stored. Turns, timings, tools, tokens and cost are still shown.' }));
        } else if (tx.content_mode === 'mixed') {
          headerBox.appendChild(h('div', { class: 'sv-banner', text: 'Some launches of this session were traced metadata-only: their prompts, outputs and tool inputs were not stored.' }));
        }
        if (sessionEnv.error) headerBox.appendChild(h('div', { class: 'muted note', text: 'Session card unavailable: ' + sessionEnv.error.message }));
      }

      function drawCharts() {
        chartsBox.replaceChildren(charts(state.turns));
      }

      function drawFiles() {
        filesBox.replaceChildren(filesPanel(state.tx.files || []));
      }

      function outlineItem(t) {
        const max = Math.max(1, ...state.turns.map((x) => x.total_tokens || 0));
        const label = (t.input || '').replace(/\s+/g, ' ').trim() || (t.observations.length + ' observations');
        const a = h('a', { href: '#', 'data-index': String(t.index), class: t.errors ? 'err' : null, title: label },
          h('span', { class: 'n', text: String(t.index + 1) }),
          h('span', { class: 't', text: label }),
          h('span', { class: 'bar', style: 'width:' + Math.max(2, Math.round(28 * (t.total_tokens || 0) / max)) + 'px' }));
        a.addEventListener('click', (e) => { e.preventDefault(); goTo(t.index); });
        return a;
      }

      function drawOutline() {
        outline.replaceChildren(...state.turns.map(outlineItem));
        markCurrent();
      }

      function drawMore() {
        moreBox.innerHTML = '';
        const left = state.tx.total_turns - state.turns.length;
        if (left > 0) {
          const btn = h('button', { type: 'button', text: 'Load ' + Math.min(200, left) + ' more of ' + left + ' turns' });
          btn.addEventListener('click', async () => {
            btn.disabled = true;
            const more = await page(state.turns.length);
            state.tx = more;
            for (const t of more.turns) { state.turns.push(t); turnsBox.appendChild(turnElement(t, ctx)); }
            drawOutline(); drawCharts(); drawMore();
          });
          moreBox.appendChild(btn);
        }
      }

      function markCurrent() {
        turnsBox.querySelectorAll('.sv-turn.current').forEach((x) => x.classList.remove('current'));
        outline.querySelectorAll('a.current').forEach((x) => x.classList.remove('current'));
        if (state.current < 0) return;
        const sec = turnsBox.querySelector('[data-index="' + state.current + '"]');
        if (sec) sec.classList.add('current');
        const a = outline.querySelector('a[data-index="' + state.current + '"]');
        if (a) { a.classList.add('current'); a.scrollIntoView({ block: 'nearest' }); }
      }

      function goTo(index) {
        const sec = turnsBox.querySelector('[data-index="' + index + '"]');
        if (!sec) return;
        state.current = index;
        markCurrent();
        sec.scrollIntoView({ block: 'start', behavior: 'smooth' });
      }
      ctxGoTo = goTo;

      // the turn nearest the top of the viewport
      function visibleIndex() {
        const secs = turnsBox.querySelectorAll('.sv-turn');
        let best = -1;
        for (const s of secs) {
          const top = s.getBoundingClientRect().top;
          if (top <= 120) best = Number(s.dataset.index); else break;
        }
        return best;
      }

      function step(d) {
        if (!state.turns.length) return;
        const at = state.current >= 0 ? state.current : visibleIndex();
        const first = state.turns[0].index;
        const last = state.turns[state.turns.length - 1].index;
        const next = at < 0 ? first : Math.max(first, Math.min(last, at + d));
        goTo(next);
      }

      drawHeader();
      drawCharts();
      for (const t of state.turns) turnsBox.appendChild(turnElement(t, ctx));
      if (!state.turns.length) turnsBox.appendChild(M.empty(ctx.metadataOnly ? 'No turns recorded (metadata only).' : 'No turns recorded yet.'));
      drawOutline();
      drawMore();
      drawFiles();
      if (params.turn) {
        const t = state.turns.find((x) => x.id === params.turn);
        if (t) setTimeout(() => { state.current = t.index; markCurrent(); const s = turnsBox.querySelector('[data-index="' + t.index + '"]'); if (s) s.scrollIntoView({ block: 'start' }); }, 0);
      }

      // keys: j/k and ]/[ step through the turns (before the shell's row keys)
      const onKey = (e) => {
        if (e.metaKey || e.ctrlKey || e.altKey) return;
        const t = e.target;
        if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
        if (document.querySelector('.overlay:not([hidden])') || document.body.classList.contains('modal-open')) return;
        const d = e.key === 'j' || e.key === ']' ? 1 : e.key === 'k' || e.key === '[' ? -1 : 0;
        if (!d) return;
        e.preventDefault();
        e.stopPropagation();
        step(d);
      };
      document.addEventListener('keydown', onKey, true);
      M.onLeave(() => document.removeEventListener('keydown', onKey, true));

      // live: while the session runs, fetch the turns from the first open one
      // (or the last loaded) on each store change, keeping the scroll anchor
      let unsub = null;
      let timer = null;
      let busy = false;
      // live: a live runtime state, or (the state can lag) an open turn or
      // activity read from the live process
      const isLive = () => {
        const c = state.card;
        if (!c) return state.turns.some((t) => t.status === 'open');
        return LIVE_STATES.includes(c.runtime_state) || c.open_turns > 0 ||
          !!(c.current_activity && c.current_activity.source === 'live_process');
      };
      async function refresh() {
        if (busy) { timer = setTimeout(refresh, 500); return; }
        busy = true;
        try {
          const allLoaded = state.turns.length >= state.tx.total_turns;
          if (!allLoaded) {
            const top = await page(0); // header and totals only
            state.tx = Object.assign({}, top, { turns: [] });
          } else {
            let from = state.turns.findIndex((t) => t.status === 'open');
            if (from < 0) from = Math.max(0, state.turns.length - 1);
            const fromIndex = state.turns.length ? state.turns[from].index : 0;
            const fresh = await page(fromIndex);
            if (!el.isConnected) return;
            const anchor = scrollAnchor(turnsBox);
            const nearBottom = window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - 40;
            state.turns = state.turns.slice(0, from).concat(fresh.turns);
            state.tx = fresh;
            for (const t of fresh.turns) {
              const old = turnsBox.querySelector('[data-index="' + t.index + '"]');
              const neu = turnElement(t, ctx);
              // keep what the reader opened
              if (old) {
                const opened = Array.from(old.querySelectorAll('details')).map((d) => d.open);
                const ds = neu.querySelectorAll('details');
                opened.forEach((o, i) => { if (ds[i]) ds[i].open = o; });
                old.replaceWith(neu);
              } else {
                const empty = turnsBox.querySelector('.empty');
                if (empty) empty.remove();
                turnsBox.appendChild(neu);
              }
            }
            restoreAnchor(anchor, nearBottom);
            drawOutline();
            drawCharts();
            drawMore();
          }
          const env = await api('/api/trace/get_session', args).catch(() => null);
          if (env) state.card = env.data.session;
          drawHeader();
          drawFiles();
          if (!isLive()) stopLive();
        } catch (err) {
          console.error('session refresh', err);
        } finally {
          busy = false;
        }
      }
      function startLive() {
        if (unsub || !M.live) return;
        unsub = M.live.subscribe(() => { clearTimeout(timer); timer = setTimeout(refresh, 300); });
        liveBadge.hidden = false;
      }
      function stopLive() {
        if (unsub) { unsub(); unsub = null; }
        clearTimeout(timer);
        liveBadge.hidden = true;
      }
      if (isLive()) startLive();
      M.onLeave(stopLive);
    },
  });

  // The first turn at or below the top of the viewport and its offset, to
  // put the reader back where they were after the turns change.
  function scrollAnchor(box) {
    for (const s of box.querySelectorAll('.sv-turn')) {
      const r = s.getBoundingClientRect();
      if (r.bottom > 0) return { index: s.dataset.index, top: r.top };
    }
    return null;
  }

  function restoreAnchor(anchor, toBottom) {
    if (toBottom) { window.scrollTo(0, document.documentElement.scrollHeight); return; }
    if (!anchor) return;
    const s = document.querySelector('.sv-turn[data-index="' + anchor.index + '"]');
    if (!s) return;
    const d = s.getBoundingClientRect().top - anchor.top;
    if (d) window.scrollBy(0, d);
  }

  // ---- Search ---------------------------------------------------------------

  // A search match's turn: source ids are `trace:<trace id>:input|output`.
  function matchTurn(r) {
    const m = /^trace:(.+):(input|output)$/.exec(r.source_id || '');
    return m ? m[1] : null;
  }

  function turnLink(r) {
    const where = (r.source_type || '').replace('trace_', '') || 'match';
    const turn = matchTurn(r);
    if (!r.session_key || !turn) return where;
    const q = { turn };
    if (r.launch_id) q.launch_id = r.launch_id;
    return link('/sessions/' + encodeURIComponent(r.session_key), 'turn ' + where, q);
  }

  M.registerPage({
    id: 'search',
    title: 'Search',
    path: '/search',
    async render(el, params) {
      const q = params.q || '';
      const form = h('form', { class: 'filters' },
        h('label', null, 'query', h('input', { type: 'search', name: 'q', value: q, placeholder: 'words in prompts, outputs, tool calls', autofocus: true })),
        h('label', null, 'harness', h('select', { name: 'provider' }, PROVIDERS.map((p) => h('option', { value: p, selected: p === (params.provider || '') || null }, p || 'any')))),
        h('label', null, 'since', h('input', { name: 'since', value: params.since || '', placeholder: '24h, 7d or RFC 3339' })),
        h('button', { type: 'submit', class: 'primary' }, 'Search'),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const fd = new FormData(form);
        M.navigate('/search', { q: fd.get('q'), provider: fd.get('provider'), since: fd.get('since') });
      });
      el.appendChild(form);
      if (!q) {
        el.appendChild(h('div', { class: 'empty', text: 'Type a query to search the trace store.' }));
        return;
      }
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading('Searching…'));
      const body = { query: q, limit: 100 };
      if (params.provider) body.provider = params.provider;
      if (params.since) body.since = M.sinceToRfc3339(params.since);
      const env = await api('/api/trace/search_traces', body);
      out.innerHTML = '';
      out.appendChild(h('div', { class: 'muted', text: env.data.matches.length + ' of ' + env.data.total_matches + ' matches' }));
      const note = envelopeNote(env);
      if (note) out.appendChild(note);
      out.appendChild(table({
        sortKey: 'timestamp', sortDesc: true,
        empty: 'No match.',
        onRow: (r) => {
          if (!r.session_key) return;
          const q = {};
          const turn = matchTurn(r);
          if (turn) q.turn = turn;
          if (turn && r.launch_id) q.launch_id = r.launch_id;
          M.navigate('/sessions/' + encodeURIComponent(r.session_key), q);
        },
        columns: [
          { key: 'timestamp', label: 'time', render: (r) => M.fmtTime(r.timestamp) },
          { key: 'provider', label: 'harness' },
          { key: 'session_key', label: 'session', render: (r) => sessionLink(r.session_key), class: 'ellipsis' },
          { key: 'source_type', label: 'where', render: (r) => turnLink(r) },
          { key: 'snippet', label: 'snippet', class: 'wrap', sortable: false },
        ],
        rows: env.data.matches || [],
      }));
    },
  });

  // ---- Skills & Agents ---------------------------------------------------------

  M.registerPage({
    id: 'skills',
    title: 'Skills & Agents',
    path: '/skills',
    async render(el, params) {
      const form = h('form', { class: 'filters' },
        h('label', null, 'skill', h('input', { name: 'skill', value: params.skill || '', placeholder: 'name (optional)' })),
        h('label', null, 'harness', h('select', { name: 'provider' }, PROVIDERS.map((p) => h('option', { value: p, selected: p === (params.provider || '') || null }, p || 'any')))),
        h('label', null, 'since', h('input', { name: 'since', value: params.since || '', placeholder: '30d, 7d or RFC 3339' })),
        h('button', { type: 'submit', class: 'primary' }, 'Apply'),
      );
      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const fd = new FormData(form);
        M.navigate('/skills', { skill: fd.get('skill'), provider: fd.get('provider'), since: fd.get('since') });
      });
      el.appendChild(form);
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading('Analyzing…'));
      const body = { limit: 100 };
      if (params.skill) body.skill = params.skill;
      if (params.provider) body.provider = params.provider;
      if (params.since) body.since = M.sinceToRfc3339(params.since);
      const env = await api('/api/trace/analyze_skills', body);
      out.innerHTML = '';
      const rows = env.data.skills || [];
      out.appendChild(h('div', { class: 'muted', text: rows.length + ' of ' + env.data.total_skills + ' skills and agents' }));
      const note = envelopeNote(env);
      if (note) out.appendChild(note);
      out.appendChild(table({
        sortKey: 'turns_loaded', sortDesc: true,
        empty: 'No skill or agent attribution in the window.',
        columns: [
          { key: 'skill_name', label: 'skill / agent' },
          { key: 'turns_loaded', label: 'turns', num: true },
          { key: 'attributed_calls', label: 'calls', num: true },
          { key: 'attributed_tokens', label: 'tokens', num: true, render: (r) => M.fmtTokens(r.attributed_tokens) },
          { key: 'attributed_cost_usd', label: 'cost', num: true, render: (r) => M.fmtCost(r.attributed_cost_usd) },
          { key: 'error_count', label: 'errors', num: true },
          { key: 'schema_error_count', label: 'schema err', num: true },
          { key: 'p50_ms', label: 'p50', num: true, render: (r) => M.fmtDuration(r.p50_ms) },
          { key: 'p95_ms', label: 'p95', num: true, render: (r) => M.fmtDuration(r.p95_ms) },
          { key: 'max_ms', label: 'max', num: true, render: (r) => M.fmtDuration(r.max_ms) },
          { key: 'slow_calls_above_4s', label: '>4s', num: true },
          { key: 'ongoing_count', label: 'ongoing', num: true },
          { key: 'limitations', label: 'limitations', class: 'wrap', sortable: false, render: (r) => (r.limitations || []).join('; ') || '–' },
        ],
        rows,
      }));
    },
  });

  // ---- Health ---------------------------------------------------------------

  M.registerPage({
    id: 'health',
    title: 'Health',
    path: '/health',
    async render(el) {
      el.appendChild(loading());
      const env = await api('/api/trace/get_health', {});
      el.innerHTML = '';
      const d = env.data;
      el.appendChild(kv([
        ['store', h('span', { class: d.db_available ? 'ok' : 'warn', text: d.db_available ? 'available' : 'unavailable' })],
        ['db path', d.db_path],
        ['schema', String(d.schema_version)],
        ['reader', d.reader_version],
        ['collector freshness', d.collector_freshness ? M.fmtTime(d.collector_freshness) : 'no live snapshot'],
        ['content mode', d.content_mode],
        ['providers', (d.provider_coverage || []).join(', ') || '–'],
        ['features', (d.features || []).join(', ') || '–'],
        ['scope', env.scope ? (env.scope.workspace || (env.scope.all_workspaces ? 'all workspaces' : JSON.stringify(env.scope))) : '–'],
        ['as of', M.fmtTime(env.as_of)],
      ]));
      const note = envelopeNote(env);
      if (note) el.appendChild(note);
      el.appendChild(h('h2', { text: 'Envelope' }));
      el.appendChild(h('pre', { class: 'block', text: JSON.stringify(env, null, 2) }));
    },
  });
})();
