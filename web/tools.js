// Read-only tool and edit activity; shared date/harness/workspace filters.
(function () {
  'use strict';
  const M = window.AgentMux, h = M.h;
  const turn = c => M.link('/sessions/' + encodeURIComponent(c.session_key) + '?turn=' + encodeURIComponent(c.turn_id), c.session_key + ' · ' + c.turn_id);
  const metricColumns = [
    { key: 'name', label: 'Name' }, { key: 'count', label: 'Calls', num: true },
    { key: 'error_rate', label: 'Errors', render: r => M.fmtPercent(r.error_rate) },
    { key: 'p50_ms', label: 'p50', render: r => M.fmtDuration(r.p50_ms) },
    { key: 'p95_ms', label: 'p95', render: r => M.fmtDuration(r.p95_ms) },
    { key: 'duration_ms', label: 'Total duration', render: r => M.fmtDuration(r.duration_ms) },
    { key: 'cost_usd', label: 'Recorded cost', render: r => M.fmtCost(r.cost_usd) },
  ];
  const table = rows => M.table({ columns: metricColumns, rows, empty: 'No activity in this range.' });
  const heading = (el, name, node) => { el.append(h('h2', null, name), node); };
  function bars(rows) { return M.charts.hbars({ items: rows.map(r => ({ label: r.name, value: r.count })) }); }
  function daily(rows, metric, format) {
    rows = rows.filter(r => r[metric] !== null).sort((a,b) => Number(a.name)-Number(b.name));
    return M.charts.bar({ format, labels: rows.map(r => new Date(Number(r.name)/1e6).toISOString().slice(0,10)), series: [{ name: metric, values: rows.map(r => r[metric] || 0) }] });
  }
  async function fetchArea(area, params) {
    const f = M.filters(params);
    return (await M.api('/api/tools/' + area, { since: f.since, until: f.until, provider: f.provider, workspace: f.workspace })).data;
  }
  function register(id, title, render) {
    M.registerPage({ id, title, path: '/' + id, group: 'Tools', live: true,
      async render(el, params) {
        const filters = h('div');
        filters.appendChild(M.filterBar({ params }));
        el.appendChild(filters);
        // Suggestions retain the date/harness scope but ignore the typed fragment.
        fetchArea('workspaces', params).then(data => {
          filters.replaceChildren(M.filterBar({ params, workspaces: data.workspaces }));
        }).catch(() => {});
        const out = h('div'); el.appendChild(out); out.appendChild(M.loading());
        try { await render(out, params); } catch (e) { out.replaceChildren(M.errorBox(e)); }
      } });
  }
  register('tools', 'Tools', async (el, params) => {
    const [s, f, q] = await Promise.all(['summary','failures','sequences'].map(a => fetchArea(a, params)));
    el.replaceChildren();
    const days = [...new Set(s.mix.map(r => r.name.split('|')[0]))].sort((a,b) => Number(a)-Number(b));
    const names = [...new Set(s.tools.map(r => r.name))];
    const mix = new Map(s.mix.map(r => [r.name, r.count]));
    heading(el, 'Tool mix over time', M.charts.bar({ stacked: true, labels: days.map(d => new Date(Number(d)/1e6).toISOString().slice(0,10)), series: names.map(name => ({ name, values: days.map(d => mix.get(d+'|'+name)||0) })) }));
    for (const [key,label] of [['provider_mix','By harness'],['model_mix','By model']]) {
      const groups = [...new Set(s[key].map(r => r.name.split('|')[0]))];
      const counts = new Map(s[key].map(r => [r.name,r.count]));
      heading(el, label, M.charts.bar({ stacked: true, labels: groups, series: names.map(name => ({ name, values: groups.map(g => counts.get(g+'|'+name)||0) })) }));
    }
    heading(el, 'Per tool', table(s.tools)); heading(el, 'MCP servers', table(s.mcp_servers));
    heading(el, 'Failing calls (latest 500 of ' + f.total + ')', M.table({ rows: f.calls, columns: [
      { key: 'tool', label: 'Tool' }, { key: 'status_message', label: 'Error' },
      { key: 'start_ns', label: 'When', render: c => M.fmtTime(new Date(c.start_ns/1e6).toISOString()) },
      { key: 'session_key', label: 'Turn', render: turn },
    ] }));
    for (const [key,label] of [['retries','Retries'],['recoveries','Error → recovery']]) {
      heading(el, label + ' (adjacent calls within a session)', M.table({ rows: q[key], columns: [
        { key: 'from', label: 'Failed tool', render: r => r.from.tool },
        { key: 'to', label: 'Next tool', render: r => r.to.tool + (r.to.is_error ? ' · error' : ' · success') },
        { key: 'session_key', label: 'Next turn', render: r => turn(r.to) },
      ] }));
    }
    heading(el, 'Frequent tool switches', bars(q.switches));
  });
  register('tool-activity', 'Agents & skills activity', async (el, params) => {
    const s = await fetchArea('activity', params); el.replaceChildren();
    el.appendChild(M.link('/skills', 'Open Skills & Agents analysis →'));
    for (const [key,label] of [['agents','Subagents'],['skills','Skills']]) {
      heading(el, label, table(s[key]));
      const rows = s[key === 'agents' ? 'agent_days' : 'skill_days'];
      for (const [metric,title,format] of [['count','Invocations',M.fmtNum],['cost_usd','Cost',M.fmtCost],['duration_ms','Duration',M.fmtDuration]]) heading(el, label + ' · ' + title, daily(rows, metric, format));
    }
    el.appendChild(h('p', { class: 'muted' }, 'Recorded costs include direct child observations for agents and skills; missing costs are unknown. Durations may overlap.'));
  });
  register('edits', 'Recent edits', async (el, params) => {
    const s = await fetchArea('edits', params); el.replaceChildren();
    el.appendChild(h('p', null, 'Latest 500 of ' + s.total + ' project/path groups. Use the workspace filter to select a project.'));
    heading(el, 'Recent successful edits', M.table({ rows: s.edits, columns: [
      { key: 'project', label: 'Project' }, { key: 'path', label: 'Path' }, { key: 'count', label: 'Edits', num: true },
      { key: 'start_ns', label: 'Latest', render: c => M.fmtTime(new Date(c.start_ns/1e6).toISOString()) },
      { key: 'session_key', label: 'Turn', render: turn },
    ] }));
  });
})();
