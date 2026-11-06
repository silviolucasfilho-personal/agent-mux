// Runs page: filled by its own task; its API lives under /api/runs/.
(function () {
  'use strict';
  window.AgentMux.registerPage({
    id: 'runs',
    title: 'Runs',
    path: '/runs',
    render(el) {
      el.appendChild(window.AgentMux.h('div', { class: 'empty', text: 'Runs: coming soon.' }));
    },
  });
})();
