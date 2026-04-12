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
    // Add-scope modal state.
    showAddScopeModal: false,
    newScope: { tenant: '', env: '', team: '' },

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

      // Initialize scope from CLI-provided initial scope.
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

      // Initial view chosen by the server:
      // - "configure" when no scopes are configured or when --answers was provided
      // - "overview" otherwise
      // Configure is no longer a direct nav target; enter it via openConfigure().
      if (initial.view === 'configure') {
        // Use the CLI-seeded scope directly.
        this.openConfigure(null);
      } else {
        this.currentView = 'overview';
        this.breadcrumb = 'Overview';
        if (Alpine.store('overview')) {
          Promise.resolve().then(() => Alpine.store('overview').refresh());
        }
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

    /// Open the Configure view for a given scope object { tenant, env, team }.
    /// Pass null to use the current scope store state unchanged.
    openConfigure(scope) {
      if (scope) {
        Alpine.store('scope').set(scope.tenant, scope.env, scope.team);
      }
      this.currentView = 'configure';
      this.breadcrumb = 'Configure';
      if (Alpine.store('scopeForm')) {
        Alpine.store('scopeForm').reset();
        Alpine.store('scopeForm').refresh();
      }
    },

    /// Confirm adding a new scope from the modal; navigate to Configure.
    confirmAddScope() {
      const s = this.newScope;
      if (!s.tenant.trim() || !s.env.trim() || !s.team.trim()) return;
      this.showAddScopeModal = false;
      this.newScope = { tenant: '', env: '', team: '' };
      this.openConfigure({ tenant: s.tenant.trim(), env: s.env.trim(), team: s.team.trim() });
    },

    navigate(view) {
      this.currentView = view;
      this.breadcrumb = view.charAt(0).toUpperCase() + view.slice(1);
      // Lazy-load the relevant store data.
      // Configure is not a direct nav target; enter via openConfigure().
      const storeMap = {
        overview: 'overview',
        providers: 'providers',
        capabilities: 'capabilities',
      };
      const storeName = storeMap[view];
      if (storeName && Alpine.store(storeName) && Alpine.store(storeName).refresh) {
        Alpine.store(storeName).refresh();
      }
    },

    /// Export scope answers as a downloadable JSON file.
    async exportScope(scope) {
      try {
        const data = await window.api.post('/api/scope/export', { scope });
        const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `${scope.tenant}-${scope.env}-${scope.team}-answers.json`;
        a.click();
        URL.revokeObjectURL(url);
      } catch (err) {
        const msg = Alpine.store('locale')
          ? Alpine.store('locale').t('ui.error.unknown')
          : 'Export failed';
        console.error('[ui] exportScope failed', err);
        alert(msg);
      }
    },

    /// Import scope answers from a JSON file and persist via /api/scope/form.
    async importScope(event) {
      const file = event.target.files[0];
      if (!file) return;
      let text;
      try {
        text = await file.text();
      } catch (e) {
        console.error('[ui] importScope: failed to read file', e);
        return;
      }
      let data;
      try {
        data = JSON.parse(text);
      } catch (e) {
        const msg = Alpine.store('locale')
          ? Alpine.store('locale').t('ui.error.import_invalid_json')
          : 'Invalid JSON file. Please select a valid answers file.';
        alert(msg);
        event.target.value = '';
        return;
      }
      const scope = {
        tenant: data.tenant || 'default',
        env: data.env || 'dev',
        team: data.team || 'default',
      };
      try {
        await window.api.post('/api/scope/form', {
          scope,
          by_provider: data.setup_answers || data.by_provider || {},
        });
      } catch (err) {
        const msg = Alpine.store('locale')
          ? Alpine.store('locale').t('ui.error.execute_failed')
          : 'Import failed';
        console.error('[ui] importScope: persist failed', err);
        alert(msg);
      }
      event.target.value = '';
      if (Alpine.store('overview')) Alpine.store('overview').refresh();
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
