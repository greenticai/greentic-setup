document.addEventListener('alpine:init', () => {
  Alpine.store('ui', {
    advancedMode: false,
    currentView: 'overview',
    breadcrumb: 'Overview',
    port: 0,

    init() {
      // Read initial state injected by the server.
      const el = document.getElementById('initial-state');
      if (!el) return;
      let initial;
      try {
        initial = JSON.parse(el.textContent || '{}');
      } catch (e) {
        console.error('invalid initial-state json', e);
        return;
      }

      this.port = initial.port || 0;

      // Boot the API client with the bearer token.
      if (window.api && initial.bearer_token) {
        window.api.boot(initial.bearer_token);
      }

      // Initialize the locale catalog.
      if (Alpine.store('locale')) {
        Alpine.store('locale').init(initial.locale || 'en', initial.strings || {});
      }

      // Initialize scope to first available.
      const bundle = initial.bundle || {};
      if (Alpine.store('scope') && bundle.available_tenants) {
        Alpine.store('scope').init(
          bundle.available_tenants[0] || '',
          bundle.available_envs?.[0] || '',
          bundle.available_teams?.[0] || ''
        );
      }

      // Populate bundle store from initial state.
      if (Alpine.store('bundle')) {
        Alpine.store('bundle').id = bundle.id || '';
        Alpine.store('bundle').displayName = bundle.display_name || '';
        Alpine.store('bundle').availableTenants = bundle.available_tenants || [];
        Alpine.store('bundle').availableEnvs = bundle.available_envs || [];
        Alpine.store('bundle').availableTeams = bundle.available_teams || [];
      }
    },

    toggleAdvanced() {
      this.advancedMode = !this.advancedMode;
    },
  });
});
