// Read-only swarms and inter-agent bus.
(function () {
  'use strict';
  const A = window.AgentMux;
  const { h, api, table, link, fmtTime, fmtCost } = A;
  const query = (ws) => '?workspace=' + encodeURIComponent(ws);
  const pre = (lines) => h('pre', { text: lines.join('\n') });
  const columns = (keys) => keys.map((key) => ({ key, label: key.replaceAll('_', ' '),
    render: (row) => typeof row[key] === 'object' ? JSON.stringify(row[key]) : (row[key] ?? '—') }));
  const taskTable = (rows) => table({ columns: columns(['title', 'status', 'assigned_to', 'branch', 'result', 'error']), rows });
  const messageTable = (rows) => table({ columns: [
    { key: 'created_ns', label: 'Time', render: (r) => fmtTime(r.created_ns) },
    ...columns(['from_launch', 'to_launch', 'topic', 'body'])], rows });
  const peerTable = (rows) => table({ columns: columns(['launch_id', 'provider', 'state', 'workspace']), rows });

  A.registerPage({ id: 'swarms', title: 'Swarms', path: '/swarms', async render(el, p) {
    const data = await api('/api/swarm/list' + (p.since ? '?since=' + encodeURIComponent(p.since) : ''));
    el.appendChild(table({ columns: [
      { key: 'id', label: 'ID', render: (r) => link('/swarms/' + encodeURIComponent(r.swarm.id), r.swarm.id) },
      { key: 'goal', label: 'Goal', render: (r) => r.swarm.goal },
      { key: 'status', label: 'Status', render: (r) => r.swarm.status },
      { key: 'started', label: 'Started', render: (r) => fmtTime(r.swarm.started_ns) },
      ...columns(['workers']),
      { key: 'cost_usd', label: 'Cost', render: (r) => fmtCost(r.cost_usd) }
    ], rows: data.swarms }));
  } });

  A.registerPage({ id: 'swarm', nav: false, navId: 'swarms', title: 'Swarm', path: '/swarms/:id',
    async render(el, p) {
      const root = '/api/swarm/' + encodeURIComponent(p.id);
      const [data, board] = await Promise.all([api(root), api(root + '/tasks')]);
      const r = data.report;
      const peers = await api('/api/bus/peers' + query(r.swarm.workspace) + '&all=1');
      el.append(h('p', { text: r.swarm.goal }), A.kv([
        ['Status', r.swarm.status], ['Workspace', r.swarm.workspace],
        ['Tasks done', r.done + '/' + r.total], ['Cost', fmtCost(r.cost_usd)]
      ]), link('/bus', 'Workspace bus', { workspace: r.swarm.workspace }));
      const content = h('section');
      const tabs = h('div', { class: 'toolbar', role: 'tablist' });
      const views = {
        Board: () => h('div', {}, pre(r.attention), taskTable(board.tasks.map((b) => b.task)),
          h('h3', { text: 'Peers' }), peerTable(peers.peers)),
        Tree: () => pre(data.tree_lines),
        Timeline: () => table({ columns: [
          ...columns(['label', 'state']),
          { key: 'start_ns', label: 'Started', render: (l) => fmtTime(l.start_ns) },
          { key: 'end_ns', label: 'Ended', render: (l) => l.end_ns ? fmtTime(l.end_ns) : 'Open' },
          { key: 'working', label: 'Working spans', render: (l) => spans(l.working) },
          { key: 'asking', label: 'Question spans', render: (l) => spans(l.asking) },
          { key: 'marks', label: 'Events', render: (l) => l.marks.map((m) => fmtTime(m.ns) + ' ' + m.glyph + ' ' + m.label).join('; ') }
        ], rows: data.timeline.lanes }),
        Log: () => pre(data.log),
        Messages: () => messageTable(r.messages.map((m) => ({ ...m, from_launch: m.from, to_launch: m.to })))
      };
      function spans(items) { return items.map(([a, b]) => fmtTime(a) + ' → ' + (b ? fmtTime(b) : 'Open')).join('; '); }
      function show(name) {
        content.replaceChildren(views[name]());
        for (const button of tabs.children) button.setAttribute('aria-selected', String(button.textContent === name));
      }
      for (const name of Object.keys(views)) tabs.appendChild(h('button', { role: 'tab', onclick: () => show(name), text: name }));
      el.append(tabs, content);
      show('Board');
    }
  });

  A.registerPage({ id: 'bus', title: 'Bus', path: '/bus', async render(el, p) {
    const input = h('input', { name: 'workspace', placeholder: 'Workspace path', value: p.workspace || '', required: true });
    el.appendChild(h('form', { class: 'toolbar', onsubmit: (e) => {
      e.preventDefault(); A.navigate('/bus', { workspace: input.value });
    } }, h('label', {}, 'Workspace ', input), h('button', { type: 'submit', text: 'Load' })));
    if (!p.workspace) { el.appendChild(h('p', { text: 'Choose a workspace to read its bus.' })); return; }
    const q = query(p.workspace);
    const [peers, tasks, messages] = await Promise.all([
      api('/api/bus/peers' + q + '&all=1'), api('/api/bus/tasks' + q), api('/api/bus/messages' + q)
    ]);
    el.append(h('h2', { text: 'Peers' }), peerTable(peers.peers),
      h('h2', { text: 'Tasks' }), taskTable(tasks.tasks),
      h('h2', { text: 'Messages' }), messageTable(messages.messages));
  } });
})();
