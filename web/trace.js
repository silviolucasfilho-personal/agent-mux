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

  function evidenceText(ev) {
    if (!ev) return null;
    if (typeof ev === 'string') return ev;
    return ev.value === null || ev.value === undefined ? null : ev.value;
  }

  function observationTable(observations) {
    return table({
      sortable: false,
      empty: 'No observations in this turn.',
      columns: [
        { key: 'start_time', label: 'start', render: (o) => M.fmtTime(o.start_time) },
        { key: 'observation_type', label: 'type' },
        { key: 'name', label: 'name', class: 'ellipsis' },
        { key: 'duration_ms', label: 'duration', num: true, render: (o) => M.fmtDuration(o.duration_ms) },
        { key: 'is_error', label: 'status', render: (o) => o.is_error ? h('span', { class: 'badge error', text: o.error_message || 'error' }) : h('span', { class: 'ok', text: 'ok' }) },
      ],
      rows: observations || [],
    });
  }

  M.registerPage({
    id: 'session',
    navId: 'sessions',
    nav: false,
    title: (p) => 'Session ' + p.key,
    path: '/sessions/:key',
    async render(el, params) {
      el.appendChild(h('div', { class: 'toolbar' }, link('/sessions', '← Sessions')));
      el.appendChild(loading());
      const args = { session_key: params.key };
      if (params.launch_id) args.launch_id = params.launch_id;
      const [sessionEnv, timelineEnv] = await Promise.all([
        api('/api/trace/get_session', args),
        api('/api/trace/get_timeline', Object.assign({ limit: 100 }, args)).catch((err) => ({ error: err })),
      ]);
      el.innerHTML = '';
      el.appendChild(h('div', { class: 'toolbar' }, link('/sessions', '← Sessions')));
      const card = sessionEnv.data.session;
      const binding = sessionEnv.data.binding || {};
      el.appendChild(cards([
        ['turns', String(card.completed_turns) + (card.open_turns ? ' +' + card.open_turns + ' open' : '')],
        ['tools', String(card.total_tools)],
        ['tokens', M.fmtTokens(card.total_tokens)],
        ['cost', M.fmtCost(card.total_cost_usd)],
        ['duration', M.fmtDuration(card.duration_ms)],
      ]));
      el.appendChild(kv([
        ['session', card.session_key],
        ['launch', binding.launch_id || card.launch_id],
        ['harness', card.provider],
        ['workspace', card.cwd],
        ['state', stateBadge(card.runtime_state)],
        ['outcome', card.task_outcome],
        ['last active', M.fmtTime(card.last_active_ns)],
        ['initial goal', evidenceText(card.initial_goal)],
        ['current activity', evidenceText(card.current_activity)],
        ['active tools', (card.active_tools || []).join(', ') || null],
      ]));
      const note = envelopeNote(sessionEnv);
      if (note) el.appendChild(note);
      if (sessionEnv.data.correlation_warnings && sessionEnv.data.correlation_warnings.length) {
        el.appendChild(h('div', { class: 'warn', text: sessionEnv.data.correlation_warnings.join(' · ') }));
      }

      el.appendChild(h('h2', { text: 'Tools' }));
      el.appendChild(table({
        sortKey: 'count', sortDesc: true,
        empty: 'No tool calls recorded.',
        columns: [{ key: 'name', label: 'tool' }, { key: 'count', label: 'calls', num: true }],
        rows: card.tool_counts || [],
      }));

      // sub-agents: observations of type agent in the timeline
      const turns = timelineEnv.error ? [] : (timelineEnv.data.turns || []);
      const agents = [];
      for (const t of turns) for (const o of t.observations || []) if (o.observation_type === 'agent') agents.push(Object.assign({ turn_id: t.turn_id }, o));
      el.appendChild(h('h2', { text: 'Subagents' }));
      el.appendChild(table({
        sortable: false,
        empty: 'No sub-agent observed in the loaded turns.',
        columns: [
          { key: 'name', label: 'agent' },
          { key: 'turn_id', label: 'turn', class: 'ellipsis' },
          { key: 'start_time', label: 'start', render: (o) => M.fmtTime(o.start_time) },
          { key: 'duration_ms', label: 'duration', num: true, render: (o) => M.fmtDuration(o.duration_ms) },
          { key: 'is_error', label: 'status', render: (o) => o.is_error ? 'error' : 'ok' },
        ],
        rows: agents,
      }));

      const files = (card.files_modified || []).map(evidenceText).filter(Boolean);
      const commands = (card.recent_commands || []).map(evidenceText).filter(Boolean);
      if (files.length) {
        el.appendChild(h('h2', { text: 'Files modified' }));
        el.appendChild(h('ul', { class: 'plain' }, files.map((f) => h('li', { text: f }))));
      }
      if (commands.length) {
        el.appendChild(h('h2', { text: 'Recent commands' }));
        el.appendChild(h('ul', { class: 'plain' }, commands.map((c) => h('li', null, h('code', { text: c })))));
      }
      const lastOut = card.last_assistant_output && evidenceText(card.last_assistant_output);
      if (lastOut) {
        el.appendChild(h('h2', { text: 'Last assistant output' }));
        el.appendChild(h('pre', { class: 'block', text: lastOut }));
      }

      el.appendChild(h('h2', { text: 'Timeline' }));
      if (timelineEnv.error) {
        el.appendChild(M.errorBox('Timeline unavailable: ' + timelineEnv.error.message));
        return;
      }
      el.appendChild(h('div', { class: 'muted', text: turns.length + ' of ' + timelineEnv.data.total_turns + ' turns' + (timelineEnv.next_cursor ? ' (more not loaded)' : '') }));
      if (!turns.length) el.appendChild(h('div', { class: 'empty', text: 'No turns recorded yet.' }));
      turns.forEach((t, i) => {
        const details = h('details', { class: 'turn', open: i < 3 || null });
        const errors = (t.observations || []).filter((o) => o.is_error).length;
        details.appendChild(h('summary', null,
          'turn ' + (i + 1),
          h('span', { class: 'muted', text: M.fmtTime(t.start_time) + ' · ' + M.fmtDuration(t.duration_ms) + ' · ' + (t.observations || []).length + ' observations' }),
          errors ? h('span', { class: 'badge error', text: errors + ' error' + (errors > 1 ? 's' : '') }) : null,
        ));
        const body = h('div', { class: 'body' });
        if (t.input_snippet) body.appendChild(h('div', { class: 'snippet' }, h('span', { class: 'who', text: 'user  ' }), t.input_snippet));
        if (t.output_snippet) body.appendChild(h('div', { class: 'snippet' }, h('span', { class: 'who', text: 'agent ' }), t.output_snippet));
        body.appendChild(observationTable(t.observations));
        details.appendChild(body);
        el.appendChild(details);
      });
    },
  });

  // ---- Search ---------------------------------------------------------------

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
        columns: [
          { key: 'timestamp', label: 'time', render: (r) => M.fmtTime(r.timestamp) },
          { key: 'provider', label: 'harness' },
          { key: 'session_key', label: 'session', render: (r) => sessionLink(r.session_key), class: 'ellipsis' },
          { key: 'source_type', label: 'where' },
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
