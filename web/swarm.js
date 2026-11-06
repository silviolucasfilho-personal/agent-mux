// Swarms page: filled by its own task; its API lives under /api/swarm/.
(function () {
  'use strict';
  window.AgentMux.registerPage({
    id: 'swarms',
    title: 'Swarms',
    path: '/swarms',
    render(el) {
      el.appendChild(window.AgentMux.h('div', { class: 'empty', text: 'Swarms: coming soon.' }));
    },
  });
})();
