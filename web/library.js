// Library page: filled by its own task; its API lives under /api/library/.
(function () {
  'use strict';
  window.AgentMux.registerPage({
    id: 'library',
    title: 'Library',
    path: '/library',
    render(el) {
      el.appendChild(window.AgentMux.h('div', { class: 'empty', text: 'Library: coming soon.' }));
    },
  });
})();
