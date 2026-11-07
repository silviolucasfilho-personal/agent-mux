// agent-mux web: hash router, page registry, API client and helpers.
// Every other script registers its pages with AgentMux.registerPage and
// uses AgentMux.api; nothing here is specific to one area.
(function () {
  'use strict';

  const pages = [];

  // ---- page registry -----------------------------------------------------

  // registerPage({id, title, path, render(el, params), nav})
  //   path: '/sessions' or '/sessions/:key' (one segment per ':name');
  //   nav: false keeps the page out of the left nav (detail pages).
  function registerPage(page) {
    if (!page || !page.id || !page.path || typeof page.render !== 'function') {
      throw new Error('registerPage needs id, path and render');
    }
    page.segments = page.path.replace(/^\//, '').split('/');
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

  function navigate(path, query) {
    let h = '#' + path;
    if (query && Object.keys(query).length) {
      const qs = new URLSearchParams();
      for (const [k, v] of Object.entries(query)) {
        if (v !== undefined && v !== null && v !== '') qs.set(k, v);
      }
      const s = qs.toString();
      if (s) h += '?' + s;
    }
    if (location.hash === h) render();
    else location.hash = h;
  }

  function renderNav() {
    const ul = document.getElementById('nav-pages');
    if (!ul) return;
    const { path } = currentRoute();
    const active = path ? matchPage(path) : null;
    ul.innerHTML = '';
    for (const page of pages) {
      if (page.nav === false) continue;
      const li = document.createElement('li');
      const a = document.createElement('a');
      a.href = '#' + page.path;
      a.textContent = page.title;
      const activeId = active && (active.page.navId || active.page.id);
      if (activeId === page.id) a.className = 'active';
      li.appendChild(a);
      ul.appendChild(li);
    }
  }

  let renderToken = 0;

  async function render() {
    const el = document.getElementById('page');
    const titleEl = document.getElementById('page-title');
    if (!el) return;
    const route = currentRoute();
    if (!route.path) {
      const first = pages.find((p) => p.nav !== false) || pages[0];
      if (first) {
        location.replace('#' + first.path);
        return;
      }
    }
    const match = route.path ? matchPage(route.path) : null;
    renderNav();
    const token = ++renderToken;
    el.innerHTML = '';
    if (!match) {
      titleEl.textContent = 'Not found';
      el.appendChild(errorBox('No page at ' + route.path));
      return;
    }
    const title = typeof match.page.title === 'function' ? match.page.title(match.params) : match.page.title;
    titleEl.textContent = title;
    document.title = title + ' · agent-mux';
    const params = Object.assign({}, route.query, match.params);
    try {
      await match.page.render(el, params);
    } catch (err) {
      if (token !== renderToken) return;
      el.innerHTML = '';
      el.appendChild(errorBox(err && err.message ? err.message : String(err)));
    }
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

  function fmtTokens(n) {
    if (n === null || n === undefined) return '–';
    n = Number(n);
    if (!isFinite(n)) return '–';
    if (Math.abs(n) >= 1e6) return (n / 1e6).toFixed(2) + 'M';
    if (Math.abs(n) >= 1e4) return (n / 1e3).toFixed(1) + 'k';
    return String(Math.round(n));
  }

  function fmtCost(usd) {
    if (usd === null || usd === undefined) return '–';
    usd = Number(usd);
    if (!isFinite(usd)) return '–';
    if (usd === 0) return '$0';
    if (Math.abs(usd) < 0.01) return '$' + usd.toFixed(4);
    return '$' + usd.toFixed(2);
  }

  function fmtDuration(ms) {
    if (ms === null || ms === undefined) return '–';
    ms = Number(ms);
    if (!isFinite(ms)) return '–';
    if (ms < 1000) return Math.round(ms) + 'ms';
    const s = ms / 1000;
    if (s < 60) return s.toFixed(1) + 's';
    const m = Math.floor(s / 60);
    if (m < 60) return m + 'm' + String(Math.round(s % 60)).padStart(2, '0') + 's';
    const h = Math.floor(m / 60);
    return h + 'h' + String(m % 60).padStart(2, '0') + 'm';
  }

  // RFC 3339 or a nanosecond count → local "YYYY-MM-DD HH:MM:SS".
  function fmtTime(t) {
    if (t === null || t === undefined || t === '') return '–';
    let d;
    if (typeof t === 'number') d = new Date(t > 1e14 ? t / 1e6 : t);
    else d = new Date(t);
    if (isNaN(d.getTime())) return String(t);
    const p = (n) => String(n).padStart(2, '0');
    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
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

  function errorBox(message) {
    return h('div', { class: 'error', role: 'alert', text: message });
  }

  function loading(text) {
    return h('div', { class: 'loading', text: text || 'Loading…' });
  }

  function link(path, text, query) {
    let href = '#' + path;
    if (query) {
      const qs = new URLSearchParams(query).toString();
      if (qs) href += '?' + qs;
    }
    return h('a', { href }, text);
  }

  // table({columns: [{key, label, num, sort, render(row), class}], rows, sortable, empty})
  // Sorting is client-side: a click on a sortable header toggles the order.
  function table(opts) {
    const columns = opts.columns;
    const rows = (opts.rows || []).slice();
    const state = { key: opts.sortKey || null, desc: !!opts.sortDesc };
    const wrap = h('div', { class: 'table-wrap' });
    if (!rows.length) {
      wrap.appendChild(h('div', { class: 'empty', text: opts.empty || 'Nothing to show.' }));
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
        const th = h('th', { class: [col.num ? 'num' : '', canSort ? 'sortable' : ''].join(' ').trim() }, col.label);
        if (state.key === col.key) th.appendChild(h('span', { class: 'dir', text: state.desc ? '▼' : '▲' }));
        if (canSort) {
          th.addEventListener('click', () => {
            if (state.key === col.key) state.desc = !state.desc;
            else { state.key = col.key; state.desc = !!col.num; }
            drawHead();
            drawBody();
          });
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
        if (opts.onRow) tr.addEventListener('click', (e) => { if (e.target.tagName !== 'A') opts.onRow(row); });
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

  function cards(items) {
    return h('div', { class: 'cards' }, items.map(([label, value]) => h('div', { class: 'card' }, h('div', { class: 'label', text: label }), h('div', { class: 'value', text: value }))));
  }

  // The envelope's warnings and coverage as a short note, or null.
  function envelopeNote(env) {
    const bits = [];
    if (env && env.coverage && env.coverage.status && env.coverage.status !== 'complete') {
      bits.push('coverage ' + env.coverage.status + (env.coverage.reasons && env.coverage.reasons.length ? ': ' + env.coverage.reasons.join('; ') : ''));
    }
    if (env && env.warnings && env.warnings.length) bits.push(env.warnings.join('; '));
    if (env && env.truncated) bits.push('truncated');
    return bits.length ? h('div', { class: 'muted', text: bits.join(' · ') }) : null;
  }

  window.AgentMux = {
    registerPage, navigate, currentRoute, matchPage, pages,
    api,
    fmtTokens, fmtCost, fmtDuration, fmtTime, sinceToRfc3339, shortPath,
    h, errorBox, loading, link, table, kv, cards, envelopeNote,
    render,
  };

  window.addEventListener('hashchange', render);
  window.addEventListener('DOMContentLoaded', render);
})();
