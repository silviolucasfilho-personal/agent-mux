'use strict';
/* agent-mux remote control.
 *
 * Mirrors every session the desktop TUI is running and types into them.
 * Two rules shape everything here:
 *
 *  - The remote never resizes a PTY. All sessions share one pane size owned
 *    by the desktop, so the terminal is created at the server's cols x rows
 *    and the page scales the element with CSS to fit.
 *  - A gap in a terminal byte stream corrupts the screen, so the server
 *    withholds output from a client that fell behind and sends a resync
 *    instead. On resync the client resets the terminal before writing.
 *
 * The token lives in an HttpOnly cookie the server sets from ?token=; this
 * file never sees it.
 */
(() => {
  const $ = (id) => document.getElementById(id);
  const ui = {
    banner: $('banner'), conn: $('conn'), chips: $('chips'), badge: $('badge'),
    add: $('add'), host: $('term-host'), term: $('term'), empty: $('empty'),
    keys: $('keys'), keyrow2: $('keyrow2'), kb: $('kb'), more: $('more'), paste: $('paste'),
    sheet: $('sheet'), sheetTabs: $('sheet-tabs'), sheetBody: $('sheet-body'),
    sheetGo: $('sheet-go'), sheetCancel: $('sheet-cancel'),
    menu: $('menu'), menuBody: $('menu-body'), menuCancel: $('menu-cancel'),
  };

  const KIND_OUTPUT = 0x01, KIND_RESYNC = 0x02, KIND_INPUT = 0x10;
  const NARROW = window.matchMedia('(max-width: 900px)');

  const state = {
    phase: 'connecting',
    ws: null,
    sid: null,             // session this client is watching
    sessions: new Map(),   // session_id -> {info, unread}
    order: [],             // session ids in server order
    desktop: null,         // the id the desktop TUI has selected
    pane: { cols: 80, rows: 24 },
    catalog: { profiles: [], skills: [], loops: [], workflows: [] },
    allowKill: true,
    allowLaunch: true,
    attempts: 0,
    sticky: { ctrl: false, alt: false },
    pendingSelect: null,   // select this session id once it shows up
    sheetTab: 'session',
    sheetPick: {},
    heartbeat: null,
    missedPongs: 0,
  };

  let term = null;

  // ---------------------------------------------------------------- terminal

  function initTerminal() {
    term = new window.Terminal({
      allowProposedApi: true,
      cols: state.pane.cols,
      rows: state.pane.rows,
      scrollback: 1000,       // matches vt100's scrollback in the TUI
      convertEol: false,      // the PTY stream already carries CRLF
      cursorBlink: false,
      cursorStyle: 'block',
      windowsMode: false,
      macOptionIsMeta: true,
      drawBoldTextInBrightColors: true,
      minimumContrastRatio: 1, // never repaint the harness's own palette
      fontSize: 14,
      lineHeight: 1.0,
      fontFamily: '"SF Mono", Menlo, "DejaVu Sans Mono", "Noto Sans Mono", "Cascadia Mono", Consolas, "Liberation Mono", monospace',
      theme: {
        background: '#0b0e14', foreground: '#d7dae0', cursor: '#d7dae0',
        selectionBackground: '#3a4a6a',
      },
    });
    // vt100 measures cells with the unicode-width crate; Unicode 11 is the
    // table that agrees with it, so columns line up with the desktop.
    try {
      const addon = new window.Unicode11Addon.Unicode11Addon();
      term.loadAddon(addon);
      term.unicode.activeVersion = '11';
    } catch (e) {
      console.debug('unicode11 addon unavailable', e);
    }
    term.open(ui.term);

    const helper = ui.term.querySelector('.xterm-helper-textarea');
    if (helper) {
      helper.setAttribute('inputmode', 'text');
      helper.setAttribute('autocapitalize', 'off');
      helper.setAttribute('autocorrect', 'off');
      helper.setAttribute('spellcheck', 'false');
    }

    term.onData(sendInput);
    term.onBell(() => {
      if (navigator.vibrate) navigator.vibrate(30);
    });
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(fitTerminal);
  }

  function fitTerminal() {
    if (!term || !term.element) return;
    const el = term.element;
    el.style.transform = 'none';
    const w = el.offsetWidth, h = el.offsetHeight;
    if (!w || !h) return;
    const fitW = ui.host.clientWidth / w;
    const fitH = ui.host.clientHeight / h;
    // Phones fit the width and let the viewport scroll vertically; on a
    // desktop never upscale past 1:1.
    const k = NARROW.matches ? fitW : Math.min(1, fitW, fitH);
    el.style.transform = `scale(${k})`;
  }

  function applyResync(bytes) {
    if (!term) return;
    term.reset();
    term.write(bytes, () => {
      term.scrollToBottom();
      ui.host.classList.remove('stale');
    });
  }

  // --------------------------------------------------------------- websocket

  function connect() {
    setPhase('connecting');
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    let ws;
    try {
      ws = new WebSocket(`${proto}//${location.host}/ws`);
    } catch (e) {
      return scheduleReconnect(String(e));
    }
    ws.binaryType = 'arraybuffer';
    state.ws = ws;
    ws.onopen = () => send({ t: 'hello', focus: state.sid });
    ws.onmessage = onFrame;
    ws.onerror = () => {};
    ws.onclose = (ev) => {
      stopHeartbeat();
      if (ev.code === 4401) {
        setPhase('stopped');
        showBanner('This link is no longer authorized. Open the URL from the desktop again.', 'error', true);
        return;
      }
      scheduleReconnect(`closed ${ev.code}`);
    };
  }

  function send(obj) {
    if (state.ws && state.ws.readyState === WebSocket.OPEN) {
      state.ws.send(JSON.stringify(obj));
      return true;
    }
    return false;
  }

  function sendBinary(kind, session, bytes) {
    if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
    const frame = new Uint8Array(5 + bytes.length);
    frame[0] = kind;
    new DataView(frame.buffer).setUint32(1, session, false); // big endian
    frame.set(bytes, 5);
    state.ws.send(frame);
  }

  function onFrame(ev) {
    if (typeof ev.data !== 'string') {
      const buf = new Uint8Array(ev.data);
      if (buf.length < 5) return;
      const kind = buf[0];
      const sid = new DataView(ev.data).getUint32(1, false);
      const payload = buf.subarray(5);
      if (kind === KIND_RESYNC) {
        if (sid === state.sid) applyResync(payload);
      } else if (kind === KIND_OUTPUT) {
        if (sid === state.sid) {
          term.write(payload);
        } else {
          const entry = state.sessions.get(sid);
          if (entry && !entry.unread) { entry.unread = true; renderSessions(); }
        }
      }
      return;
    }
    let msg;
    try { msg = JSON.parse(ev.data); } catch (e) { return; }
    switch (msg.t) {
      case 'hello': onHello(msg); break;
      case 'sessions': onSessions(msg); break;
      case 'catalog': state.catalog = msg; if (!ui.sheet.hidden) renderSheet(); break;
      case 'exit': break; // the sessions frame that follows carries the state
      case 'result': onResult(msg); break;
      case 'notice': showBanner(msg.text, msg.level === 'error' ? 'error' : 'warn', false); break;
      case 'pong': state.missedPongs = 0; break;
      default: console.debug('unknown frame', msg.t);
    }
  }

  function onHello(msg) {
    setPhase('ready');
    state.attempts = 0;
    state.allowKill = msg.allow_kill;
    state.allowLaunch = msg.allow_launch;
    applyPane(msg.cols, msg.rows);
    hideBanner();
    startHeartbeat();
  }

  function applyPane(cols, rows) {
    if (!cols || !rows) return;
    if (cols === state.pane.cols && rows === state.pane.rows && term) return fitTerminal();
    state.pane = { cols, rows };
    if (term) { term.resize(cols, rows); fitTerminal(); }
  }

  function onResult(msg) {
    if (!msg.ok) {
      showBanner(msg.error || 'That did not work.', 'error', false);
      return;
    }
    closeSheet();
    if (typeof msg.s === 'number') {
      // The session may not be in the list yet; take it on arrival.
      if (state.sessions.has(msg.s)) selectSession(msg.s);
      else state.pendingSelect = msg.s;
    }
  }

  function scheduleReconnect(why) {
    if (state.phase === 'stopped') return;
    setPhase('closed');
    ui.host.classList.add('stale');
    state.attempts += 1;
    const base = Math.min(10000, 500 * Math.pow(2, state.attempts - 1));
    const delay = base * (0.8 + Math.random() * 0.4);
    showBanner(`Reconnecting… (${state.attempts})`, 'info', true);
    console.debug('reconnect', why, Math.round(delay));
    setTimeout(connect, delay);
  }

  function startHeartbeat() {
    stopHeartbeat();
    state.missedPongs = 0;
    state.heartbeat = setInterval(() => {
      // iOS keeps half-dead sockets open for minutes; two silent pings is
      // enough to call it.
      if (state.missedPongs >= 2 && state.ws) { state.ws.close(); return; }
      state.missedPongs += 1;
      send({ t: 'ping' });
    }, 20000);
  }

  function stopHeartbeat() {
    if (state.heartbeat) clearInterval(state.heartbeat);
    state.heartbeat = null;
  }

  function setPhase(phase) {
    state.phase = phase;
    ui.conn.className = 'dot ' + (phase === 'ready' ? 'ready' : phase === 'connecting' ? 'connecting' : 'closed');
    ui.conn.title = phase;
  }

  // ---------------------------------------------------------------- sessions

  function onSessions(msg) {
    applyPane(msg.cols, msg.rows);
    state.desktop = typeof msg.selected === 'number' ? msg.selected : null;
    const seen = new Set();
    state.order = [];
    for (const s of msg.sessions) {
      seen.add(s.session_id);
      state.order.push(s.session_id);
      const entry = state.sessions.get(s.session_id);
      if (entry) entry.info = s;
      else state.sessions.set(s.session_id, { info: s, unread: false });
    }
    for (const id of [...state.sessions.keys()]) {
      if (!seen.has(id)) state.sessions.delete(id);
    }
    if (state.pendingSelect !== null && state.sessions.has(state.pendingSelect)) {
      const want = state.pendingSelect;
      state.pendingSelect = null;
      selectSession(want);
    } else if (state.sid === null || !state.sessions.has(state.sid)) {
      const next = state.sessions.has(state.desktop) ? state.desktop : state.order[0];
      if (next !== undefined) selectSession(next);
      else { state.sid = null; if (term) term.reset(); }
    }
    renderSessions();
  }

  function stateClass(s) {
    switch (s) {
      case 'working': return 'green';
      case 'waiting_for_user': return 'yellow';
      case 'exited': case 'disconnected': return 'red';
      default: return '';
    }
  }

  function renderSessions() {
    ui.chips.textContent = '';
    let waiting = 0;
    for (const id of state.order) {
      const entry = state.sessions.get(id);
      if (!entry) continue;
      const info = entry.info;
      if (info.state === 'waiting_for_user') waiting += 1;

      const chip = document.createElement('button');
      chip.className = 'chip';
      if (id === state.sid) chip.classList.add('current');
      if (id === state.desktop) chip.classList.add('desktop');
      if (entry.unread) chip.classList.add('unread');
      chip.dataset.sid = String(id);

      const dot = document.createElement('span');
      dot.className = 'dot ' + stateClass(info.state);
      dot.style.background = `var(--${stateClass(info.state) || 'dim'})`;
      chip.appendChild(dot);

      const label = document.createElement('span');
      label.className = 'label';
      label.textContent = info.name || `session ${id}`;
      chip.appendChild(label);

      if (info.state === 'exited') {
        const code = document.createElement('span');
        code.className = 'code';
        code.textContent = typeof info.exit_code === 'number' ? `exit ${info.exit_code}` : 'exited';
        chip.appendChild(code);
      }

      chip.addEventListener('click', () => selectSession(id));
      bindLongPress(chip, () => openSessionMenu(id));
      ui.chips.appendChild(chip);
    }
    ui.badge.hidden = waiting === 0;
    ui.badge.textContent = String(waiting);
    ui.empty.hidden = state.order.length > 0;
    document.title = waiting > 0 ? `(${waiting}) agent-mux` : 'agent-mux';
  }

  function selectSession(id) {
    if (!state.sessions.has(id)) return;
    state.sid = id;
    const entry = state.sessions.get(id);
    if (entry) entry.unread = false;
    if (term) term.reset();
    ui.host.classList.add('stale');
    send({ t: 'select', s: id });
    renderSessions();
  }

  // ------------------------------------------------------------------- input

  function sendInput(data) {
    if (state.sid === null) return;
    let text = data;
    // Sticky modifiers apply to the next character typed, so a phone
    // keyboard can produce Ctrl+C without a hardware Ctrl.
    if (state.sticky.ctrl && text.length === 1) {
      const c = text.toLowerCase().charCodeAt(0);
      if (c >= 97 && c <= 122) text = String.fromCharCode(c - 96);
      else if (c >= 64 && c < 96) text = String.fromCharCode(c - 64);
    }
    if (state.sticky.alt && text.length >= 1) text = '\x1b' + text;
    if (state.sticky.ctrl || state.sticky.alt) clearSticky();
    sendBinary(KIND_INPUT, state.sid, new TextEncoder().encode(text));
  }

  function sendKey(key, ch, mods) {
    if (state.sid === null) return;
    const all = new Set(mods || []);
    if (state.sticky.ctrl) all.add('ctrl');
    if (state.sticky.alt) all.add('alt');
    if (all.size) clearSticky();
    send({ t: 'key', s: state.sid, key, ch, mods: [...all] });
  }

  async function sendPaste() {
    if (state.sid === null) return;
    // xterm's hidden textarea handles a normal paste, but on a phone there
    // is frequently no way to aim a paste at it. Reading the clipboard
    // ourselves and letting the server wrap it (it knows whether the app
    // enabled bracketed paste) is the reliable path.
    if (!navigator.clipboard || !navigator.clipboard.readText) {
      return showBanner('This browser will not share the clipboard; paste into the terminal instead.', 'warn', false);
    }
    try {
      const text = await navigator.clipboard.readText();
      if (text) send({ t: 'paste', s: state.sid, text });
    } catch (e) {
      showBanner('Clipboard permission denied.', 'warn', false);
    }
  }

  function clearSticky() {
    state.sticky.ctrl = false;
    state.sticky.alt = false;
    for (const b of document.querySelectorAll('.key.sticky')) b.classList.remove('armed');
  }

  function bindKeyBar() {
    for (const btn of document.querySelectorAll('.key[data-key], .key[data-char]')) {
      const key = btn.dataset.key || 'char';
      const ch = btn.dataset.char;
      const mods = btn.dataset.mods ? btn.dataset.mods.split(',') : [];
      const fire = () => sendKey(key, ch, mods);
      btn.addEventListener('click', (e) => { e.preventDefault(); fire(); });
      if (btn.dataset.repeat) bindRepeat(btn, fire);
    }
    for (const btn of document.querySelectorAll('.key.sticky')) {
      btn.addEventListener('click', () => {
        const which = btn.dataset.sticky;
        const on = !state.sticky[which];
        clearSticky();
        state.sticky[which] = on;
        btn.classList.toggle('armed', on);
      });
    }
    ui.kb.addEventListener('click', () => term && term.focus());
    ui.paste.addEventListener('click', sendPaste);
    ui.more.addEventListener('click', () => { ui.keyrow2.hidden = !ui.keyrow2.hidden; fitTerminal(); });
    ui.add.addEventListener('click', () => openLaunchSheet('session'));
    if (!NARROW.matches) ui.keys.classList.add('auto-hidden');
  }

  function bindRepeat(el, fn) {
    let timer = null, delay = null;
    const stop = () => { clearTimeout(delay); clearInterval(timer); timer = delay = null; };
    el.addEventListener('pointerdown', () => {
      delay = setTimeout(() => { timer = setInterval(fn, 80); }, 400);
    });
    for (const ev of ['pointerup', 'pointercancel', 'pointerleave']) el.addEventListener(ev, stop);
  }

  function bindLongPress(el, fn) {
    let timer = null, startX = 0, startY = 0;
    el.addEventListener('pointerdown', (e) => {
      startX = e.clientX; startY = e.clientY;
      timer = setTimeout(() => { timer = null; fn(); }, 500);
    });
    const cancel = (e) => {
      if (timer && e && Math.hypot(e.clientX - startX, e.clientY - startY) > 10) { clearTimeout(timer); timer = null; }
    };
    el.addEventListener('pointermove', cancel);
    for (const ev of ['pointerup', 'pointercancel', 'pointerleave']) {
      el.addEventListener(ev, () => { if (timer) { clearTimeout(timer); timer = null; } });
    }
  }

  // ------------------------------------------------------------ launch sheet

  function openLaunchSheet(tab) {
    if (!state.allowLaunch) { showBanner('Launching is disabled on this server.', 'warn', false); return; }
    state.sheetTab = tab;
    state.sheetPick = {};
    ui.sheet.hidden = false;
    send({ t: 'catalog' });
    renderSheet();
  }

  function closeSheet() { ui.sheet.hidden = true; ui.menu.hidden = true; }

  function field(parent, labelText, el) {
    const l = document.createElement('label');
    l.textContent = labelText;
    parent.appendChild(l);
    parent.appendChild(el);
    return el;
  }

  function input(value, placeholder) {
    const el = document.createElement('input');
    el.type = 'text';
    el.value = value || '';
    if (placeholder) el.placeholder = placeholder;
    el.autocapitalize = 'off';
    el.autocorrect = 'off';
    el.spellcheck = false;
    return el;
  }

  function label(parent, text) {
    const l = document.createElement('label');
    l.textContent = text;
    parent.appendChild(l);
  }

  function picker(parent, items, key, onPick) {
    const box = document.createElement('div');
    box.className = 'picker';
    for (const it of items) {
      const b = document.createElement('button');
      b.className = 'pick';
      if (state.sheetPick[key] === it.value) b.classList.add('on');
      b.textContent = it.label;
      if (it.sub) {
        const s = document.createElement('span');
        s.className = 'sub';
        s.textContent = ' ' + it.sub;
        b.appendChild(s);
      }
      b.addEventListener('click', () => {
        state.sheetPick[key] = it.value;
        if (onPick) onPick(it);
        renderSheet();
      });
      box.appendChild(b);
    }
    parent.appendChild(box);
    return box;
  }

  function renderSheet() {
    for (const t of ui.sheetTabs.querySelectorAll('.tab')) {
      t.classList.toggle('on', t.dataset.tab === state.sheetTab);
    }
    const body = ui.sheetBody;
    body.textContent = '';
    const c = state.catalog;

    if (state.sheetTab === 'session') {
      const profiles = (c.profiles || []).map((p) => ({ value: p.name, label: p.name, sub: p.command, dir: p.default_dir }));
      label(body, 'Profile');
      picker(body, profiles, 'profile', (it) => {
        if (it.dir && !state.sheetPick.dirTouched) state.sheetPick.dir = it.dir;
      });
      const dir = field(body, 'Directory', input(state.sheetPick.dir || '', '/path/to/repo'));
      dir.addEventListener('input', () => { state.sheetPick.dir = dir.value; state.sheetPick.dirTouched = true; });
    } else if (state.sheetTab === 'skill') {
      const skills = (c.skills || []).map((s) => ({ value: s.id, label: s.name || s.id, sub: (s.harnesses || []).join('/') }));
      label(body, 'Skill');
      picker(body, skills, 'skill');
      const chosen = (c.skills || []).find((s) => s.id === state.sheetPick.skill);
      if (chosen && (chosen.harnesses || []).length > 1) {
        label(body, 'Harness');
        picker(body, chosen.harnesses.map((h) => ({ value: h, label: h })), 'harness');
      }
      const sdir = field(body, 'Directory', input(state.sheetPick.skillDir || '', 'defaults to the desktop\u2019s directory'));
      sdir.addEventListener('input', () => { state.sheetPick.skillDir = sdir.value; });
    } else if (state.sheetTab === 'loop') {
      const loops = (c.loops || []).map((lp) => ({
        value: lp.id,
        label: lp.id,
        sub: `${lp.pattern}${lp.paused ? ' · paused' : lp.enabled ? '' : ' · off'}`,
      }));
      label(body, 'Loop');
      picker(body, loops, 'loop');
      if (!loops.length) body.appendChild(hint('No loops are registered.'));
    } else {
      const wfs = (c.workflows || []).filter((w) => w.valid).map((w) => ({ value: w.name, label: w.name, sub: w.source }));
      label(body, 'Workflow');
      picker(body, wfs, 'workflow');
      const chosen = (c.workflows || []).find((w) => w.name === state.sheetPick.workflow);
      const ws = field(body, 'Workspace', input(state.sheetPick.workspace || '', '/path/to/repo'));
      ws.addEventListener('input', () => { state.sheetPick.workspace = ws.value; });
      for (const arg of (chosen && chosen.args) || []) {
        const el = field(body, arg, input((state.sheetPick.args || {})[arg] || '', ''));
        el.addEventListener('input', () => {
          state.sheetPick.args = state.sheetPick.args || {};
          state.sheetPick.args[arg] = el.value;
        });
      }
    }
  }

  function hint(text) {
    const p = document.createElement('p');
    p.className = 'hint';
    p.textContent = text;
    return p;
  }

  function submitLaunch() {
    const p = state.sheetPick;
    if (state.sheetTab === 'session') {
      if (!p.profile || !p.dir) return showBanner('Pick a profile and a directory.', 'warn', false);
      send({ t: 'launch', profile: p.profile, dir: p.dir });
    } else if (state.sheetTab === 'skill') {
      if (!p.skill) return showBanner('Pick a skill.', 'warn', false);
      send({ t: 'launch_skill', skill: p.skill, harness: p.harness, dir: p.skillDir || undefined });
    } else if (state.sheetTab === 'loop') {
      if (!p.loop) return showBanner('Pick a loop.', 'warn', false);
      send({ t: 'start_loop', loop: p.loop });
    } else {
      if (!p.workflow || !p.workspace) return showBanner('Pick a workflow and a workspace.', 'warn', false);
      send({ t: 'start_workflow', workflow: p.workflow, workspace: p.workspace, args: p.args || {} });
    }
  }

  // ------------------------------------------------------- session menu/kill

  function openSessionMenu(id) {
    const entry = state.sessions.get(id);
    if (!entry) return;
    ui.menu.hidden = false;
    const body = ui.menuBody;
    body.textContent = '';

    const title = document.createElement('p');
    title.className = 'hint';
    title.textContent = `${entry.info.name || id} — ${entry.info.cwd || ''}`;
    body.appendChild(title);

    if (state.allowKill) {
      const kill = document.createElement('button');
      kill.className = 'btn danger';
      kill.textContent = entry.info.state === 'exited' ? 'Remove' : 'Kill…';
      kill.addEventListener('click', () => confirmKill(id, entry.info.name || String(id)));
      body.appendChild(kill);
    } else {
      body.appendChild(hint('Killing is disabled on this server.'));
    }
  }

  function confirmKill(id, name) {
    const body = ui.menuBody;
    body.textContent = '';
    const q = document.createElement('p');
    q.className = 'hint';
    q.textContent = `Kill ${name}? This cannot be undone.`;
    body.appendChild(q);
    const go = document.createElement('button');
    go.className = 'btn danger';
    go.textContent = 'Kill it';
    go.addEventListener('click', () => { send({ t: 'kill', s: id }); ui.menu.hidden = true; });
    body.appendChild(go);
  }

  // --------------------------------------------------------- viewport/banner

  function bindViewport() {
    const apply = () => {
      const vv = window.visualViewport;
      document.documentElement.style.setProperty('--vvh', `${(vv ? vv.height : window.innerHeight)}px`);
      fitTerminal();
    };
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', apply);
      window.visualViewport.addEventListener('scroll', apply);
    }
    window.addEventListener('resize', apply);
    window.addEventListener('orientationchange', () => setTimeout(apply, 250));
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState !== 'visible') return;
      if (state.phase !== 'ready' && state.phase !== 'stopped') {
        state.attempts = 0;
        connect();
      } else if (state.phase === 'ready' && state.sid !== null) {
        // A background tab is throttled, so frames can have been dropped
        // while the socket stayed open. Ask for the screen rather than
        // trusting whatever partial stream arrived.
        ui.host.classList.add('stale');
        send({ t: 'resync', s: state.sid });
      }
    });
    apply();
  }

  function showBanner(text, level, sticky) {
    ui.banner.hidden = false;
    ui.banner.textContent = text;
    ui.banner.className = 'banner ' + (level || '');
    if (!sticky) setTimeout(() => { if (ui.banner.textContent === text) hideBanner(); }, 5000);
  }

  function hideBanner() { ui.banner.hidden = true; ui.banner.textContent = ''; }

  function insecureCheck() {
    const h = location.hostname;
    const local = h === 'localhost' || h === '127.0.0.1' || h === '::1' || h === '[::1]' || h.endsWith('.local');
    if (location.protocol === 'http:' && !local) {
      showBanner('Unencrypted connection. Use only on a network you trust, or over a VPN or SSH tunnel.', 'warn', true);
    }
  }

  function boot() {
    // The server turns ?token= into an HttpOnly cookie; drop it from the URL
    // so it leaves the address bar, history and any share sheet.
    if (location.search.includes('token=')) {
      history.replaceState(null, '', location.pathname);
    }
    initTerminal();
    bindKeyBar();
    bindViewport();
    ui.sheetGo.addEventListener('click', submitLaunch);
    ui.sheetCancel.addEventListener('click', closeSheet);
    ui.menuCancel.addEventListener('click', () => { ui.menu.hidden = true; });
    for (const t of ui.sheetTabs.querySelectorAll('.tab')) {
      t.addEventListener('click', () => { state.sheetTab = t.dataset.tab; state.sheetPick = {}; renderSheet(); });
    }
    ui.banner.addEventListener('click', hideBanner);
    insecureCheck();
    connect();
  }

  boot();
})();
