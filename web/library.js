// Library area: configuration library (kinds, items, item detail) and agents registry.
(function () {
  'use strict';
  const M = window.AgentMux;
  const { h, link, table, kv, loading, errorBox, shortPath } = M;

  async function fetchJson(path) {
    try {
      const res = await fetch(path, { headers: { accept: 'application/json' } });
      if (!res.ok) {
        let msg = 'HTTP ' + res.status;
        try {
          const errObj = await res.json();
          if (errObj && errObj.error) msg = errObj.error;
        } catch (_) {}
        throw new Error(msg);
      }
      return await res.json();
    } catch (err) {
      if (M && M.api) {
        return await M.api(path);
      }
      throw err;
    }
  }

  function sourceBadge(source) {
    let cls = 'badge';
    let text = source || 'unknown';
    if (source === 'override') {
      cls = 'badge working';
      text = 'override';
    } else if (source === 'builtin') {
      text = 'built-in';
    } else if (source === 'user') {
      cls = 'badge ok';
      text = 'user';
    }
    return h('span', { class: cls, text });
  }

  function fmtSize(bytes) {
    if (bytes === null || bytes === undefined) return '–';
    if (bytes >= 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    if (bytes >= 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return bytes + ' B';
  }

  // ---- Library (#/library) -------------------------------------------------

  M.registerPage({
    id: 'library',
    title: 'Library',
    path: '/library',
    async render(el, params) {
      const selectedKind = params.kind || '';
      const searchQuery = (params.search || '').trim().toLowerCase();

      const form = h(
        'form',
        { class: 'filters' },
        h(
          'label',
          null,
          'Kind',
          h(
            'select',
            { name: 'kind', id: 'library-kind-select' },
            h('option', { value: '', selected: !selectedKind || null }, 'All kinds')
          )
        ),
        h(
          'label',
          null,
          'Search',
          h('input', {
            type: 'search',
            name: 'search',
            value: params.search || '',
            placeholder: 'Name or path fragment',
          })
        ),
        h('button', { type: 'submit', class: 'primary' }, 'Apply'),
        h('button', { type: 'button', onclick: () => M.navigate('/library') }, 'Clear')
      );

      form.addEventListener('submit', (e) => {
        e.preventDefault();
        const fd = new FormData(form);
        M.navigate('/library', {
          kind: fd.get('kind'),
          search: fd.get('search'),
        });
      });

      el.appendChild(form);
      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading('Loading configuration items…'));

      let kindsData = null;
      let itemsData = null;

      try {
        const [kindsRes, itemsRes] = await Promise.all([
          fetchJson('/api/library/kinds').catch(() => ({ kinds: [] })),
          fetchJson(
            '/api/library/items' + (selectedKind ? '?kind=' + encodeURIComponent(selectedKind) : '')
          ),
        ]);
        kindsData = kindsRes.kinds || kindsRes || [];
        itemsData = itemsRes.items || itemsRes || [];
      } catch (err) {
        out.innerHTML = '';
        out.appendChild(errorBox(err.message || String(err)));
        return;
      }

      // Populate kind select dropdown options
      const selectEl = form.querySelector('#library-kind-select');
      if (selectEl && kindsData.length) {
        selectEl.innerHTML = '';
        selectEl.appendChild(
          h('option', { value: '', selected: !selectedKind || null }, 'All kinds')
        );
        for (const k of kindsData) {
          const opt = h(
            'option',
            { value: k.label, selected: selectedKind === k.label || null },
            k.title || k.label
          );
          selectEl.appendChild(opt);
        }
      }

      out.innerHTML = '';

      let items = itemsData;
      if (searchQuery) {
        items = items.filter(
          (item) =>
            (item.name || '').toLowerCase().includes(searchQuery) ||
            (item.id || '').toLowerCase().includes(searchQuery) ||
            (item.path || '').toLowerCase().includes(searchQuery)
        );
      }

      if (!items.length) {
        out.appendChild(
          h('div', { class: 'empty', text: 'No configuration items found matching these filters.' })
        );
        return;
      }

      // Group items by kind
      const groups = new Map();
      for (const item of items) {
        const k = item.kind || 'other';
        if (!groups.has(k)) {
          groups.set(k, {
            title: item.kind_title || k,
            items: [],
          });
        }
        groups.get(k).items.push(item);
      }

      out.appendChild(
        h(
          'div',
          { class: 'muted', style: 'margin-bottom: 16px;' },
          items.length + ' item' + (items.length === 1 ? '' : 's') + ' in configuration library'
        )
      );

      for (const [kindKey, group] of groups.entries()) {
        const section = h('div', { class: 'library-kind-section', style: 'margin-bottom: 24px;' });
        section.appendChild(
          h('h2', null, group.title + ' (' + group.items.length + ')')
        );

        const tbl = table({
          sortKey: 'name',
          sortDesc: false,
          empty: 'No items in this section.',
          columns: [
            {
              key: 'name',
              label: 'Name',
              render: (r) => link('/library/' + encodeURIComponent(r.id), r.name || r.id),
            },
            {
              key: 'source',
              label: 'Source',
              render: (r) => sourceBadge(r.source),
            },
            {
              key: 'path',
              label: 'Path',
              class: 'ellipsis',
              title: (r) => r.path,
              render: (r) => shortPath(r.path),
            },
            {
              key: 'size',
              label: 'Size',
              num: true,
              render: (r) => fmtSize(r.size),
            },
            {
              key: 'status',
              label: 'Status',
              render: (r) =>
                r.valid
                  ? h('span', { class: 'muted', text: 'valid' })
                  : h('span', {
                      class: 'badge error',
                      title: (r.problems || []).join('; '),
                      text: 'issues',
                    }),
            },
          ],
          rows: group.items,
        });

        section.appendChild(tbl);
        out.appendChild(section);
      }
    },
  });

  // ---- Item detail (#/library/<id>) ----------------------------------------

  M.registerPage({
    id: 'library-item',
    title: (params) => (params.id ? 'Item: ' + params.id : 'Library Item'),
    path: '/library/:id',
    nav: false,
    navId: 'library',
    async render(el, params) {
      const id = params.id;
      if (!id) {
        el.appendChild(errorBox('No item id specified'));
        return;
      }

      el.appendChild(
        h(
          'div',
          { class: 'toolbar' },
          link('/library', '← Back to Library')
        )
      );

      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading('Loading asset ' + id + '…'));

      let item = null;
      try {
        item = await fetchJson('/api/library/items/' + encodeURIComponent(id));
      } catch (err) {
        out.innerHTML = '';
        out.appendChild(errorBox(err.message || String(err)));
        return;
      }

      out.innerHTML = '';

      const kvPairs = [
        ['ID', item.id],
        ['Name', item.name],
        ['Kind', item.kind_title ? item.kind_title + ' (' + item.kind + ')' : item.kind],
        ['Source', sourceBadge(item.source)],
        [
          'Shadows built-in',
          item.shadows_builtin ? 'Yes (library override shadows compiled-in)' : 'No',
        ],
        ['Path', item.path],
        ['Override path', item.override_path || item.path],
        ['Size', fmtSize(item.size)],
        ['Status', item.valid ? 'Valid' : (item.problems || []).join('; ')],
      ];

      if (item.placeholders && item.placeholders.length) {
        kvPairs.push(['Placeholders', item.placeholders.join(', ')]);
      }
      if (item.repo_path) {
        kvPairs.push(['Compiled-in repo path', item.repo_path]);
      }

      out.appendChild(kv(kvPairs));

      out.appendChild(h('h2', { style: 'margin-top: 20px;' }, 'Effective text'));
      out.appendChild(
        h('pre', { class: 'block', style: 'max-height: 40em; overflow: auto;' }, item.text || '')
      );
    },
  });

  // ---- Agents page (#/agents) ----------------------------------------------

  M.registerPage({
    id: 'agents',
    title: 'Agents',
    path: '/agents',
    async render(el) {
      el.appendChild(
        h(
          'div',
          { class: 'muted', style: 'margin-bottom: 16px;' },
          'Workflow personas and scheduled task agents from the registry.'
        )
      );

      const out = h('div');
      el.appendChild(out);
      out.appendChild(loading('Loading agents registry…'));

      let data = null;
      try {
        data = await fetchJson('/api/library/agents');
      } catch (err) {
        out.innerHTML = '';
        out.appendChild(errorBox(err.message || String(err)));
        return;
      }

      out.innerHTML = '';
      const agents = data.agents || [];

      if (!agents.length) {
        out.appendChild(h('div', { class: 'empty', text: 'No agents found in registry.' }));
        return;
      }

      out.appendChild(
        table({
          sortKey: 'name',
          sortDesc: false,
          empty: 'No agents registered.',
          columns: [
            {
              key: 'name',
              label: 'Name',
              render: (r) => {
                const itemPath = 'agents/' + r.name + '.toml';
                return link('/library/' + encodeURIComponent(itemPath), r.name);
              },
            },
            {
              key: 'description',
              label: 'Description',
              class: 'wrap',
            },
            {
              key: 'tools',
              label: 'Tools',
              render: (r) => r.tools_label || (r.tools && r.tools.join(', ')) || 'all',
            },
            {
              key: 'model',
              label: 'Model',
              render: (r) => r.model || '–',
            },
            {
              key: 'task_kind',
              label: 'Task Kind',
              render: (r) => r.task_kind || '–',
            },
            {
              key: 'schedule',
              label: 'Schedule',
              render: (r) => r.schedule || '–',
            },
            {
              key: 'source',
              label: 'Source',
              render: (r) => sourceBadge(r.source),
            },
          ],
          rows: agents,
        })
      );
    },
  });
})();
