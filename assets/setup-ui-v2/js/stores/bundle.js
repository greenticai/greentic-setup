document.addEventListener('alpine:init', () => {
  Alpine.store('bundle', {
    id: '',
    displayName: '',
    availableTenants: [],
    availableEnvs: [],
    availableTeams: [],

    async refresh() {
      const data = await window.api.get('/api/bundle');
      this.id = data.id;
      this.displayName = data.display_name;
      this.availableTenants = data.available_tenants || [];
      this.availableEnvs = data.available_envs || [];
      this.availableTeams = data.available_teams || [];
    },
  });
});
