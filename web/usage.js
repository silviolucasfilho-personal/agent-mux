// Usage pages: the Overview dashboard (the landing page) and Usage
// (tokens, cost and activity). Both read /api/usage/ (src/web/usage.rs),
// whose numbers sum generation observations by their own timestamps, so a
// window holds only what was spent inside it. Loaded after trace.js.
(function () {
  'use strict';
  const M = window.AgentMux;
  const { h, charts } = M;

  // The usage body of the shared filters: the window, the harness, the
  // project fragment and the browser's offset for day/hour buckets.
  function usageBody(params, extra) {
    const f = M.filters(params);
    const body = { tz_offset_min: -new Date().getTimezoneOffset() };
    if (f.since) body.since = f.since;
    if (f.until) body.until = f.until;
    if (f.provider) body.provider = f.provider;
    if (f.workspace) body.project = f.workspace;
    return Object.assign(body, extra || {});
  }

  function usage(route, params, extra) {
    return M.api('/api/usage/' + route, usageBody(params, extra));
  }

  // A local midnight for a 'YYYY-MM-DD' day (Date parses a bare day as UTC).
  const localDay = (d) => (d ? d + 'T00:00:00' : d);
  const short = (d) => String(d).slice(5);
  const pct = (v) => (v === null || v === undefined ? '–' : M.fmtPercent(v));
  const num = (v) => Number(v) || 0;
  const clip = (s, n) => { s = String(s).replace(/\s+/g, ' ').trim(); return s.length > n ? s.slice(0, n - 1) + '…' : s; };

  function stored(key, fallback) {
    try { return localStorage.getItem(key) || fallback; } catch (_) { return fallback; }
  }
  function store(key, value) {
    try { localStorage.setItem(key, value); } catch (_) { /* private window */ }
  }

  // A panel whose chart can be drawn as bars or as a line; the choice is a
  // per-viewer convenience kept in localStorage.
  function toggledChart(title, key, draw) {
    let mode = stored(key, 'bars');
    const holder = h('div');
    const seg = h('div', { class: 'segmented', role: 'group', 'aria-label': 'Chart type', style: 'float:right;margin-top:-4px' });
    const paint = () => {
      holder.innerHTML = '';
      holder.appendChild(draw(mode));
      seg.querySelectorAll('button').forEach((b) => {
        const on = b.dataset.mode === mode;
        b.classList.toggle('on', on);
        b.setAttribute('aria-pressed', on ? 'true' : 'false');
      });
    };
    for (const m of ['bars', 'line']) {
      seg.appendChild(h('button', { type: 'button', 'data-mode': m, onclick: () => { mode = m; store(key, m); paint(); } }, m));
    }
    const p = M.panel(null, h('div', { style: 'overflow:hidden' }, seg, h('h2', { class: 'panel-title', text: title })), holder);
    paint();
    return p;
  }

  function costSourceNote() {
    return h('p', { class: 'muted', style: 'font-size:12px;margin:4px 0 0' },
      'Cost is the sum of each call\'s own cost in the window (agent-mux\'s price, or the provider\'s figure when the transcript carries one). ' +
      'The harness\'s reported session total is shown apart as "reported".');
  }

  // ---- Overview ------------------------------------------------------------------

  M.registerPage({
    id: 'overview',
    title: 'Overview',
    path: '/overview',
    group: 'Overview',
    live: true,
    async render(el, params) {
      el.appendChild(M.filterBar({ params }));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(M.loading('Loading usage…'));
      const v = await usage('overview', params);
      out.innerHTML = '';
      const t = v.totals;
      const days = v.days || [];
      const byDay = new Map((v.daily || []).map((d) => [d.day, d]));
      const series = (k) => days.map((d) => num((byDay.get(d) || {})[k]));
      const labels = days.map(short);
      const live = (v.live && v.live.sessions) || [];

      if (!t.sessions && !t.generations && !live.length) {
        out.appendChild(M.empty('No usage in this range.', 'Widen the date range or clear the filters; sessions appear as the TUI traces them.'));
        return;
      }
      out.appendChild(M.cards([
        ['Sessions', M.fmtNum(t.sessions), charts.sparkline(series('sessions'), { labels })],
        ['Launches', M.fmtNum(t.launches), t.lines_added || t.lines_removed ? '+' + M.fmtNum(t.lines_added) + ' / −' + M.fmtNum(t.lines_removed) + ' lines' : null],
        ['Turns', M.fmtNum(t.turns), charts.sparkline(series('turns'), { labels, color: 3 })],
        ['Tokens', M.fmtTokens(t.total_tokens), charts.sparkline(series('total_tokens'), { labels, format: M.fmtTokens, color: 1 })],
        ['Cost', M.fmtCost(t.cost_usd), charts.sparkline(series('cost_usd'), { labels, format: M.fmtCost, color: 2 })],
        ['Live now', M.fmtNum(live.length), (v.live && v.live.available) ? 'from the TUI snapshots' : 'no TUI running'],
      ]));
      out.appendChild(M.panel('Tokens', M.kv([
        ['Input', M.fmtNum(t.input_tokens)],
        ['Output', M.fmtNum(t.output_tokens)],
        ['Cache read', M.fmtNum(t.cache_read_tokens)],
        ['Cache write', M.fmtNum(t.cache_write_tokens)],
        ['Reasoning (part of output)', M.fmtNum(t.reasoning_tokens)],
        ['Generations', M.fmtNum(t.generations) + (t.unpriced_generations ? ' (' + M.fmtNum(t.unpriced_generations) + ' unpriced)' : '')],
        ['Reported by the harnesses', t.reported_cost_usd === null ? '–' : M.fmtCost(t.reported_cost_usd)],
      ])));

      const grid = h('div', { class: 'panels', style: 'margin-top:16px' });
      grid.appendChild(toggledChart('Daily spend', 'agent-mux-overview-spend', (mode) => {
        const s = [{ name: 'cost', values: series('cost_usd') }];
        const onClick = (i) => M.navigate('/usage', { range: 'custom', from: days[i], to: days[i] });
        return mode === 'line'
          ? charts.line({ labels, series: s, area: true, format: M.fmtCost })
          : charts.bar({ labels, series: s, format: M.fmtCost, onClick });
      }));
      const cal = v.calendar || {};
      grid.appendChild(M.panel('Activity (turns per day)', charts.heatmap({
        values: cal.turns || {},
        from: localDay(cal.from),
        to: localDay(cal.to),
        format: (n) => M.fmtNum(n) + ' turns',
        onClick: (day) => M.navigate('/usage', { range: 'custom', from: M.dayKey(day) || day, to: M.dayKey(day) || day }),
      })));
      grid.appendChild(M.panel('Top projects', charts.hbars({
        items: (v.top_projects || []).map((p) => ({
          label: M.shortPath(p.project), title: p.project, value: p.cost_usd,
          hint: M.fmtNum(p.sessions) + ' sessions · ' + M.fmtTokens(p.total_tokens),
          href: M.hrefFor('/usage', Object.assign({}, params, { workspace: p.project })),
        })),
        format: M.fmtCost, limit: 8,
      })));
      grid.appendChild(M.panel('Top models', charts.hbars({
        items: (v.top_models || []).map((m) => ({ label: m.model, value: m.cost_usd, hint: M.fmtTokens(m.total_tokens) + ' tokens' })),
        format: M.fmtCost, limit: 8, color: 1,
      })));
      grid.appendChild(M.panel('Harnesses', charts.hbars({
        items: (v.top_harnesses || []).map((p) => ({
          label: p.provider, value: p.cost_usd,
          hint: M.fmtNum(p.sessions) + ' sessions · ' + M.fmtNum(p.launches) + ' launches',
          href: M.hrefFor('/usage', Object.assign({}, params, { provider: p.provider })),
        })),
        format: M.fmtCost, color: 2,
      })));
      out.appendChild(grid);
      out.appendChild(M.panel('Live sessions now', M.table({
        columns: [
          { key: 'provider', label: 'Harness' },
          { key: 'state', label: 'State' },
          { key: 'session_key', label: 'Session', render: (r) => (r.session_key ? M.link('/sessions/' + encodeURIComponent(r.session_key), r.session_key, { launch_id: r.launch_id }) : r.launch_id) },
          { key: 'cwd', label: 'Workspace', render: (r) => M.shortPath(r.cwd), title: (r) => r.cwd },
          { key: 'active_tools', label: 'Active tools', render: (r) => (r.active_tools || []).join(', ') || '–' },
          { key: 'updated_at_ns', label: 'Updated', num: true, render: (r) => M.fmtAgo(r.updated_at_ns) },
        ],
        rows: live,
        empty: 'No live sessions.',
        emptyHint: 'Sessions running in the agent-mux TUI appear here.',
      })));
      out.appendChild(costSourceNote());
    },
  });

  // ---- Usage ------------------------------------------------------------------------

  const tok = (k) => ({ key: k, num: true, render: (r) => M.fmtTokens(r[k]) });

  function modelTable(rows) {
    return M.table({
      columns: [
        { key: 'model', label: 'Model' },
        { key: 'generations', label: 'Calls', num: true, render: (r) => M.fmtNum(r.generations) },
        { key: 'sessions', label: 'Sessions', num: true },
        Object.assign(tok('input_tokens'), { label: 'Input' }),
        Object.assign(tok('output_tokens'), { label: 'Output' }),
        Object.assign(tok('cache_read_tokens'), { label: 'Cache read' }),
        Object.assign(tok('cache_write_tokens'), { label: 'Cache write' }),
        Object.assign(tok('reasoning_tokens'), { label: 'Reasoning' }),
        { key: 'cost_usd', label: 'Cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
        { key: 'share', label: 'Share', num: true, render: (r) => pct(r.share) },
        { key: 'unpriced_generations', label: 'Unpriced', num: true, title: () => 'calls with tokens but no price row and no provider cost' },
      ],
      rows, sortKey: 'cost_usd', sortDesc: true, empty: 'No model calls in this range.',
    });
  }

  function projectTable(rows, params) {
    return M.table({
      columns: [
        { key: 'project', label: 'Project / workspace', render: (r) => M.link('/usage', M.shortPath(r.project), Object.assign({}, params, { workspace: r.project })), title: (r) => r.project },
        { key: 'sessions', label: 'Sessions', num: true },
        { key: 'turns', label: 'Turns', num: true },
        Object.assign(tok('total_tokens'), { label: 'Tokens' }),
        { key: 'cost_usd', label: 'Cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
        { key: 'share', label: 'Share', num: true, render: (r) => pct(r.share) },
      ],
      rows, sortKey: 'cost_usd', sortDesc: true, empty: 'No projects in this range.',
    });
  }

  function harnessTable(rows) {
    return M.table({
      columns: [
        { key: 'provider', label: 'Harness' },
        { key: 'sessions', label: 'Sessions', num: true },
        { key: 'launches', label: 'Launches', num: true },
        { key: 'turns', label: 'Turns', num: true },
        Object.assign(tok('total_tokens'), { label: 'Tokens' }),
        { key: 'cost_usd', label: 'Cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
        { key: 'reported_cost_usd', label: 'Reported', num: true, render: (r) => M.fmtCost(r.reported_cost_usd), title: () => 'the harness\'s own session totals, launches started in the range' },
        { key: 'lines_added', label: 'Lines +', num: true },
        { key: 'lines_removed', label: 'Lines −', num: true },
      ],
      rows, sortKey: 'cost_usd', sortDesc: true, empty: 'No harness activity in this range.',
    });
  }

  function cachePanel(c) {
    const t = c.totals || {};
    return M.panel('Cache economics',
      M.cards([
        ['Hit ratio', pct(t.hit_ratio), 'cache read / (input + cache read + write)'],
        ['Cache read', M.fmtTokens(t.cache_read_tokens), M.fmtCost(t.cache_read_cost_usd) + ' paid'],
        ['Saved by reads', M.fmtCost(t.savings_usd), 'vs. the same tokens uncached'],
        ['Write premium', M.fmtCost(t.write_premium_usd), M.fmtTokens(t.cache_write_tokens) + ' written'],
        ['Net savings', M.fmtCost(t.net_savings_usd)],
      ]),
      M.table({
        columns: [
          { key: 'model', label: 'Model' },
          { key: 'hit_ratio', label: 'Hit ratio', num: true, render: (r) => pct(r.hit_ratio) },
          Object.assign(tok('input_tokens'), { label: 'Input' }),
          Object.assign(tok('cache_read_tokens'), { label: 'Cache read' }),
          Object.assign(tok('cache_write_tokens'), { label: 'Cache write' }),
          { key: 'input_rate_per_m', label: 'Input $/M', num: true, render: (r) => (r.input_rate_per_m === null ? '–' : '$' + r.input_rate_per_m.toFixed(2)) },
          { key: 'savings_usd', label: 'Saved', num: true, render: (r) => M.fmtCost(r.savings_usd) },
          { key: 'write_premium_usd', label: 'Write premium', num: true, render: (r) => M.fmtCost(r.write_premium_usd) },
          { key: 'net_savings_usd', label: 'Net', num: true, render: (r) => M.fmtCost(r.net_savings_usd) },
        ],
        rows: c.models || [], sortKey: 'net_savings_usd', sortDesc: true, empty: 'No cached prompts in this range.',
      }),
      h('p', { class: 'muted', style: 'font-size:12px;margin:4px 0 0' },
        'The savings estimate prices each model\'s cache reads at its observed uncached input price (input cost ÷ input tokens) and subtracts what they cost; the write premium is what cache writes cost above that price. A model without priced input has no estimate.'));
  }

  function sessionsTable(s) {
    return M.table({
      columns: [
        { key: 'session_key', label: 'Session', render: (r) => M.link('/sessions/' + encodeURIComponent(r.session_key), clip(r.title || r.session_key, 72)), title: (r) => (r.title ? r.title + '\n' : '') + r.session_key },
        { key: 'provider', label: 'Harness' },
        { key: 'cwd', label: 'Workspace', render: (r) => M.shortPath(r.cwd), title: (r) => r.cwd },
        { key: 'turns', label: 'Turns', num: true },
        { key: 'tools', label: 'Tools', num: true },
        Object.assign(tok('total_tokens'), { label: 'Tokens' }),
        { key: 'cost_usd', label: 'Cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
        { key: 'reported_cost_usd', label: 'Reported', num: true, render: (r) => M.fmtCost(r.reported_cost_usd), title: () => 'the harness\'s lifetime total for the session' },
        { key: 'last_ns', label: 'Last activity', num: true, render: (r) => M.fmtAgo(r.last_ns) },
      ],
      rows: s.sessions || [], sortKey: 'cost_usd', sortDesc: true, empty: 'No sessions in this range.',
    });
  }

  function histogramPanel(title, d, format) {
    const p = d.percentiles || {};
    return M.panel(title,
      charts.bar({ labels: d.histogram.labels, series: [{ name: 'sessions', values: d.histogram.counts }], height: 160 }),
      h('p', { class: 'muted', style: 'font-size:12px;margin:4px 0 0' },
        'p50 ' + format(p.p50) + ' · p95 ' + format(p.p95) + ' · max ' + format(p.max)));
  }

  function distributionsPanels(d) {
    const wrap = h('div');
    wrap.appendChild(M.panel('Session archetypes', M.table({
      columns: [
        { key: 'archetype', label: 'Archetype' },
        { key: 'description', label: 'Rule' },
        { key: 'sessions', label: 'Sessions', num: true },
        { key: 'avg_turns', label: 'Avg turns', num: true, render: (r) => r.avg_turns.toFixed(1) },
        Object.assign(tok('total_tokens'), { label: 'Tokens' }),
        { key: 'cost_usd', label: 'Cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
      ],
      rows: d.archetypes || [], sortable: false,
    })));
    const grid = h('div', { class: 'panels', style: 'margin-top:16px' });
    grid.appendChild(histogramPanel('Duration', d.duration_ms, M.fmtDuration));
    grid.appendChild(histogramPanel('Turns per session', d.turns, (v) => (v === null || v === undefined ? '–' : M.fmtNum(v))));
    grid.appendChild(histogramPanel('Tools per turn', d.tools_per_turn, (v) => (v === null || v === undefined ? '–' : Number(v).toFixed(2))));
    wrap.appendChild(grid);
    return wrap;
  }

  // Weekday × hour as a grid of cells shaded like the calendar heatmap.
  function hourGrid(a) {
    const peak = Math.max(0, ...a.turns.flat());
    const shade = (n) => (n <= 0 || !peak ? 0 : Math.min(4, Math.ceil((n / peak) * 4)));
    const cell = 'display:block;aspect-ratio:1;border-radius:2px;min-width:0';
    const grid = h('div', { class: 'hour-grid', role: 'img', 'aria-label': 'Turns by weekday and hour', style: 'display:grid;grid-template-columns:2.5em repeat(24,1fr);gap:2px;font-size:10px;align-items:center' });
    grid.appendChild(h('span'));
    for (let hr = 0; hr < 24; hr++) grid.appendChild(h('span', { class: 'muted', style: 'text-align:center', text: hr % 3 === 0 ? String(hr) : '' }));
    a.turns.forEach((row, w) => {
      grid.appendChild(h('span', { class: 'muted', text: a.weekdays[w] }));
      row.forEach((n, hr) => {
        grid.appendChild(h('span', {
          style: cell + ';background:var(--heat-' + shade(n) + ')',
          'data-tip': a.weekdays[w] + ' ' + String(hr).padStart(2, '0') + ':00 · ' + M.fmtNum(n) + ' turns · ' + M.fmtCost(a.cost_usd[w][hr]),
        }));
      });
    });
    return h('div', { style: 'overflow-x:auto' }, grid);
  }

  function activityPanels(a) {
    const grid = h('div', { class: 'panels', style: 'margin-top:16px' });
    grid.appendChild(M.panel('Activity by weekday and hour', hourGrid(a)));
    const hours = Array.from({ length: 24 }, (_, i) => String(i).padStart(2, '0'));
    grid.appendChild(M.panel('By hour', charts.bar({
      labels: hours,
      series: [{ name: 'turns', values: a.turns_by_hour }],
      height: 160,
    })));
    grid.appendChild(M.panel('By weekday', charts.bar({
      labels: a.weekdays,
      series: [{ name: 'turns', values: a.turns_by_weekday }],
      height: 160,
    })));
    grid.appendChild(M.panel('Cost by hour', charts.bar({
      labels: hours,
      series: [{ name: 'cost', values: a.cost_by_hour }],
      format: M.fmtCost, height: 160,
    })));
    return grid;
  }

  M.registerPage({
    id: 'usage',
    title: 'Usage',
    path: '/usage',
    group: 'Usage',
    live: true,
    async render(el, params) {
      el.appendChild(M.filterBar({ params }));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(M.loading('Loading usage…'));
      const [daily, br, cache, sess, dist, act] = await Promise.all([
        usage('daily', params),
        usage('breakdown', params),
        usage('cache', params),
        usage('sessions', params, { limit: 50 }),
        usage('distributions', params),
        usage('activity', params),
      ]);
      out.innerHTML = '';
      const t = br.totals;
      out.appendChild(M.cards([
        ['Cost', M.fmtCost(t.cost_usd), t.reported_cost_usd === null ? null : 'reported ' + M.fmtCost(t.reported_cost_usd)],
        ['Tokens', M.fmtTokens(t.total_tokens), 'in ' + M.fmtTokens(t.input_tokens) + ' · out ' + M.fmtTokens(t.output_tokens)],
        ['Cache read / write', M.fmtTokens(t.cache_read_tokens) + ' / ' + M.fmtTokens(t.cache_write_tokens)],
        ['Reasoning', M.fmtTokens(t.reasoning_tokens)],
        ['Sessions', M.fmtNum(t.sessions), M.fmtNum(t.turns) + ' turns'],
        ['Unpriced calls', M.fmtNum(t.unpriced_generations), t.unpriced_generations ? 'no price row, no provider cost' : null],
      ]));

      const labels = daily.days.map(short);
      const grid = h('div', { class: 'panels' });
      grid.appendChild(M.panel('Cost per day by model', charts.bar({ labels, series: daily.cost, stacked: true, format: M.fmtCost })));
      grid.appendChild(M.panel('Tokens per day by model', charts.bar({ labels, series: daily.tokens, stacked: true, format: M.fmtTokens })));
      out.appendChild(grid);

      const sec = (title, node) => { const p = M.panel(title, node); p.style.marginTop = '16px'; out.appendChild(p); };
      sec('Models', modelTable(br.models || []));
      sec('Projects and workspaces', projectTable(br.projects || [], params));
      sec('Harnesses', harnessTable(br.harnesses || []));
      const c = cachePanel(cache);
      c.style.marginTop = '16px';
      out.appendChild(c);
      sec('Sessions by cost' + (sess.total > (sess.sessions || []).length ? ' (top ' + sess.sessions.length + ' of ' + M.fmtNum(sess.total) + ')' : ''), sessionsTable(sess));
      const d = distributionsPanels(dist);
      d.style.marginTop = '16px';
      out.appendChild(d);
      out.appendChild(activityPanels(act));
      out.appendChild(costSourceNote());
    },
  });
})();
