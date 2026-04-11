document.addEventListener('alpine:init', () => {
  Alpine.store('overview', {
    loading: false,
    error: null,
    stats: { scopes_count: 0, providers_count: 0, secrets_count: 0, warnings_count: 0 },
    scopes: [],

    async refresh() {
      const scope = Alpine.store('scope');
      if (!scope.tenant) return;
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.get('/api/overview?' + scope.queryString());
        this.stats = data.stats;
        this.scopes = data.scopes || [];
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },
  });
});
