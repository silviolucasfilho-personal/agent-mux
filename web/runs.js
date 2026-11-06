// Runs pages: Runs (loop runs and workflow runs), Loop run, Workflow run
// and Inbox. Every call is GET /api/runs/<…>; the answers are plain JSON
// documents ({ok: true, …}), not the trace envelope.
(function () {
  'use strict';
  const M = window.AgentMux;
  const { h, table, kv, cards, link, loading } = M;

  // GET JSON from `path`; throws like AgentMux.api on a non-2xx status or an
  // {ok: false} body (the shared client only POSTs).
  async function getJson(path) {
    const res = await fetch(path, { headers: { accept: 'application/json' } });
    const text = await res.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch (_) {
      data = null;
    }
    if (!res.ok || (data && data.ok === false)) {
      const msg = data && data.error ? data.error : res.status + ' ' + res.statusText + (text ? ': ' + text.slice(0, 200) : '');
      const err = new Error(msg);
      err.status = res.status;
      err.body = data;
      throw err;
    }
    return data;
  }

  const LOOPS = '/api/runs/loops';
  const WORKFLOWS = '/api/runs/workflows';
  const INBOX = '/api/runs/inbox';

  function loopLink(id, text) {
    return link('/runs/loop/' + encodeURIComponent(id), text || id);
  }

  function workflowLink(id, text) {
    return link('/runs/workflow/' + encodeURIComponent(id), text || id);
  }

  function sessionLink(key, text) {
    if (!key) return h('span', { class: 'muted', text: text || '–' });
    return link('/sessions/' + encodeURIComponent(key), text || key);
  }

  function seconds(s) {
    return s === null || s === undefined ? '–' : M.fmtDuration(Number(s) * 1000);
  }

  // The loop run's status word: the outcome in the reader's words, then
  // the decision once a human made one.
  function loopStatus(r) {
    const cls = r.in_inbox ? 'badge warn' : r.outcome === 'failed' ? 'badge warn' : 'badge';
    const word = r.decision ? r.outcome_word + ' · ' + r.decision : r.outcome_word;
    return h('span', { class: cls, text: word });
  }

  function workflowStatus(r) {
    const cls = r.needs_human ? 'badge warn' : r.status === 'running' ? 'badge working' : 'badge';
    const word = r.awaiting ? r.status + ' · awaiting ' + r.awaiting : r.status;
    return h('span', { class: cls, text: word });
  }

  // The verdict of a loop run: the verifier's word, else the stored one.
  function loopVerdict(r, facts) {
    if (facts && facts.verifier_verdict) return facts.verifier_verdict;
    const v = r.detail && typeof r.detail === 'object' ? r.detail.verifier : null;
    if (typeof v === 'string') return v;
    if (v && typeof v === 'object' && v.verdict) return v.verdict;
    return null;
  }

  function workflowVerdict(r) {
    if (r.awaiting) return r.awaiting;
    const text = r.result_text || '';
    const m = /^\s*([A-Z_]+_VERDICT)\s*:\s*(\S+)/m.exec(text);
    return m ? m[2] : null;
  }

  function section(title, node) {
    return h('section', null, h('h2', { text: title }), node);
  }

  function pre(text) {
    return h('pre', { class: 'block', text: text });
  }

  function fileList(files) {
    if (!files || !files.length) return h('div', { class: 'empty', text: 'No files touched.' });
    return h('ul', { class: 'plain' }, files.map((f) => h('li', { text: f })));
  }

  const loopColumns = [
    { key: 'id', label: 'run', render: (r) => loopLink(r.id), class: 'ellipsis' },
    { key: 'loop_id', label: 'loop', render: (r) => r.loop_id + ' · ' + r.pattern, title: (r) => r.pattern },
    { key: 'workspace', label: 'workspace', render: (r) => M.shortPath(r.workspace), title: (r) => r.workspace, class: 'ellipsis' },
    { key: 'harness', label: 'harness' },
    { key: 'outcome', label: 'status', render: loopStatus },
    { key: 'verdict', label: 'verdict', render: (r) => loopVerdict(r, null) || '–', sort: (r) => loopVerdict(r, null) || '' },
    // a blocked run never started: sort by what the cell shows
    { key: 'started_ns', label: 'started', num: true, render: (r) => M.fmtTime(r.started_ns || r.scheduled_ns), sort: (r) => r.started_ns || r.scheduled_ns },
    { key: 'duration_s', label: 'duration', num: true, render: (r) => seconds(r.duration_s) },
    { key: 'cost_usd', label: 'cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
  ];

  const workflowColumns = [
    { key: 'id', label: 'run', render: (r) => workflowLink(r.id), class: 'ellipsis' },
    { key: 'workflow', label: 'workflow' },
    { key: 'workspace', label: 'workspace', render: (r) => M.shortPath(r.workspace), title: (r) => r.workspace, class: 'ellipsis' },
    { key: 'harness', label: 'harness' },
    { key: 'status', label: 'status', render: workflowStatus },
    { key: 'verdict', label: 'verdict', render: (r) => workflowVerdict(r) || '–', sort: (r) => workflowVerdict(r) || '' },
    { key: 'started_ns', label: 'started', num: true, render: (r) => M.fmtTime(r.started_ns) },
    { key: 'duration_s', label: 'duration', num: true, render: (r) => seconds(r.duration_s) },
    { key: 'cost_usd', label: 'cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
  ];

  // ---- Runs ------------------------------------------------------------------

  M.registerPage({
    id: 'runs',
    title: 'Runs',
    path: '/runs',
    async render(el, params) {
      const limit = Number(params.limit) > 0 ? Number(params.limit) : 100;
      el.appendChild(h('div', { class: 'toolbar' },
        link('/inbox', 'Inbox'),
        h('span', { class: 'muted', text: ' · the ' + limit + ' most recent runs of each kind' }),
      ));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading());
      const [loops, workflows] = await Promise.all([
        getJson(LOOPS + '?limit=' + limit),
        getJson(WORKFLOWS + '?limit=' + limit),
      ]);
      out.innerHTML = '';
      out.appendChild(section('Loop runs', table({
        sortKey: 'started_ns', sortDesc: true,
        empty: 'No loop runs in the store.',
        columns: loopColumns,
        rows: loops.runs || [],
        onRow: (r) => M.navigate('/runs/loop/' + encodeURIComponent(r.id)),
      })));
      out.appendChild(section('Workflow runs', table({
        sortKey: 'started_ns', sortDesc: true,
        empty: 'No workflow runs in the store.',
        columns: workflowColumns,
        rows: workflows.runs || [],
        onRow: (r) => M.navigate('/runs/workflow/' + encodeURIComponent(r.id)),
      })));
    },
  });

  // ---- Loop run ----------------------------------------------------------------

  M.registerPage({
    id: 'runs-loop',
    navId: 'runs',
    nav: false,
    title: (p) => 'Loop run ' + p.id,
    path: '/runs/loop/:id',
    async render(el, params) {
      el.appendChild(h('div', { class: 'toolbar' }, link('/runs', '← Runs')));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading());
      const doc = await getJson(LOOPS + '/' + encodeURIComponent(params.id));
      const r = doc.run;
      const facts = doc.facts;
      const launch = doc.launch;
      out.innerHTML = '';
      out.appendChild(cards([
        ['status', r.decision ? r.outcome_word + ' · ' + r.decision : r.outcome_word],
        ['tokens', M.fmtTokens(r.tokens !== null && r.tokens !== undefined ? r.tokens : facts && facts.tokens)],
        ['cost', M.fmtCost(r.cost_usd !== null && r.cost_usd !== undefined ? r.cost_usd : facts && facts.cost_usd)],
        ['duration', seconds(r.duration_s)],
        ['files touched', String((doc.files_touched || []).length)],
      ]));
      const detail = r.detail && typeof r.detail === 'object' ? r.detail : {};
      out.appendChild(section('Details', kv([
        ['run', r.id],
        ['loop', r.loop_id],
        ['pattern', r.pattern + (r.pattern_hash ? ' (' + String(r.pattern_hash).slice(0, 12) + ')' : '')],
        ['workspace', r.workspace],
        ['harness', r.harness],
        ['level', r.level === r.effective_level ? r.level : r.level + ' → ' + r.effective_level],
        ['outcome', r.outcome + ' (' + r.outcome_word + ')'],
        ['reason', detail.reason || null],
        ['level reason', detail.level_reason || null],
        ['decision', r.decision ? r.decision + ' at ' + M.fmtTime(r.decided_ns) : (r.in_inbox ? 'waiting in the inbox' : '–')],
        ['scheduled', M.fmtTime(r.scheduled_ns)],
        ['started', M.fmtTime(r.started_ns)],
        ['ended', M.fmtTime(r.ended_ns)],
        ['worktree', r.worktree || null],
        ['branch', r.branch || null],
        ['launch', launch ? sessionLink(launch.session_key, launch.session_key || launch.id) : (r.launch_id ? r.launch_id + ' (not in the store)' : null)],
        ['gate violation', detail.gate_violation ? JSON.stringify(detail.gate_violation) : null],
        ['exit', detail.exit_code !== undefined ? String(detail.exit_code) + (detail.timed_out ? ' (timed out)' : '') : null],
      ])));
      const verdict = loopVerdict(r, facts);
      out.appendChild(section('Verdict', kv([
        ['verdict', verdict || (facts && facts.verifier_ran ? 'no verdict line' : 'no verifier ran')],
        ['verifier ran', facts ? (facts.verifier_ran ? 'yes' : 'no') : '–'],
        ['each checker', facts && facts.verifier_verdicts && facts.verifier_verdicts.length ? facts.verifier_verdicts.join(', ') : null],
      ])));
      out.appendChild(section('Budget facts', kv([
        ['items found', r.items_found],
        ['actions taken', r.actions_taken],
        ['escalations', r.escalations],
        ['tokens (recorded)', M.fmtTokens(r.tokens)],
        ['tokens (traced)', facts ? M.fmtTokens(facts.tokens) : '–'],
        ['cost (recorded)', M.fmtCost(r.cost_usd)],
        ['cost (traced)', facts ? M.fmtCost(facts.cost_usd) : '–'],
        ['readiness score', r.readiness_score],
      ])));
      out.appendChild(section('Files touched', fileList(doc.files_touched)));
      if (facts && facts.final_message) out.appendChild(section('Final message', pre(facts.final_message)));
      else if (detail.final_message) out.appendChild(section('Final message', pre(String(detail.final_message))));
      out.appendChild(section('Detail', pre(JSON.stringify(detail, null, 2))));
    },
  });

  // ---- Workflow run ------------------------------------------------------------

  const stepColumns = [
    { key: 'session', label: 'session' },
    { key: 'step_id', label: 'step' },
    { key: 'phase', label: 'phase' },
    { key: 'kind', label: 'kind' },
    { key: 'item', label: 'item', render: (s) => s.item === null || s.item === undefined ? '–' : (typeof s.item === 'string' ? s.item : JSON.stringify(s.item)), class: 'ellipsis' },
    { key: 'harness', label: 'harness' },
    { key: 'status', label: 'status', render: (s) => h('span', { class: s.ended_ns ? 'badge' : 'badge working', text: s.ended_ns ? 'done' : (s.started_ns ? 'running' : 'pending') }), sort: (s) => s.ended_ns ? 2 : (s.started_ns ? 1 : 0) },
    { key: 'session_key', label: 'launch', render: (s) => sessionLink(s.session_key, s.session_key || s.launch_id), class: 'ellipsis' },
    { key: 'duration_s', label: 'duration', num: true, render: (s) => seconds(s.duration_s) },
    { key: 'tokens', label: 'tokens', num: true, render: (s) => M.fmtTokens(s.tokens) },
    { key: 'cost_usd', label: 'cost', num: true, render: (s) => M.fmtCost(s.cost_usd) },
  ];

  M.registerPage({
    id: 'runs-workflow',
    navId: 'runs',
    nav: false,
    title: (p) => 'Workflow run ' + p.id,
    path: '/runs/workflow/:id',
    async render(el, params) {
      el.appendChild(h('div', { class: 'toolbar' }, link('/runs', '← Runs')));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading());
      const doc = await getJson(WORKFLOWS + '/' + encodeURIComponent(params.id));
      const r = doc.run;
      const steps = doc.steps || [];
      out.innerHTML = '';
      out.appendChild(cards([
        ['status', r.awaiting ? r.status + ' · awaiting ' + r.awaiting : r.status],
        ['sessions', String(r.sessions)],
        ['tokens', M.fmtTokens(r.tokens)],
        ['cost', M.fmtCost(r.cost_usd)],
        ['duration', seconds(r.duration_s)],
      ]));
      const args = r.args && typeof r.args === 'object' ? r.args : {};
      out.appendChild(section('Run', kv([
        ['document', r.workflow + ' (' + r.source + ')'],
        ['run', r.id],
        ['workspace', r.workspace],
        ['harness', r.harness + (r.profile ? ' · ' + r.profile : '')],
        ['args', Object.keys(args).length ? Object.entries(args).map(([k, v]) => k + ' = ' + (typeof v === 'string' ? v : JSON.stringify(v))).join('\n') : (r.args === null || r.args === undefined ? null : JSON.stringify(r.args))],
        ['status', workflowStatus(r)],
        ['verdict', workflowVerdict(r)],
        ['error', r.error],
        ['budget', r.budget_tokens ? M.fmtTokens(r.budget_tokens) + ' tokens' : null],
        ['started', M.fmtTime(r.started_ns)],
        ['ended', M.fmtTime(r.ended_ns)],
        ['resumed from', r.resumed_from],
        ['document hash', r.document_hash ? String(r.document_hash).slice(0, 12) : null],
      ])));
      const stepsOut = h('div');
      stepsOut.appendChild(table({
        empty: 'No sessions recorded for this run.',
        sortable: false,
        columns: stepColumns,
        rows: steps,
      }));
      for (const s of steps) {
        if (s.result_text) {
          stepsOut.appendChild(h('details', null,
            h('summary', { text: s.session + ' result' }),
            pre(s.result_text),
          ));
        }
        if (s.changed_files && Array.isArray(s.changed_files) && s.changed_files.length) {
          stepsOut.appendChild(h('details', null,
            h('summary', { text: s.session + ' changed files' }),
            fileList(s.changed_files),
          ));
        }
      }
      out.appendChild(section('Steps', stepsOut));
      out.appendChild(section('Result', r.result_text ? pre(r.result_text) : h('div', { class: 'empty', text: 'No result recorded.' })));
      if (r.document) {
        out.appendChild(section('Document', h('details', null, h('summary', { text: 'the TOML this run executed' }), pre(r.document))));
      }
    },
  });

  // ---- Inbox -------------------------------------------------------------------

  M.registerPage({
    id: 'inbox',
    title: 'Inbox',
    path: '/inbox',
    async render(el) {
      el.appendChild(h('div', { class: 'muted', text: 'Everything waiting on a human, as the TUI inbox (I) shows it. Decide in the TUI or with agent-mux loop decide; this page only reads.' }));
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading());
      const inbox = await getJson(INBOX);
      const loops = inbox.loops || [];
      const workflows = inbox.workflows || [];
      out.innerHTML = '';
      out.appendChild(cards([
        ['loop runs', String(loops.length)],
        ['workflow runs', String(workflows.length)],
      ]));
      out.appendChild(section('Loops', table({
        empty: 'No loop run waits on you.',
        columns: [
          { key: 'id', label: 'run', render: (r) => loopLink(r.id), class: 'ellipsis' },
          { key: 'loop_id', label: 'loop', render: (r) => r.loop_id + ' · ' + r.pattern },
          { key: 'workspace', label: 'workspace', render: (r) => M.shortPath(r.workspace), title: (r) => r.workspace, class: 'ellipsis' },
          { key: 'outcome_word', label: 'wants', render: (r) => h('span', { class: 'badge warn', text: r.outcome_word }) },
          { key: 'branch', label: 'branch', render: (r) => r.branch || '–' },
          { key: 'started_ns', label: 'started', num: true, render: (r) => M.fmtTime(r.started_ns || r.scheduled_ns), sort: (r) => r.started_ns || r.scheduled_ns },
          { key: 'cost_usd', label: 'cost', num: true, render: (r) => M.fmtCost(r.cost_usd) },
        ],
        rows: loops,
        onRow: (r) => M.navigate('/runs/loop/' + encodeURIComponent(r.id)),
      })));
      out.appendChild(section('Workflows', table({
        empty: 'No workflow run waits on you.',
        columns: [
          { key: 'id', label: 'run', render: (r) => workflowLink(r.id), class: 'ellipsis' },
          { key: 'workflow', label: 'workflow' },
          { key: 'workspace', label: 'workspace', render: (r) => M.shortPath(r.workspace), title: (r) => r.workspace, class: 'ellipsis' },
          { key: 'wants', label: 'wants', render: (r) => h('span', { class: 'badge warn', text: r.awaiting ? 'verdict to read: ' + r.awaiting : 'did not finish' }), sort: (r) => r.awaiting || '' },
          { key: 'status', label: 'status' },
          { key: 'error', label: 'error', render: (r) => r.error || '–', class: 'ellipsis', title: (r) => r.error || '' },
          { key: 'started_ns', label: 'started', num: true, render: (r) => M.fmtTime(r.started_ns) },
        ],
        rows: workflows,
        onRow: (r) => M.navigate('/runs/workflow/' + encodeURIComponent(r.id)),
      })));
    },
  });
})();
