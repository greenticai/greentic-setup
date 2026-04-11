document.addEventListener('alpine:init', () => {
  Alpine.store('ui', {
    advancedMode: false,
    currentView: 'overview',
    breadcrumb: 'Overview',
    port: 0,
    availableLocales: ['en'],
    scopeFromCli: false,
    // Show the first-run onboarding banner until the user dismisses it.
    // Kept in-memory only (no localStorage per security rule) so a new
    // `greentic-setup` invocation re-shows it.
    onboardingDismissed: false,
    // Whether any mutation has occurred since the last successful rebuild.
    pendingMutations: false,

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
      this.availableLocales = initial.available_locales || ['en'];
      this.scopeFromCli = !!initial.scope_from_cli;
      this.advancedMode = !!initial.advanced;

      // Boot the API client with the bearer token.
      if (window.api && initial.bearer_token) {
        window.api.boot(initial.bearer_token);
      }

      // Initialize the locale catalog (server already filtered to ui.*).
      if (Alpine.store('locale')) {
        Alpine.store('locale').init(initial.locale || 'en', initial.strings || {});
      }

      // Initialize scope. Prefer the CLI-provided initial scope, then the
      // first value in each allow-list as fallback.
      const bundle = initial.bundle || {};
      const initialScope = initial.initial_scope || {};
      if (Alpine.store('scope')) {
        Alpine.store('scope').init(
          initialScope.tenant || bundle.available_tenants?.[0] || '',
          initialScope.env || bundle.available_envs?.[0] || '',
          initialScope.team || bundle.available_teams?.[0] || ''
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

      // Initial view is chosen by the server: "wizard" when no scopes are
      // configured or when --answers was provided, else "overview".
      this.currentView = initial.view === 'wizard' ? 'wizard' : 'overview';
      this.breadcrumb = this.currentView === 'wizard' ? 'Configure scope' : 'Overview';

      // If we're booting into the wizard, kick off a session immediately
      // with the CLI-seeded scope and any prefill answers.
      if (this.currentView === 'wizard' && Alpine.store('wizard')) {
        const scope = Alpine.store('scope');
        const prefill = initial.prefill_answers || null;
        // Run on next tick so Alpine has finished registering stores.
        Promise.resolve().then(() => {
          Alpine.store('wizard').start(
            scope.tenant,
            scope.env,
            scope.team,
            null,
            prefill
          );
        });
      } else if (Alpine.store('overview')) {
        // On overview, refresh the stats immediately.
        Promise.resolve().then(() => {
          Alpine.store('overview').refresh();
        });
      }

      // Poll pending state from server.
      Promise.resolve().then(() => this._pollPending());
    },

    async _pollPending() {
      try {
        const data = await window.api.get('/api/rebuild/pending');
        this.pendingMutations = !!data.pending;
      } catch (_) {
        // Non-fatal: pending badge stays in last-known state.
      }
    },

    toggleAdvanced() {
      this.advancedMode = !this.advancedMode;
    },

    dismissOnboarding() {
      this.onboardingDismissed = true;
    },

    navigate(view) {
      this.currentView = view;
      // Lazy-load the relevant store data.
      const storeMap = {
        overview: 'overview',
        providers: 'providers',
        secrets: 'secrets',
        capabilities: 'capabilities',
      };
      const storeName = storeMap[view];
      if (storeName && Alpine.store(storeName) && Alpine.store(storeName).refresh) {
        Alpine.store(storeName).refresh();
      }
    },

    async triggerRebuild() {
      if (!this.pendingMutations) return;
      try {
        const data = await window.api.post('/api/rebuild', {});
        this.pendingMutations = false;
        if (Alpine.store('overview')) {
          Alpine.store('overview').refresh();
        }
        // Brief toast — we use a simple alert for Phase 1b; a proper toast
        // component is planned for Phase 2.
        const msg = Alpine.store('locale')
          ? Alpine.store('locale').t('ui.topbar.rebuild_success')
          : 'Rebuild successful';
        console.info(msg, data);
      } catch (err) {
        const msg = Alpine.store('locale')
          ? Alpine.store('locale').t('ui.topbar.rebuild_failed')
          : 'Rebuild failed';
        console.error(msg, err);
        alert(msg);
      }
    },
  });
});
