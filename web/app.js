// agent-mux web: the shell. Hash router, grouped nav, page registry, API
// client, formatting and DOM helpers, themes, the shared filter bar, SVG
// charts (M.charts), the live channel (M.live), the command palette and
// keyboard shortcuts. The Overview page lives in usage.js. Every script registers
// its pages with AgentMux.registerPage and uses these helpers; nothing here
// is specific to one area. No dependencies, no build step.
(function () {
  'use strict';

  const pages = [];

  // Nav groups in display order; a group with no page is not drawn.
  const GROUPS = ['Overview', 'Sessions', 'Usage', 'Tools', 'Swarms/Runs', 'Library', 'Health'];
  // Pages registered without a `group` land in their area's group.
  const DEFAULT_GROUP = {
    overview: 'Overview',
    sessions: 'Sessions', search: 'Sessions',
    usage: 'Usage',
    tools: 'Tools', skills: 'Tools',
    swarms: 'Swarms/Runs', bus: 'Swarms/Runs', runs: 'Swarms/Runs', inbox: 'Swarms/Runs',
    library: 'Library', agents: 'Library',
    health: 'Health',
  };
  // `g` + this key jumps to the page (a page may set its own `key`).
  const DEFAULT_KEY = {
    overview: 'o', sessions: 's', search: 'f', usage: 'u', tools: 't', skills: 'k',
    swarms: 'w', bus: 'b', runs: 'r', inbox: 'i', library: 'l', agents: 'a', health: 'h',
  };

  // ---- page registry -----------------------------------------------------

  // registerPage({id, title, path, render(el, params), nav, navId, group, key, live})
  //   path: '/sessions' or '/sessions/:key' (one segment per ':name');
  //   nav: false keeps the page out of the left nav (detail pages);
  //   group: the nav group (GROUPS; defaults by id, else 'Tools');
  //   key: the letter after `g` that jumps to it;
  //   live: true re-renders the page when the store changes (M.live).
  function registerPage(page) {
    if (!page || !page.id || !page.path || typeof page.render !== 'function') {
      throw new Error('registerPage needs id, path and render');
    }
    page.segments = page.path.replace(/^\//, '').split('/');
    if (!page.group) page.group = DEFAULT_GROUP[page.navId || page.id] || 'Tools';
    if (!page.key && DEFAULT_KEY[page.id] && !pages.some((p) => p.key === DEFAULT_KEY[page.id])) page.key = DEFAULT_KEY[page.id];
    pages.push(page);
    if (document.readyState !== 'loading') renderNav();
    return page;
  }

  function matchPage(path) {
    const parts = path.replace(/^\//, '').split('/');
    for (const page of pages) {
      if (page.segments.length !== parts.length) continue;
      const params = {};
      let ok = true;
      for (let i = 0; i < parts.length; i++) {
        const seg = page.segments[i];
        if (seg.startsWith(':')) {
          params[seg.slice(1)] = decodeURIComponent(parts[i]);
        } else if (seg !== parts[i]) {
          ok = false;
          break;
        }
      }
      if (ok) return { page, params };
    }
    return null;
  }

  function navPages() {
    return pages.filter((p) => p.nav !== false);
  }

  function pageTitle(page, params) {
    return typeof page.title === 'function' ? page.title(params || {}) : page.title;
  }

  // ---- routing -----------------------------------------------------------

  function currentRoute() {
    const hash = location.hash.replace(/^#/, '');
    const q = hash.indexOf('?');
    const path = q >= 0 ? hash.slice(0, q) : hash;
    const query = {};
    if (q >= 0) {
      for (const [k, v] of new URLSearchParams(hash.slice(q + 1))) query[k] = v;
    }
    return { path: path || null, query };
  }

  function hrefFor(path, query) {
    let h = '#' + path;
    if (query && Object.keys(query).length) {
      const qs = new URLSearchParams();
      for (const [k, v] of Object.entries(query)) {
        if (v !== undefined && v !== null && v !== '') qs.set(k, v);
      }
      const s = qs.toString();
      if (s) h += '?' + s;
    }
    return h;
  }

  function navigate(path, query) {
    const h = hrefFor(path, query);
    if (location.hash === h) render();
    else location.hash = h;
  }

  // Changes some query keys of the current route (null/'' removes one).
  function setQuery(changes) {
    const route = currentRoute();
    navigate(route.path || '/', Object.assign({}, route.query, changes));
  }

  function renderNav() {
    const box = document.getElementById('nav-pages');
    if (!box) return;
    const { path } = currentRoute();
    const active = path ? matchPage(path) : null;
    const activeId = active && (active.page.navId || active.page.id);
    box.innerHTML = '';
    const known = GROUPS.concat(navPages().map((p) => p.group).filter((g) => !GROUPS.includes(g)));
    for (const group of known) {
      const members = navPages().filter((p) => p.group === group);
      if (!members.length) continue;
      const section = h('div', { class: 'nav-group' }, h('div', { class: 'nav-group-title', text: group }));
      const ul = h('ul');
      for (const page of members) {
        const a = h('a', { href: '#' + page.path, class: activeId === page.id ? 'active' : null }, pageTitle(page));
        if (activeId === page.id) a.setAttribute('aria-current', 'page');
        if (page.key) a.appendChild(h('kbd', { class: 'nav-key', title: 'g then ' + page.key, text: 'g' + page.key }));
        ul.appendChild(h('li', null, a));
      }
      section.appendChild(ul);
      box.appendChild(section);
    }
  }

  let renderToken = 0;
  let leaveHooks = [];
  let liveUnsub = null;

  // Runs `fn` when the current page is left (or re-rendered).
  function onLeave(fn) {
    leaveHooks.push(fn);
  }

  function leavePage() {
    const hooks = leaveHooks;
    leaveHooks = [];
    for (const fn of hooks) {
      try { fn(); } catch (_) { /* a hook never breaks routing */ }
    }
  }

  // Renders the current route. `opts.quiet` (a live refresh) renders into a
  // detached element and swaps it in when done, keeping the old content,
  // the scroll position and the selected row meanwhile.
  async function render(opts) {
    const quiet = !!(opts && opts.quiet);
    const el = document.getElementById('page');
    const titleEl = document.getElementById('page-title');
    if (!el) return;
    const route = currentRoute();
    if (!route.path) {
      // the Overview is the landing page, whichever script registered it
      const first = pages.find((p) => p.id === 'overview') || navPages()[0] || pages[0];
      if (first) {
        location.replace('#' + first.path);
        return;
      }
    }
    const match = route.path ? matchPage(route.path) : null;
    renderNav();
    closeMenu();
    const token = ++renderToken;
    leavePage();
    if (liveUnsub) { liveUnsub(); liveUnsub = null; }
    if (!match) {
      el.innerHTML = '';
      titleEl.textContent = 'Not found';
      el.appendChild(errorBox('No page at ' + route.path));
      return;
    }
    const title = pageTitle(match.page, match.params);
    titleEl.textContent = title;
    document.title = title + ' · agent-mux';
    const params = Object.assign({}, route.query, match.params);
    const selected = quiet ? selectedRowIndex() : -1;
    const target = quiet ? h('div') : el;
    if (!quiet) el.innerHTML = '';
    try {
      await match.page.render(target, params);
      if (token !== renderToken) return;
      if (quiet) {
        el.replaceChildren(...target.childNodes);
        if (selected >= 0) selectRow(selected, false);
      }
    } catch (err) {
      if (token !== renderToken) return;
      el.innerHTML = '';
      el.appendChild(errorBox(err && err.message ? err.message : String(err)));
    }
    if (token === renderToken && match.page.live) {
      liveUnsub = live.subscribe(() => scheduleLiveRender(token));
    }
  }

  let liveTimer = null;
  function scheduleLiveRender(token) {
    clearTimeout(liveTimer);
    liveTimer = setTimeout(() => {
      if (token !== renderToken) return;
      // never pull the page from under someone typing or reading a dialog
      if (isTyping(document.activeElement) || document.querySelector('.overlay:not([hidden])')) {
        scheduleLiveRender(token);
        return;
      }
      render({ quiet: true });
    }, 400);
  }

  // ---- API ---------------------------------------------------------------

  // POST JSON to `path`; throws on a non-2xx status or an {ok:false} body.
  async function api(path, body) {
    const res = await fetch(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(body || {}),
    });
    let data = null;
    const text = await res.text();
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

  // ---- formatting ----------------------------------------------------------

  function num(n) {
    if (n === null || n === undefined || n === '') return null;
    n = Number(n);
    return isFinite(n) ? n : null;
  }

  function trim0(s) {
    return s.replace(/\.0$/, '');
  }

  // 950, 12.3k, 1.2M, 3.4B
  function fmtTokens(n) {
    n = num(n);
    if (n === null) return '–';
    const a = Math.abs(n);
    if (a >= 1e9) return trim0((n / 1e9).toFixed(1)) + 'B';
    if (a >= 1e6) return trim0((n / 1e6).toFixed(1)) + 'M';
    if (a >= 1e3) return trim0((n / 1e3).toFixed(1)) + 'k';
    return String(Math.round(n));
  }

  // 1,234,567
  function fmtNum(n) {
    n = num(n);
    if (n === null) return '–';
    return Math.round(n).toLocaleString('en-US');
  }

  // $3.41, $1.2k, $0.0042
  function fmtCost(usd) {
    usd = num(usd);
    if (usd === null) return '–';
    if (usd === 0) return '$0';
    const a = Math.abs(usd);
    if (a >= 1e4) return '$' + trim0((usd / 1e3).toFixed(1)) + 'k';
    if (a < 0.01) return '$' + usd.toFixed(4);
    return '$' + usd.toFixed(2);
  }

  // 0.42 → 42%
  function fmtPercent(ratio, digits) {
    ratio = num(ratio);
    if (ratio === null) return '–';
    return (ratio * 100).toFixed(digits === undefined ? 0 : digits) + '%';
  }

  // 850ms, 12.4s, 3m05s, 2h07m, 3d4h
  function fmtDuration(ms) {
    ms = num(ms);
    if (ms === null) return '–';
    if (ms < 1000) return Math.round(ms) + 'ms';
    const s = ms / 1000;
    if (s < 60) return s.toFixed(1) + 's';
    const m = Math.floor(s / 60);
    if (m < 60) return m + 'm' + String(Math.round(s % 60)).padStart(2, '0') + 's';
    const hr = Math.floor(m / 60);
    if (hr < 48) return hr + 'h' + String(m % 60).padStart(2, '0') + 'm';
    return Math.floor(hr / 24) + 'd' + (hr % 24) + 'h';
  }

  function toDate(t) {
    if (t === null || t === undefined || t === '') return null;
    if (t instanceof Date) return isNaN(t.getTime()) ? null : t;
    // a bare day is local midnight, not UTC
    if (typeof t === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(t)) t = t + 'T00:00:00';
    const d = typeof t === 'number' ? new Date(t > 1e14 ? t / 1e6 : t) : new Date(t);
    return isNaN(d.getTime()) ? null : d;
  }

  // RFC 3339 or a nanosecond count → local "YYYY-MM-DD HH:MM:SS".
  function fmtTime(t) {
    if (t === null || t === undefined || t === '') return '–';
    const d = toDate(t);
    if (!d) return String(t);
    const p = (n) => String(n).padStart(2, '0');
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  }

  // "just now", "3m ago", "5h ago", "2d ago"
  function fmtAgo(t) {
    const d = toDate(t);
    if (!d) return '–';
    const s = (Date.now() - d.getTime()) / 1000;
    if (s < 45) return 'just now';
    if (s < 3600) return Math.max(1, Math.round(s / 60)) + 'm ago';
    if (s < 48 * 3600) return Math.round(s / 3600) + 'h ago';
    return Math.round(s / 86400) + 'd ago';
  }

  // Local calendar day "YYYY-MM-DD" of a time.
  function dayKey(t) {
    const d = toDate(t);
    if (!d) return null;
    const p = (n) => String(n).padStart(2, '0');
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate());
  }

  // "24h", "7d", "90m", "2w" or a date → RFC 3339 (UTC). Returns '' for ''.
  function sinceToRfc3339(s) {
    if (!s) return '';
    s = String(s).trim();
    const m = /^(\d+)\s*([mhdw])$/i.exec(s);
    if (m) {
      const n = Number(m[1]);
      const unit = { m: 60e3, h: 3600e3, d: 86400e3, w: 7 * 86400e3 }[m[2].toLowerCase()];
      return new Date(Date.now() - n * unit).toISOString();
    }
    const d = new Date(s);
    if (!isNaN(d.getTime())) return d.toISOString();
    return s;
  }

  function shortPath(p) {
    if (!p) return '–';
    const parts = String(p).split('/').filter(Boolean);
    return parts.length > 2 ? '…/' + parts.slice(-2).join('/') : p;
  }

  // ---- DOM helpers -----------------------------------------------------------

  const SVG_NS = 'http://www.w3.org/2000/svg';

  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === null || v === false) continue;
        if (k === 'class') el.className = v;
        else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
        else if (k === 'text') el.textContent = v;
        else el.setAttribute(k, v === true ? '' : v);
      }
    }
    for (const c of children.flat()) {
      if (c === null || c === undefined || c === false) continue;
      el.appendChild(typeof c === 'string' || typeof c === 'number' ? document.createTextNode(String(c)) : c);
    }
    return el;
  }

  // An SVG element: svg('rect', {x: 1, …}, ...children).
  function svg(tag, attrs, ...children) {
    const el = document.createElementNS(SVG_NS, tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        if (v === undefined || v === null || v === false) continue;
        if (k === 'text') el.textContent = v;
        else el.setAttribute(k, v);
      }
    }
    for (const c of children.flat()) {
      if (c === null || c === undefined || c === false) continue;
      el.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
    }
    return el;
  }

  function errorBox(message) {
    return h('div', { class: 'error', role: 'alert', text: message });
  }

  function loading(text) {
    return h('div', { class: 'loading', role: 'status' }, h('span', { class: 'spinner', 'aria-hidden': 'true' }), text || 'Loading…');
  }

  // An empty state: a short line and an optional hint below it.
  function empty(text, hint) {
    return h('div', { class: 'empty' }, h('div', { class: 'empty-title', text: text || 'Nothing to show.' }), hint ? h('div', { class: 'empty-hint', text: hint }) : null);
  }

  function link(path, text, query) {
    return h('a', { href: hrefFor(path, query) }, text);
  }

  // table({columns: [{key, label, num, sort, render(row), class, title}], rows, sortable, empty, onRow})
  // Sorting is client-side: a click on a sortable header toggles the order.
  // Rows take part in j/k navigation; Enter opens one (onRow, else its first link).
  function table(opts) {
    const columns = opts.columns;
    const rows = (opts.rows || []).slice();
    const state = { key: opts.sortKey || null, desc: !!opts.sortDesc };
    const wrap = h('div', { class: 'table-wrap' });
    if (!rows.length) {
      wrap.appendChild(empty(opts.empty || 'Nothing to show.', opts.emptyHint));
      return wrap;
    }
    const tbl = h('table', { class: 'grid' });
    const thead = h('thead');
    const tbody = h('tbody');
    const sortable = opts.sortable !== false;
    function sortedRows() {
      if (!state.key) return rows;
      const col = columns.find((c) => c.key === state.key);
      const get = (col && col.sort) || ((r) => r[state.key]);
      const out = rows.slice().sort((a, b) => {
        const x = get(a), y = get(b);
        if (x === y) return 0;
        if (x === null || x === undefined) return 1;
        if (y === null || y === undefined) return -1;
        if (typeof x === 'number' && typeof y === 'number') return x - y;
        return String(x).localeCompare(String(y));
      });
      return state.desc ? out.reverse() : out;
    }
    function drawHead() {
      thead.innerHTML = '';
      const tr = h('tr');
      for (const col of columns) {
        const canSort = sortable && col.sortable !== false;
        const th = h('th', { class: [col.num ? 'num' : '', canSort ? 'sortable' : ''].join(' ').trim() || null }, col.label);
        if (state.key === col.key) {
          th.setAttribute('aria-sort', state.desc ? 'descending' : 'ascending');
          th.appendChild(h('span', { class: 'dir', text: state.desc ? '▼' : '▲' }));
        }
        if (canSort) {
          th.tabIndex = 0;
          const toggle = () => {
            if (state.key === col.key) state.desc = !state.desc;
            else { state.key = col.key; state.desc = !!col.num; }
            drawHead();
            drawBody();
          };
          th.addEventListener('click', toggle);
          th.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(); } });
        }
        tr.appendChild(th);
      }
      thead.appendChild(tr);
    }
    function drawBody() {
      tbody.innerHTML = '';
      for (const row of sortedRows()) {
        const tr = h('tr');
        for (const col of columns) {
          const cls = [col.num ? 'num' : '', col.class || ''].join(' ').trim();
          const td = h('td', { class: cls || null });
          const v = col.render ? col.render(row) : row[col.key];
          if (v instanceof Node) td.appendChild(v);
          else td.textContent = v === null || v === undefined ? '–' : String(v);
          if (col.title) td.title = col.title(row) || '';
          tr.appendChild(td);
        }
        if (opts.onRow) {
          tr.classList.add('clickable');
          tr.addEventListener('click', (e) => { if (e.target.tagName !== 'A') opts.onRow(row); });
          tr._open = () => opts.onRow(row);
        }
        tbody.appendChild(tr);
      }
    }
    drawHead();
    drawBody();
    tbl.appendChild(thead);
    tbl.appendChild(tbody);
    wrap.appendChild(tbl);
    return wrap;
  }

  // kv([[label, value], ...]) → a two-column table.
  function kv(pairs) {
    const tbl = h('table', { class: 'kv' });
    for (const [label, value] of pairs) {
      if (value === undefined) continue;
      const td = h('td');
      if (value instanceof Node) td.appendChild(value);
      else td.textContent = value === null || value === '' ? '–' : String(value);
      tbl.appendChild(h('tr', null, h('th', { text: label }), td));
    }
    return tbl;
  }

  // cards([[label, value, extra?], ...]); `extra` is a Node (a sparkline) or a hint string.
  function cards(items) {
    return h('div', { class: 'cards' }, items.map(([label, value, extra]) => h('div', { class: 'card' },
      h('div', { class: 'label', text: label }),
      h('div', { class: 'value', text: value === null || value === undefined ? '–' : String(value) }),
      extra instanceof Node ? h('div', { class: 'extra' }, extra) : extra ? h('div', { class: 'hint', text: extra }) : null)));
  }

  // The envelope's warnings and coverage as a short note, or null.
  function envelopeNote(env) {
    const bits = [];
    if (env && env.coverage && env.coverage.status && env.coverage.status !== 'complete') {
      bits.push('coverage ' + env.coverage.status + (env.coverage.reasons && env.coverage.reasons.length ? ': ' + env.coverage.reasons.join('; ') : ''));
    }
    if (env && env.warnings && env.warnings.length) bits.push(env.warnings.join('; '));
    if (env && env.truncated) bits.push('truncated');
    return bits.length ? h('div', { class: 'muted note', text: bits.join(' · ') }) : null;
  }

  // A titled panel around `children`.
  function panel(title, ...children) {
    return h('section', { class: 'panel' }, title ? h('h2', { class: 'panel-title', text: title }) : null, ...children);
  }

  // ---- theme -----------------------------------------------------------------

  const THEME_KEY = 'agent-mux-theme';
  const THEMES = ['auto', 'light', 'dark'];

  function storedTheme() {
    try { return localStorage.getItem(THEME_KEY) || 'auto'; } catch (_) { return 'auto'; }
  }

  function applyTheme(theme) {
    if (!THEMES.includes(theme)) theme = 'auto';
    const root = document.documentElement;
    if (theme === 'auto') root.removeAttribute('data-theme');
    else root.setAttribute('data-theme', theme);
    try { localStorage.setItem(THEME_KEY, theme); } catch (_) { /* private mode */ }
    const btn = document.getElementById('theme-toggle');
    if (btn) {
      btn.textContent = { auto: '◐ auto', light: '☀ light', dark: '☾ dark' }[theme];
      btn.title = 'Theme: ' + theme + ' (click to change)';
    }
    // charts read colours from CSS: redraw them
    document.querySelectorAll('.chart').forEach((c) => c._redraw && c._redraw());
  }

  function cycleTheme() {
    const now = storedTheme();
    applyTheme(THEMES[(THEMES.indexOf(now) + 1) % THEMES.length]);
  }

  function isDark() {
    const t = document.documentElement.getAttribute('data-theme');
    if (t) return t === 'dark';
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  }

  // ---- filter bar --------------------------------------------------------------

  const RANGES = ['24h', '7d', '30d', '90d', 'custom'];
  const PROVIDERS = ['claude', 'codex', 'antigravity', 'opencode'];

  // filters(params) → {range, since, until, provider, workspace}: the shared
  // filters of the hash query (`range`, `from`, `to`, `provider`,
  // `workspace`) with `since`/`until` as RFC 3339 ('' when open).
  function filters(params, defaults) {
    params = params || currentRoute().query;
    const d = Object.assign({ range: '7d' }, defaults || {});
    const range = RANGES.includes(params.range) ? params.range : d.range;
    let since = '';
    let until = '';
    if (range === 'custom') {
      const from = params.from ? new Date(params.from + 'T00:00:00') : null;
      const to = params.to ? new Date(params.to + 'T00:00:00') : null;
      if (from && !isNaN(from)) since = from.toISOString();
      if (to && !isNaN(to)) until = new Date(to.getTime() + 86400e3).toISOString();
    } else if (range) {
      since = sinceToRfc3339(range);
    }
    return {
      range,
      from: params.from || '',
      to: params.to || '',
      since,
      until,
      provider: params.provider || d.provider || '',
      workspace: params.workspace || d.workspace || '',
    };
  }

  // filterBar({params, fields, providers, workspaces, defaults}) → a form
  // whose controls write `range`/`from`/`to`, `provider` and `workspace`
  // into the hash query (so a filtered view is a link). `fields` picks
  // among 'range', 'provider', 'workspace' (default: all three);
  // `workspaces` fills the workspace suggestions.
  function filterBar(opts) {
    opts = opts || {};
    const fields = opts.fields || ['range', 'provider', 'workspace'];
    const f = filters(opts.params, opts.defaults);
    const bar = h('form', { class: 'filter-bar', role: 'search' });
    bar.addEventListener('submit', (e) => e.preventDefault());
    if (fields.includes('range')) {
      const seg = h('div', { class: 'segmented', role: 'group', 'aria-label': 'Date range' });
      for (const r of RANGES) {
        seg.appendChild(h('button', {
          type: 'button', class: r === f.range ? 'on' : null, 'aria-pressed': r === f.range ? 'true' : 'false',
          onclick: () => setQuery({ range: r, from: r === 'custom' ? f.from : null, to: r === 'custom' ? f.to : null }),
        }, r));
      }
      bar.appendChild(seg);
      if (f.range === 'custom') {
        const from = h('input', { type: 'date', name: 'from', value: f.from, 'aria-label': 'From' });
        const to = h('input', { type: 'date', name: 'to', value: f.to, 'aria-label': 'To' });
        const apply = () => setQuery({ range: 'custom', from: from.value, to: to.value });
        from.addEventListener('change', apply);
        to.addEventListener('change', apply);
        bar.appendChild(h('span', { class: 'dates' }, from, h('span', { class: 'muted', text: '→' }), to));
      }
    }
    if (fields.includes('provider')) {
      const sel = h('select', { name: 'provider', 'aria-label': 'Harness' },
        h('option', { value: '' }, 'all harnesses'),
        (opts.providers || PROVIDERS).map((p) => h('option', { value: p, selected: p === f.provider || null }, p)));
      sel.addEventListener('change', () => setQuery({ provider: sel.value }));
      bar.appendChild(sel);
    }
    if (fields.includes('workspace')) {
      const listId = 'ws-list-' + Math.random().toString(36).slice(2, 8);
      const input = h('input', { type: 'search', name: 'workspace', value: f.workspace, placeholder: 'workspace / project', list: listId, 'aria-label': 'Workspace or project' });
      const commit = () => { if (input.value !== f.workspace) setQuery({ workspace: input.value }); };
      input.addEventListener('change', commit);
      input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); commit(); } });
      bar.appendChild(input);
      bar.appendChild(h('datalist', { id: listId }, (opts.workspaces || []).map((w) => h('option', { value: w }))));
    }
    if (f.provider || f.workspace || (opts.params && opts.params.range)) {
      bar.appendChild(h('button', { type: 'button', class: 'link', onclick: () => setQuery({ range: null, from: null, to: null, provider: null, workspace: null }) }, 'reset'));
    }
    return bar;
  }

  // ---- charts ------------------------------------------------------------------

  // Series colours come from CSS (--c1 … --c8) so both themes apply.
  function seriesColor(i) {
    const v = getComputedStyle(document.documentElement).getPropertyValue('--c' + ((i % 8) + 1)).trim();
    return v || '#4c78a8';
  }

  let tipEl = null;
  function tip() {
    if (!tipEl) {
      tipEl = h('div', { class: 'chart-tip', role: 'tooltip', hidden: true });
      document.body.appendChild(tipEl);
    }
    return tipEl;
  }

  function showTip(e, text) {
    const t = tip();
    t.textContent = text;
    t.hidden = false;
    const pad = 12;
    const r = t.getBoundingClientRect();
    let x = e.clientX + pad;
    let y = e.clientY + pad;
    if (x + r.width > window.innerWidth - 4) x = e.clientX - r.width - pad;
    if (y + r.height > window.innerHeight - 4) y = e.clientY - r.height - pad;
    t.style.left = Math.max(4, x) + 'px';
    t.style.top = Math.max(4, y) + 'px';
  }

  function hideTip() {
    if (tipEl) tipEl.hidden = true;
  }

  // Every element with a `data-tip` shows it on hover/focus.
  document.addEventListener('mouseover', (e) => {
    const el = e.target.closest && e.target.closest('[data-tip]');
    if (el) showTip(e, el.getAttribute('data-tip'));
  });
  document.addEventListener('mousemove', (e) => {
    const el = e.target.closest && e.target.closest('[data-tip]');
    if (el) showTip(e, el.getAttribute('data-tip'));
    else hideTip();
  });
  document.addEventListener('scroll', hideTip, true);

  // A chart box that draws at its own width and redraws on resize/theme.
  function chartBox(cls, draw) {
    const box = h('div', { class: 'chart ' + (cls || '') });
    let lastW = 0;
    box._redraw = () => {
      const w = Math.max(160, Math.floor(box.clientWidth || 600));
      lastW = w;
      box.replaceChildren(draw(w));
    };
    if (window.ResizeObserver) {
      new ResizeObserver(() => {
        const w = Math.floor(box.clientWidth);
        if (w && Math.abs(w - lastW) > 2) box._redraw();
      }).observe(box);
    }
    box._redraw();
    return box;
  }

  // A round tick step for about four ticks up to `v`: 1, 2 or 5 × 10^n.
  function niceStep(v) {
    if (!(v > 0)) return 1;
    const raw = v / 4;
    const p = Math.pow(10, Math.floor(Math.log10(raw)));
    for (const m of [1, 2, 5, 10]) if (m * p >= raw) return m * p;
    return 10 * p;
  }

  // The axis top for a peak `v`: a whole number of niceStep(v) (of at
  // least 1 for counts, `integer`).
  function niceMax(v, integer) {
    const step = integer ? Math.max(1, niceStep(v)) : niceStep(v);
    return Number((step * Math.max(1, Math.ceil((v > 0 ? v : 1) / step - 1e-9))).toPrecision(12));
  }

  function legend(series) {
    if (series.length < 2) return null;
    return h('div', { class: 'legend' }, series.map((s, i) => h('span', { class: 'legend-item' },
      h('span', { class: 'swatch', style: 'background:' + seriesColor(i) }), s.name)));
  }

  // Shared frame of bar and line: a y grid on round ticks, thinned x labels.
  function frame(w, height, labels, max, fmt, integer) {
    const m = { l: 48, r: 8, t: 8, b: 22 };
    const iw = w - m.l - m.r;
    const ih = height - m.t - m.b;
    const root = svg('svg', { width: w, height, viewBox: '0 0 ' + w + ' ' + height, role: 'img' });
    const g = svg('g', { class: 'axis' });
    const step = integer ? Math.max(1, niceStep(max)) : niceStep(max);
    const ticks = Math.max(1, Math.round(max / step));
    for (let i = 0; i <= ticks; i++) {
      const v = Number((step * i).toPrecision(12));
      const y = m.t + ih - (ih * i) / ticks;
      g.appendChild(svg('line', { x1: m.l, x2: w - m.r, y1: y, y2: y, class: i ? 'grid' : 'base' }));
      g.appendChild(svg('text', { x: m.l - 6, y: y + 4, 'text-anchor': 'end', text: fmt(v) }));
    }
    const n = labels.length;
    const every = Math.max(1, Math.ceil(n / Math.max(1, Math.floor(iw / 64))));
    labels.forEach((l, i) => {
      if (i % every !== 0 && i !== n - 1) return;
      const x = m.l + (iw * (i + 0.5)) / n;
      g.appendChild(svg('text', { x, y: height - 6, 'text-anchor': 'middle', text: String(l) }));
    });
    root.appendChild(g);
    return { root, m, iw, ih };
  }

  // bar({labels, series: [{name, values}], stacked, height, format, onClick(i)})
  function bar(opts) {
    const labels = opts.labels || [];
    const series = opts.series || [];
    const fmt = opts.format || fmtNum;
    const height = opts.height || 200;
    const wrap = h('div', { class: 'chart-wrap' });
    if (!labels.length || !series.length) {
      wrap.appendChild(empty(opts.empty || 'No data for this range.'));
      return wrap;
    }
    const totals = labels.map((_, i) => series.reduce((a, s) => a + (Number(s.values[i]) || 0), 0));
    const peak = opts.stacked ? Math.max(0, ...totals) : Math.max(0, ...series.flatMap((s) => s.values.map((v) => Number(v) || 0)));
    const integer = fmt === fmtNum;
    const max = niceMax(peak, integer);
    wrap.appendChild(chartBox('bar', (w) => {
      const { root, m, iw, ih } = frame(w, height, labels, max, fmt, integer);
      const slot = iw / labels.length;
      const gap = Math.min(6, slot * 0.2);
      labels.forEach((label, i) => {
        let base = 0;
        series.forEach((s, j) => {
          const v = Number(s.values[i]) || 0;
          if (!v) return;
          const bh = (ih * v) / max;
          let x, bw, y;
          if (opts.stacked) {
            x = m.l + slot * i + gap / 2;
            bw = slot - gap;
            y = m.t + ih - (ih * base) / max - bh;
            base += v;
          } else {
            bw = (slot - gap) / series.length;
            x = m.l + slot * i + gap / 2 + bw * j;
            y = m.t + ih - bh;
          }
          const tipText = label + (series.length > 1 ? ' · ' + s.name : '') + ': ' + fmt(v) + (opts.stacked && series.length > 1 ? ' (total ' + fmt(totals[i]) + ')' : '');
          const r = svg('rect', { x: x.toFixed(1), y: y.toFixed(1), width: Math.max(1, bw).toFixed(1), height: Math.max(0.5, bh).toFixed(1), fill: seriesColor(j), rx: 1.5, 'data-tip': tipText, tabindex: '-1' });
          if (opts.onClick) { r.style.cursor = 'pointer'; r.addEventListener('click', () => opts.onClick(i)); }
          root.appendChild(r);
        });
      });
      return root;
    }));
    const lg = legend(series);
    if (lg) wrap.appendChild(lg);
    return wrap;
  }

  // line({labels, series: [{name, values}], area, height, format})
  function line(opts) {
    const labels = opts.labels || [];
    const series = opts.series || [];
    const fmt = opts.format || fmtNum;
    const height = opts.height || 200;
    const wrap = h('div', { class: 'chart-wrap' });
    if (!labels.length || !series.length) {
      wrap.appendChild(empty(opts.empty || 'No data for this range.'));
      return wrap;
    }
    const integer = fmt === fmtNum;
    const max = niceMax(Math.max(0, ...series.flatMap((s) => s.values.map((v) => Number(v) || 0))), integer);
    wrap.appendChild(chartBox('line', (w) => {
      const { root, m, iw, ih } = frame(w, height, labels, max, fmt, integer);
      const n = labels.length;
      const xAt = (i) => m.l + (iw * (i + 0.5)) / n;
      const yAt = (v) => m.t + ih - (ih * (Number(v) || 0)) / max;
      series.forEach((s, j) => {
        const pts = s.values.map((v, i) => xAt(i).toFixed(1) + ',' + yAt(v).toFixed(1));
        if (opts.area) {
          const d = 'M' + xAt(0).toFixed(1) + ',' + (m.t + ih) + ' L' + pts.join(' L') + ' L' + xAt(n - 1).toFixed(1) + ',' + (m.t + ih) + ' Z';
          root.appendChild(svg('path', { d, fill: seriesColor(j), 'fill-opacity': series.length > 1 ? 0.12 : 0.18, stroke: 'none' }));
        }
        root.appendChild(svg('polyline', { points: pts.join(' '), fill: 'none', stroke: seriesColor(j), 'stroke-width': 2, 'stroke-linejoin': 'round', 'stroke-linecap': 'round' }));
      });
      // one hover column per label carries the tooltip of every series
      labels.forEach((label, i) => {
        const text = label + ': ' + series.map((s) => (series.length > 1 ? s.name + ' ' : '') + fmt(s.values[i])).join(', ');
        const col = svg('g', { class: 'hover-col', 'data-tip': text });
        col.appendChild(svg('rect', { x: (m.l + (iw * i) / n).toFixed(1), y: m.t, width: (iw / n).toFixed(1), height: ih, fill: 'transparent' }));
        series.forEach((s, j) => col.appendChild(svg('circle', { cx: xAt(i).toFixed(1), cy: yAt(s.values[i]).toFixed(1), r: 3, fill: seriesColor(j), class: 'dot' })));
        root.appendChild(col);
      });
      return root;
    }));
    const lg = legend(series);
    if (lg) wrap.appendChild(lg);
    return wrap;
  }

  // heatmap({values: {'YYYY-MM-DD': n}, from, to, format, onClick(day)}):
  // a calendar of weeks (columns) by weekday (rows), five shades.
  function heatmap(opts) {
    const values = opts.values || {};
    const fmt = opts.format || fmtNum;
    const to = toDate(opts.to) || new Date();
    const from = toDate(opts.from) || new Date(to.getTime() - 364 * 86400e3);
    const start = new Date(from.getFullYear(), from.getMonth(), from.getDate());
    start.setDate(start.getDate() - start.getDay()); // back to Sunday
    const end = new Date(to.getFullYear(), to.getMonth(), to.getDate());
    const days = [];
    for (let d = new Date(start); d <= end; d.setDate(d.getDate() + 1)) days.push(new Date(d));
    const peak = Math.max(0, ...Object.values(values).map(Number).filter(isFinite));
    const weeks = Math.ceil(days.length / 7);
    const wrap = h('div', { class: 'chart-wrap' });
    wrap.appendChild(chartBox('heatmap', (w) => {
      const left = 26;
      const top = 16;
      const cell = Math.max(6, Math.min(16, Math.floor((w - left - 4) / weeks) - 2));
      const step = cell + 2;
      const width = left + weeks * step + 4;
      const height = top + 7 * step + 4;
      const root = svg('svg', { width, height, viewBox: '0 0 ' + width + ' ' + height, role: 'img', class: 'heatmap-svg' });
      const g = svg('g', { class: 'axis' });
      ['', 'Mon', '', 'Wed', '', 'Fri', ''].forEach((t, i) => { if (t) g.appendChild(svg('text', { x: 0, y: top + i * step + cell - 1, text: t })); });
      let lastMonth = -1;
      days.forEach((d, i) => {
        const wk = Math.floor(i / 7);
        if (d.getDay() === 0 && d.getMonth() !== lastMonth) {
          lastMonth = d.getMonth();
          g.appendChild(svg('text', { x: left + wk * step, y: 10, text: d.toLocaleString('en-US', { month: 'short' }) }));
        }
      });
      root.appendChild(g);
      days.forEach((d, i) => {
        const key = dayKey(d);
        const v = Number(values[key]) || 0;
        const out = d < from && dayKey(d) !== dayKey(from);
        const level = v <= 0 || !peak ? 0 : Math.min(4, 1 + Math.floor((v / peak) * 3.999));
        const r = svg('rect', {
          x: left + Math.floor(i / 7) * step, y: top + d.getDay() * step, width: cell, height: cell, rx: 2,
          class: 'heat l' + level + (out ? ' out' : ''), 'data-tip': key + ': ' + fmt(v),
        });
        if (opts.onClick) { r.style.cursor = 'pointer'; r.addEventListener('click', () => opts.onClick(key)); }
        root.appendChild(r);
      });
      return root;
    }));
    const scale = h('div', { class: 'legend heat-legend' }, 'less', [0, 1, 2, 3, 4].map((l) => h('span', { class: 'swatch heat-swatch l' + l })), 'more');
    wrap.appendChild(scale);
    return wrap;
  }

  // hbars({items: [{label, value, href, hint}], format, limit}): a ranked
  // list with a proportional bar behind each value.
  function hbars(opts) {
    const fmt = opts.format || fmtNum;
    const items = (opts.items || []).slice().sort((a, b) => (Number(b.value) || 0) - (Number(a.value) || 0)).slice(0, opts.limit || 10);
    if (!items.length) return empty(opts.empty || 'No data for this range.');
    const max = Math.max(...items.map((i) => Number(i.value) || 0)) || 1;
    const total = (opts.items || []).reduce((a, i) => a + (Number(i.value) || 0), 0);
    return h('ol', { class: 'hbars' }, items.map((it, n) => {
      const pct = ((Number(it.value) || 0) / max) * 100;
      const label = it.href ? h('a', { href: it.href }, it.label) : h('span', null, it.label);
      return h('li', { 'data-tip': it.label + ': ' + fmt(it.value) + (total ? ' (' + fmtPercent((Number(it.value) || 0) / total) + ')' : '') + (it.hint ? ' · ' + it.hint : '') },
        h('span', { class: 'hbar', style: 'width:' + pct.toFixed(1) + '%;background:' + seriesColor(opts.color === undefined ? 0 : opts.color) }),
        h('span', { class: 'hbar-label', title: it.title || null }, label),
        h('span', { class: 'hbar-value', text: fmt(it.value) }));
    }));
  }

  // sparkline(values, {width, height, area, format, labels}) → an inline SVG.
  function sparkline(values, opts) {
    opts = opts || {};
    const w = opts.width || 120;
    const ht = opts.height || 28;
    const vals = (values || []).map((v) => Number(v) || 0);
    const root = svg('svg', { width: w, height: ht, viewBox: '0 0 ' + w + ' ' + ht, class: 'sparkline', role: 'img' });
    if (vals.length < 2) return root;
    const max = Math.max(...vals) || 1;
    const fmt = opts.format || fmtNum;
    const xAt = (i) => 1 + ((w - 2) * i) / (vals.length - 1);
    const yAt = (v) => ht - 2 - ((ht - 4) * v) / max;
    const pts = vals.map((v, i) => xAt(i).toFixed(1) + ',' + yAt(v).toFixed(1));
    const color = seriesColor(opts.color || 0);
    if (opts.area !== false) root.appendChild(svg('path', { d: 'M1,' + ht + ' L' + pts.join(' L') + ' L' + (w - 1) + ',' + ht + ' Z', fill: color, 'fill-opacity': 0.15 }));
    root.appendChild(svg('polyline', { points: pts.join(' '), fill: 'none', stroke: color, 'stroke-width': 1.5 }));
    root.appendChild(svg('circle', { cx: xAt(vals.length - 1), cy: yAt(vals[vals.length - 1]), r: 2, fill: color }));
    const lab = opts.labels || [];
    root.setAttribute('data-tip', vals.map((v, i) => (lab[i] ? lab[i] + ' ' : '') + fmt(v)).slice(-7).join(' · '));
    return root;
  }

  // Days from `since` to now as 'YYYY-MM-DD' keys (for bucketing charts).
  function dayRange(since, until) {
    const end = toDate(until) || new Date();
    const start = toDate(since) || new Date(end.getTime() - 6 * 86400e3);
    const out = [];
    const d = new Date(start.getFullYear(), start.getMonth(), start.getDate());
    while (d <= end && out.length < 1000) {
      out.push(dayKey(d));
      d.setDate(d.getDate() + 1);
    }
    return out;
  }

  const charts = { bar, line, heatmap, hbars, sparkline, color: seriesColor, dayRange, niceMax, niceStep };

  // ---- live ----------------------------------------------------------------------

  // M.live.subscribe(fn) calls fn({seq, previous}) whenever the store's
  // change feed advances (GET /api/live, Server-Sent Events) and returns
  // the unsubscribe function. One EventSource serves every subscriber.
  const live = (function () {
    const subs = new Set();
    let source = null;
    let lastSeq = null;
    let status = 'connecting';
    function setStatus(s) {
      status = s;
      const dot = document.getElementById('live-status');
      if (dot) {
        dot.className = 'live-dot ' + s;
        dot.title = { live: 'Live: the page refreshes when the store changes', connecting: 'Connecting to the live channel…', offline: 'Live channel offline (retrying)' }[s];
        dot.setAttribute('aria-label', dot.title);
      }
    }
    function emit(data) {
      for (const fn of Array.from(subs)) {
        try { fn(data); } catch (err) { console.error('live subscriber', err); }
      }
    }
    function connect() {
      if (source || !window.EventSource) return;
      source = new EventSource('/api/live');
      source.addEventListener('open', () => setStatus('live'));
      source.addEventListener('error', () => setStatus(source && source.readyState === 2 ? 'offline' : 'connecting'));
      source.addEventListener('ready', (e) => {
        setStatus('live');
        const data = parse(e.data);
        // a reconnect that missed changes counts as one
        if (lastSeq !== null && data && data.seq !== lastSeq) emit({ seq: data.seq, previous: lastSeq });
        if (data) lastSeq = data.seq;
      });
      source.addEventListener('change', (e) => {
        const data = parse(e.data);
        if (!data) return;
        lastSeq = data.seq;
        emit(data);
      });
    }
    function parse(s) {
      try { return JSON.parse(s); } catch (_) { return null; }
    }
    return {
      subscribe(fn) {
        subs.add(fn);
        connect();
        return () => subs.delete(fn);
      },
      connect,
      get status() { return status; },
      get seq() { return lastSeq; },
    };
  })();

  // ---- overlays: palette and shortcuts ----------------------------------------------

  function isTyping(el) {
    if (!el) return false;
    const tag = el.tagName;
    return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || el.isContentEditable;
  }

  function openOverlay(id) {
    const o = document.getElementById(id);
    if (!o) return null;
    o.hidden = false;
    document.body.classList.add('modal-open');
    return o;
  }

  function closeOverlays() {
    let closed = false;
    document.querySelectorAll('.overlay').forEach((o) => {
      if (!o.hidden) { o.hidden = true; closed = true; }
    });
    document.body.classList.remove('modal-open');
    return closed;
  }

  let sessionCache = { at: 0, rows: [] };
  async function recentSessions() {
    if (Date.now() - sessionCache.at < 30e3) return sessionCache.rows;
    try {
      const env = await api('/api/trace/list_sessions', { limit: 100 });
      sessionCache = { at: Date.now(), rows: (env.data && env.data.sessions) || [] };
    } catch (_) {
      sessionCache = { at: Date.now(), rows: [] };
    }
    return sessionCache.rows;
  }

  function fuzzy(hay, needle) {
    hay = hay.toLowerCase();
    if (!needle) return 1;
    const i = hay.indexOf(needle);
    if (i >= 0) return 100 - i;
    let p = 0;
    for (const ch of needle) {
      p = hay.indexOf(ch, p);
      if (p < 0) return 0;
      p++;
    }
    return 1;
  }

  const palette = { items: [], index: 0 };

  function paletteCommands() {
    const items = navPages().map((p) => ({ kind: 'page', label: pageTitle(p), hint: p.group + (p.key ? ' · g ' + p.key : ''), run: () => navigate(p.path) }));
    items.push({ kind: 'action', label: 'Toggle theme', hint: 'auto → light → dark', run: cycleTheme });
    items.push({ kind: 'action', label: 'Keyboard shortcuts', hint: '?', run: () => setTimeout(showShortcuts, 0) });
    return items;
  }

  async function drawPalette() {
    const input = document.getElementById('palette-input');
    const list = document.getElementById('palette-list');
    const q = input.value.trim();
    const needle = q.toLowerCase();
    const scored = paletteCommands().map((c) => ({ c, s: fuzzy(c.label + ' ' + c.hint, needle) })).filter((x) => x.s > 0);
    scored.sort((a, b) => b.s - a.s);
    let items = scored.map((x) => x.c);
    const sessions = await recentSessions();
    if (input.value.trim() !== q) return; // typed on meanwhile
    const matches = sessions
      .map((s) => ({ s, score: needle ? fuzzy([s.session_key, s.launch_id, s.title, s.cwd].filter(Boolean).join(' '), needle) : 1 }))
      .filter((x) => x.score > (needle ? 1 : 0))
      .sort((a, b) => b.score - a.score)
      .slice(0, needle ? 8 : 5)
      .map(({ s }) => ({ kind: 'session', label: s.session_key || s.launch_id, hint: [s.provider, shortPath(s.cwd), fmtAgo(s.last_active)].filter(Boolean).join(' · '), run: () => (s.session_key ? navigate('/sessions/' + encodeURIComponent(s.session_key)) : openSession(s.launch_id)) }));
    items = items.concat(matches);
    if (q && !matches.some((m) => m.label === q)) {
      items.push({ kind: 'session', label: 'Open session ' + q, hint: 'by key or launch id', run: () => openSession(q), keepOpen: true });
    }
    palette.items = items;
    palette.index = Math.min(palette.index, Math.max(0, items.length - 1));
    list.innerHTML = '';
    if (!items.length) list.appendChild(h('li', { class: 'muted empty-row', text: 'No match.' }));
    items.forEach((it, i) => {
      const li = h('li', { role: 'option', id: 'pal-' + i, class: i === palette.index ? 'on' : null, 'aria-selected': i === palette.index ? 'true' : 'false' },
        h('span', { class: 'pal-kind', text: it.kind }), h('span', { class: 'pal-label', text: it.label }), h('span', { class: 'pal-hint', text: it.hint || '' }));
      li.addEventListener('mousedown', (e) => { e.preventDefault(); runPalette(i); });
      li.addEventListener('mousemove', () => { if (palette.index !== i) { palette.index = i; markPalette(); } });
      list.appendChild(li);
    });
    input.setAttribute('aria-activedescendant', items.length ? 'pal-' + palette.index : '');
  }

  function markPalette() {
    const list = document.getElementById('palette-list');
    Array.from(list.children).forEach((li, i) => {
      li.classList.toggle('on', i === palette.index);
      li.setAttribute('aria-selected', i === palette.index ? 'true' : 'false');
      if (i === palette.index) li.scrollIntoView({ block: 'nearest' });
    });
    document.getElementById('palette-input').setAttribute('aria-activedescendant', 'pal-' + palette.index);
  }

  function runPalette(i) {
    const it = palette.items[i];
    if (!it) return;
    if (!it.keepOpen) closeOverlays();
    it.run();
  }

  // GET /api/resolve?id=…: a session key or a launch id → {session_key, launch_id}.
  async function resolveSession(id) {
    const res = await fetch('/api/resolve?id=' + encodeURIComponent(id), { headers: { accept: 'application/json' } });
    let data = null;
    try { data = await res.json(); } catch (_) { data = null; }
    if (!res.ok || !data || data.ok === false) {
      const err = new Error((data && data.error) || res.status + ' ' + res.statusText);
      err.status = res.status;
      throw err;
    }
    return data;
  }

  // Opens a session named by its key or by one of its launch ids; an
  // unknown id keeps the palette open with the reason.
  async function openSession(id) {
    try {
      const r = await resolveSession(id);
      closeOverlays();
      navigate('/sessions/' + encodeURIComponent(r.session_key), r.launch_id ? { launch_id: r.launch_id } : null);
    } catch (err) {
      const list = document.getElementById('palette-list');
      if (list && !document.getElementById('palette').hidden) {
        list.replaceChildren(h('li', { class: 'muted empty-row', role: 'alert', text: err.message }));
        palette.items = [];
      } else {
        closeOverlays();
        navigate('/sessions/' + encodeURIComponent(id));
      }
    }
  }

  function showPalette() {
    closeOverlays();
    const o = openOverlay('palette');
    if (!o) return;
    const input = document.getElementById('palette-input');
    input.value = '';
    palette.index = 0;
    drawPalette();
    input.focus();
  }

  function showShortcuts() {
    closeOverlays();
    const o = openOverlay('shortcuts');
    if (!o) return;
    const body = document.getElementById('shortcuts-body');
    const row = (keys, what) => h('tr', null, h('td', null, keys.map((k) => h('kbd', { text: k }))), h('td', { text: what }));
    const jumps = navPages().filter((p) => p.key).map((p) => row(['g', p.key], pageTitle(p)));
    body.replaceChildren(
      h('table', { class: 'shortcuts' },
        h('tbody', null,
          row([navigator.platform && /Mac/.test(navigator.platform) ? '⌘' : 'Ctrl', 'K'], 'Command palette: pages, actions, jump to a session'),
          row(['j'], 'Next row'), row(['k'], 'Previous row'), row(['Enter'], 'Open the selected row'),
          row(['/'], 'Focus search or filter'), row(['t'], 'Toggle theme'), row(['?'], 'This overlay'), row(['Esc'], 'Close, or leave a field'),
          jumps)));
    o.querySelector('button.close').focus();
  }

  // ---- row navigation ------------------------------------------------------------------

  function tableRows() {
    return Array.from(document.querySelectorAll('#page table.grid tbody tr'));
  }

  function selectedRowIndex() {
    return tableRows().findIndex((r) => r.classList.contains('selected'));
  }

  function selectRow(i, scroll) {
    const rows = tableRows();
    if (!rows.length) return;
    i = Math.max(0, Math.min(rows.length - 1, i));
    rows.forEach((r, n) => {
      r.classList.toggle('selected', n === i);
      if (n === i) r.setAttribute('aria-selected', 'true');
      else r.removeAttribute('aria-selected');
    });
    if (scroll !== false) rows[i].scrollIntoView({ block: 'nearest' });
  }

  function openRow() {
    const rows = tableRows();
    const r = rows[selectedRowIndex()];
    if (!r) return false;
    if (r._open) { r._open(); return true; }
    const a = r.querySelector('a[href]');
    if (a) { a.click(); return true; }
    return false;
  }

  function focusSearch() {
    const page = document.getElementById('page');
    const target = page.querySelector('input[type=search]') || page.querySelector('input[type=text], input:not([type])') || page.querySelector('form input:not([type=hidden]), form select');
    if (target) { target.focus(); if (target.select) target.select(); return; }
    showPalette();
  }

  // ---- keyboard -------------------------------------------------------------------------

  let gPending = 0;

  function onKey(e) {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && !e.altKey && (e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
      if (!document.getElementById('palette').hidden) closeOverlays();
      else showPalette();
      return;
    }
    const pal = document.getElementById('palette');
    if (pal && !pal.hidden) {
      if (e.key === 'Escape') { e.preventDefault(); closeOverlays(); }
      else if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) { e.preventDefault(); palette.index = Math.min(palette.items.length - 1, palette.index + 1); markPalette(); }
      else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) { e.preventDefault(); palette.index = Math.max(0, palette.index - 1); markPalette(); }
      else if (e.key === 'Enter') { e.preventDefault(); runPalette(palette.index); }
      return;
    }
    if (e.key === 'Escape') {
      if (closeOverlays()) { e.preventDefault(); return; }
      if (isTyping(document.activeElement)) { document.activeElement.blur(); e.preventDefault(); }
      return;
    }
    if (mod || e.altKey || isTyping(e.target)) return;
    if (document.body.classList.contains('modal-open')) return;
    if (gPending && Date.now() - gPending < 1500) {
      gPending = 0;
      const page = navPages().find((p) => p.key === e.key);
      if (page) { e.preventDefault(); navigate(page.path); }
      return;
    }
    gPending = 0;
    switch (e.key) {
      case 'g': gPending = Date.now(); e.preventDefault(); break;
      case 'j': e.preventDefault(); selectRow(selectedRowIndex() + 1); break;
      case 'k': e.preventDefault(); selectRow(Math.max(0, selectedRowIndex() - 1)); break;
      case 'Enter': if (selectedRowIndex() >= 0 && openRow()) e.preventDefault(); break;
      case '/': e.preventDefault(); focusSearch(); break;
      case '?': e.preventDefault(); showShortcuts(); break;
      case 't': e.preventDefault(); cycleTheme(); break;
      default: break;
    }
  }

  // ---- shell wiring --------------------------------------------------------------------

  function closeMenu() {
    const app = document.getElementById('app');
    if (app) app.classList.remove('menu-open');
  }

  function wireShell() {
    applyTheme(storedTheme());
    const on = (id, ev, fn) => { const el = document.getElementById(id); if (el) el.addEventListener(ev, fn); };
    on('theme-toggle', 'click', cycleTheme);
    on('palette-button', 'click', showPalette);
    on('help-button', 'click', showShortcuts);
    on('menu-button', 'click', () => document.getElementById('app').classList.toggle('menu-open'));
    on('palette-input', 'input', () => { palette.index = 0; drawPalette(); });
    document.querySelectorAll('.overlay').forEach((o) => {
      o.addEventListener('mousedown', (e) => { if (e.target === o) closeOverlays(); });
      const close = o.querySelector('button.close');
      if (close) close.addEventListener('click', closeOverlays);
    });
    const kbd = document.getElementById('palette-kbd');
    if (kbd && navigator.platform && !/Mac/.test(navigator.platform)) kbd.textContent = 'Ctrl K';
    if (window.matchMedia) {
      const mq = window.matchMedia('(prefers-color-scheme: dark)');
      const redraw = () => document.querySelectorAll('.chart').forEach((c) => c._redraw && c._redraw());
      if (mq.addEventListener) mq.addEventListener('change', redraw);
    }
    document.addEventListener('keydown', onKey);
    live.connect();
    render();
  }

  window.AgentMux = {
    registerPage, navigate, currentRoute, matchPage, pages, setQuery, hrefFor, onLeave, groups: GROUPS,
    api,
    fmtTokens, fmtNum, fmtCost, fmtPercent, fmtDuration, fmtTime, fmtAgo, dayKey, sinceToRfc3339, shortPath,
    h, svg, errorBox, loading, empty, link, table, kv, cards, panel, envelopeNote,
    filters, filterBar, providers: PROVIDERS,
    charts, live,
    theme: { cycle: cycleTheme, apply: applyTheme, isDark },
    palette: { open: showPalette }, openSession, resolveSession, shortcuts: { open: showShortcuts },
    render,
  };

  window.addEventListener('hashchange', () => render());
  if (document.readyState === 'loading') window.addEventListener('DOMContentLoaded', wireShell);
  else wireShell();
})();
