document.addEventListener('alpine:init', () => {
  Alpine.store('scope', {
    tenant: '',
    env: '',
    team: '',

    init(initialTenant, initialEnv, initialTeam) {
      this.tenant = initialTenant || '';
      this.env = initialEnv || '';
      this.team = initialTeam || '';
    },

    set(tenant, env, team) {
      this.tenant = tenant;
      this.env = env;
      this.team = team;
      // Tell the overview store to refresh for the new scope.
      if (Alpine.store('overview')) {
        Alpine.store('overview').refresh();
      }
    },

    queryString() {
      return 'tenant=' + encodeURIComponent(this.tenant) +
        '&env=' + encodeURIComponent(this.env) +
        '&team=' + encodeURIComponent(this.team);
    },
  });
});
