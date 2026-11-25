// Projects: the work items of a spec-wave repository, as this machine last
// read them. Every page is answered from the cache: nothing here asks
// GitHub, and nothing writes.
(function () {
  'use strict';
  const A = window.AgentMux;
  const { h, api, table, link, kv, cards, panel, empty, fmtAgo, fmtTime, fmtCost, fmtTokens } = A;

  const enc = encodeURIComponent;
  const hubQuery = (p) => '?hub=' + enc(p.owner + '/' + p.repo);
  const hubPath = (hub) => '/projects/' + hub.split('/').map(enc).join('/');
  const itemPath = (hub, n) => hubPath(hub) + '/items/' + n;
  const issueUrl = (hub, n) => 'https://github.com/' + hub + '/issues/' + n;
  const pre = (lines) => h('pre', { text: (lines || []).join('\n') });
  const external = (url, text) => h('a', { href: url, target: '_blank', rel: 'noreferrer noopener' }, text);
  const badge = (text, cls) => h('span', { class: 'badge' + (cls ? ' ' + cls : ''), text });

  // The tabs of a page, kept in the `view` query key so a tab has a link
  // and a live refresh stays on it.
  function tabs(names, current) {
    const bar = h('div', { class: 'toolbar', role: 'tablist' });
    for (const name of names) {
      bar.appendChild(h('button', {
        role: 'tab', type: 'button', 'aria-selected': String(name === current),
        onclick: () => A.setQuery({ view: name === names[0] ? null : name }), text: name,
      }));
    }
    return bar;
  }

  function readNote(data) {
    const bits = ['read from GitHub ' + fmtAgo(data.synced_ns) + ' (' + fmtTime(data.synced_ns) + ')'];
    if (data.board_error) bits.push('the board was not read: ' + data.board_error);
    return h('div', { class: 'muted note', text: bits.join(' · ') + ' · `agent-mux project sync` reads it again' });
  }

  function pulls(row) {
    if (!row.pulls || !row.pulls.length) return '–';
    const box = h('span');
    row.pulls.forEach((p, i) => {
      if (i) box.appendChild(document.createTextNode(' '));
      const state = p.state === 'OPEN' ? (p.draft ? 'draft' : 'open') : p.state.toLowerCase();
      box.appendChild(external(p.url, p.repo.split('/').pop() + '#' + p.number + ' ' + state));
    });
    return box;
  }

  function nextCell(row) {
    const blocked = row.next && row.next.blocked;
    return h('span', { class: blocked ? 'warn' : null, text: row.next_line || '–' });
  }

  function sessionsCell(totals, n) {
    const t = totals && totals[n];
    if (!t || !t.rolled.sessions) return '–';
    return t.rolled.sessions + ' · ' + fmtCost(t.rolled.cost_usd) + (t.rolled.unpriced ? '+' : '');
  }

  function itemColumns(hub, totals, opts) {
    const cols = [
      { key: 'number', label: '#', num: true, render: (r) => link(itemPath(hub, r.number), '#' + r.number) },
      { key: 'type', label: 'Type' },
      { key: 'title', label: 'Title', class: 'wrap', render: (r) => link(itemPath(hub, r.number), r.title) },
    ];
    if (!opts || opts.stage !== false) cols.push({ key: 'stage', label: 'Stage', render: (r) => r.stage + (r.stage_derived ? ' (derived)' : '') });
    cols.push(
      { key: 'priority', label: 'Priority' },
      { key: 'milestone', label: 'Milestone' },
      { key: 'pulls', label: 'Pull requests', sortable: false, render: pulls },
      { key: 'next_line', label: 'Next step', class: 'wrap', render: nextCell },
    );
    if (totals) cols.push({ key: 'sessions', label: 'Sessions', sort: (r) => (totals[r.number] ? totals[r.number].rolled.cost_usd : 0), render: (r) => sessionsCell(totals, r.number) });
    return cols;
  }

  // ---- the list of Projects ------------------------------------------------

  A.registerPage({ id: 'projects', title: 'Projects', path: '/projects', group: 'Projects', key: 'p', live: true,
    async render(el) {
      const data = await api('/api/project/list');
      if (!data.projects.length) {
        el.appendChild(empty('No Project was read on this machine.',
          'In a repository configured with spec-wave, run `agent-mux project sync` or open it in the TUI (F1 → Open project).'));
        return;
      }
      el.appendChild(table({ columns: [
        { key: 'hub', label: 'Project', render: (r) => link(hubPath(r.hub), r.hub) },
        { key: 'open', label: 'Open items', num: true },
        { key: 'needs_you', label: 'Wait on a person', num: true },
        { key: 'milestones', label: 'Open milestones', num: true },
        { key: 'synced_ns', label: 'Read', render: (r) => fmtAgo(r.synced_ns) },
        { key: 'root', label: 'Folder', render: (r) => (r.root ? A.shortPath(r.root) : '–') },
      ], rows: data.projects, onRow: (r) => A.navigate(hubPath(r.hub)) }));
    } });

  // ---- one Project -----------------------------------------------------------

  const VIEWS = ['Board', 'Needs you', 'Milestones', 'Fleet', 'Activity'];
  const DOCS = [['spec', 'spec'], ['plan', 'plan'], ['decomposition', 'decomposition'], ['qa-plan', 'QA plan']];
  const DOC_LABEL = { base: '✓ merged', pr: '◐ in review', branch: '◐ on its branch', missing: '· not written', unknown: '? not read' };

  function boardView(data, p) {
    const box = h('div');
    const types = Array.from(new Set(data.items.map((i) => i.type).filter(Boolean))).sort();
    const bar = h('div', { class: 'toolbar' }, h('span', { class: 'muted', text: 'Type' }));
    for (const t of [''].concat(types)) {
      bar.appendChild(h('button', {
        type: 'button', 'aria-pressed': String((p.type || '') === t),
        onclick: () => A.setQuery({ type: t || null }), text: t || 'All',
      }));
    }
    box.appendChild(bar);
    const rows = data.items.filter((i) => !p.type || i.type === p.type);
    let shown = 0;
    for (const stage of data.stages) {
      const at = rows.filter((i) => i.stage === stage);
      if (!at.length) continue;
      shown += at.length;
      box.appendChild(panel(stage + ' (' + at.length + ')',
        table({ columns: itemColumns(data.hub, data.totals, { stage: false }), rows: at })));
    }
    if (!shown) box.appendChild(empty('No open item' + (p.type ? ' of type ' + p.type : '') + '.'));
    return box;
  }

  function needsView(data) {
    const rows = data.items.filter((i) => i.needs_operator && ['Feature', 'Bug', 'RFC'].includes(i.type) && i.next.action !== 'implement');
    return table({ columns: itemColumns(data.hub, null), rows,
      empty: 'Nothing waits on a person.', emptyHint: 'A document to validate, a critique that failed or a merge that is pending shows here.' });
  }

  function milestonesView(data) {
    const box = h('div');
    const open = data.milestones.filter((m) => m.open);
    if (!open.length) return empty('No open milestone.');
    for (const m of open) {
      const total = m.open_issues + m.closed_issues;
      const head = m.closed_issues + ' of ' + total + ' issues closed' + (m.due ? ' · due ' + String(m.due).slice(0, 10) : '');
      const cols = [
        { key: 'number', label: '#', num: true, render: (f) => link(itemPath(data.hub, f.number), '#' + f.number) },
        { key: 'title', label: 'Feature', render: (f) => link(itemPath(data.hub, f.number), f.title) },
        { key: 'stage', label: 'Stage', render: (f) => (f.open ? f.stage : 'closed') },
      ].concat(DOCS.map(([key, label]) => ({ key, label, sortable: false, render: (f) => DOC_LABEL[f.docs[key]] || f.docs[key] })));
      box.appendChild(panel(m.title, h('div', { class: 'muted note', text: head }),
        table({ columns: cols, rows: m.features, empty: 'No Feature in this milestone.' })));
    }
    return box;
  }

  function fleetView(data) {
    const title = (n) => {
      const it = data.items.find((i) => i.number === n);
      return it ? it.title : '';
    };
    return table({ columns: [
      { key: 'item', label: '#', num: true, render: (d) => link(itemPath(data.hub, d.item), '#' + d.item) },
      { key: 'title', label: 'Title', render: (d) => title(d.item) },
      { key: 'state', label: 'State', render: (d) => badge(d.state, d.state === 'blocked' ? 'error' : d.state === 'running' ? 'working' : '') },
      { key: 'target', label: 'For' },
      { key: 'harness', label: 'Harness' },
      { key: 'owner', label: 'Held by' },
      { key: 'queued_ns', label: 'Queued', render: (d) => fmtAgo(d.queued_ns) },
      { key: 'failures', label: 'Failures', num: true },
      { key: 'reason', label: 'Reason' },
    ], rows: data.fleet, empty: 'Nothing is dispatched.', emptyHint: '`agent-mux project dispatch <n>` puts an item in the fleet\'s queue.' });
  }

  async function activityView(p) {
    const data = await api('/api/project/activity' + hubQuery(p));
    if (!data.sessions) return empty('No step of this Project has run a session on this machine.');
    return h('div', { class: 'panels' }, panel('The last 30 days', pre(data.recent)), panel('Ever', pre(data.ever)));
  }

  A.registerPage({ id: 'project', nav: false, navId: 'projects', group: 'Projects', live: true,
    title: (p) => (p.owner ? p.owner + '/' + p.repo : 'Project'), path: '/projects/:owner/:repo',
    async render(el, p) {
      const data = await api('/api/project/board' + hubQuery(p));
      const view = VIEWS.includes(p.view) ? p.view : VIEWS[0];
      const needs = data.items.filter((i) => i.needs_operator && ['Feature', 'Bug', 'RFC'].includes(i.type) && i.next.action !== 'implement').length;
      el.append(
        cards([
          ['Open items', data.items.length],
          ['Wait on a person', needs],
          ['Open milestones', data.milestones.filter((m) => m.open).length],
          ['Dispatched', data.fleet.length],
        ]),
        readNote(data));
      if (data.warnings.length) {
        el.appendChild(panel('What could not be read', pre(data.warnings)));
      }
      el.appendChild(tabs(VIEWS, view));
      const body = view === 'Needs you' ? needsView(data)
        : view === 'Milestones' ? milestonesView(data)
        : view === 'Fleet' ? fleetView(data)
        : view === 'Activity' ? await activityView(p)
        : boardView(data, p);
      el.appendChild(body);
    } });

  // ---- one item --------------------------------------------------------------

  function documentsView(it) {
    const rows = Object.entries(it.docs || {}).map(([name, d]) => ({ name, ...d }));
    return table({ columns: [
      { key: 'name', label: 'Document' },
      { key: 'state', label: 'Where', render: (d) => DOC_LABEL[d.state] || d.state },
      { key: 'path', label: 'Path' },
      { key: 'branch', label: 'Branch' },
      { key: 'pr', label: 'Pull request', render: (d) => (d.pr ? '#' + d.pr : '–') },
    ], rows, empty: 'This item has no document.' });
  }

  function pullsView(it) {
    return table({ columns: [
      { key: 'number', label: 'Pull request', render: (p) => external(p.url, p.repo + '#' + p.number) },
      { key: 'state', label: 'State', render: (p) => (p.state === 'OPEN' && p.draft ? 'DRAFT' : p.state) },
      { key: 'review', label: 'Review', render: (p) => p.review || '–' },
      { key: 'head', label: 'Branch' },
      { key: 'base', label: 'Into' },
    ], rows: it.pulls || [], empty: 'No pull request is linked to this item.' });
  }

  function traceView(data) {
    const r = data.report;
    if (!r.launches.length) {
      return empty('No step of this item has run a session on this machine.');
    }
    const session = (l) => {
      const label = l.provider + ' · ' + l.project.role;
      if (l.session_key) return link('/sessions/' + enc(l.session_key), label);
      return h('a', { href: '#', onclick: (e) => { e.preventDefault(); A.openSession(l.launch_id); } }, label);
    };
    const runs = table({ columns: [
      { key: 'item', label: '#', num: true, render: (x) => link(itemPath(data.hub, x.item), '#' + x.item) },
      { key: 'step', label: 'Step' },
      { key: 'attempt', label: 'Attempt', num: true },
      { key: 'started_ns', label: 'Started', render: (x) => fmtTime(x.started_ns) },
      { key: 'ended_ns', label: 'Ended', render: (x) => (x.ended_ns ? fmtTime(x.ended_ns) : 'going') },
      { key: 'sessions', label: 'Sessions', sortable: false, render: (x) => {
        const box = h('span');
        x.sessions.forEach((at, i) => {
          if (i) box.appendChild(document.createTextNode(', '));
          box.appendChild(session(r.launches[at]));
        });
        return box;
      } },
      { key: 'tokens', label: 'Tokens', num: true, render: (x) => fmtTokens(x.tokens) },
      { key: 'cost_usd', label: 'Cost', num: true, render: (x) => fmtCost(x.cost_usd) + (x.unpriced ? '+' : '') },
      { key: 'run', label: 'Run', render: (x) => (x.run ? link('/runs/workflow/' + enc(x.run), 'open') : '–') },
    ], rows: data.runs });
    return h('div', {}, runs, panel('As a tree', pre(data.tree_lines)));
  }

  A.registerPage({ id: 'project-item', nav: false, navId: 'projects', group: 'Projects', live: true,
    title: (p) => (p.n ? '#' + p.n : 'Item'), path: '/projects/:owner/:repo/items/:n',
    async render(el, p) {
      const data = await api('/api/project/item' + hubQuery(p) + '&n=' + enc(p.n));
      const it = data.item;
      const crumbs = h('div', { class: 'muted note' }, link(hubPath(data.hub), data.hub));
      for (const a of data.ancestors) {
        crumbs.append(' › ', link(itemPath(data.hub, a.number), (a.type || 'Item') + ' #' + a.number + ' ' + a.title));
      }
      const names = ['Card', 'Children', 'Documents', 'Pull requests', 'Trace'];
      if (data.order) names.splice(2, 0, 'Order');
      const view = names.includes(p.view) ? p.view : names[0];
      el.append(crumbs,
        h('h2', { text: (it.item_type || 'Item') + ' #' + it.number + ' ' + data.title }),
        kv([
          ['Stage', it.shown_stage + (it.stage_derived ? ' (derived: the issue is not on the board)' : '')],
          ['State', it.open ? 'open' : 'closed'],
          ['Next step', h('span', { class: it.next.blocked ? 'warn' : null, text: it.next.action === 'none' ? it.next.reason : it.next.action + ': ' + it.next.reason })],
          ['Unblock', it.next.blocked ? it.next.blocked.unblock : undefined],
          ['Milestone', it.milestone || undefined],
          ['Priority', it.priority || undefined],
          ['Fleet', data.dispatch ? data.dispatch.state + (data.dispatch.reason ? ': ' + data.dispatch.reason : '') : undefined],
          ['Issue', external(issueUrl(data.hub, it.number), 'open on GitHub')],
        ]),
        h('div', { class: 'muted note', text: 'read from GitHub ' + fmtAgo(data.synced_ns) }),
        tabs(names, view));
      const body = view === 'Children' ? table({ columns: itemColumns(data.hub, null), rows: data.children, empty: 'Nothing is under this item.' })
        : view === 'Order' ? pre(data.order.lines)
        : view === 'Documents' ? documentsView(it)
        : view === 'Pull requests' ? pullsView(it)
        : view === 'Trace' ? traceView(data)
        : pre(data.card.concat(data.sessions_line ? ['', data.sessions_line] : []));
      el.appendChild(body);
    } });
})();
