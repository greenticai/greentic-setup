document.addEventListener('alpine:init', () => {
  Alpine.store('secrets', {
    loading: false,
    error: null,
    items: [],
    // Map of uri → { value, remaining, timer } for revealed values.
    // Revealed values are held here transiently and cleared after 30 seconds.
    // They are NEVER persisted to localStorage or any external store.
    revealed: {},
    editingSecret: null,
    editValue: '',
    showAddModal: false,
    newEntry: { provider_id: '', key: '', value: '' },

    async refresh() {
      const scope = Alpine.store('scope');
      if (!scope.tenant) return;
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.get('/api/secrets?' + scope.queryString());
        this.items = data.secrets || [];
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    async toggleReveal(secret) {
      const uri = secret.uri;
      if (this.revealed[uri]) {
        this.hideSecret(uri);
        return;
      }
      try {
        const scope = Alpine.store('scope');
        const data = await window.api.post('/api/secrets/reveal', {
          tenant: scope.tenant,
          env: scope.env,
          team: scope.team,
          provider_id: secret.provider_id,
          key: secret.key,
          confirmed: true,
        });
        const REVEAL_DURATION = 30;
        const entry = Alpine.reactive({ value: data.value, remaining: REVEAL_DURATION });
        this.revealed = { ...this.revealed, [uri]: entry };

        // Countdown timer — clears value after 30 seconds.
        // The raw value is held only for the countdown window and cleared on timer fire.
        const timer = setInterval(() => {
          const current = this.revealed[uri];
          if (!current) { clearInterval(timer); return; }
          current.remaining -= 1;
          if (current.remaining <= 0) {
            clearInterval(timer);
            this.hideSecret(uri);
          }
        }, 1000);
      } catch (err) {
        this.error = err;
      }
    },

    hideSecret(uri) {
      const copy = { ...this.revealed };
      delete copy[uri];
      this.revealed = copy;
    },

    beginEdit(secret) {
      this.editingSecret = secret;
      this.editValue = '';
    },

    cancelEdit() {
      this.editingSecret = null;
      this.editValue = '';
    },

    async saveEdit() {
      if (!this.editingSecret || !this.editValue) return;
      const scope = Alpine.store('scope');
      try {
        await window.api.put('/api/secrets', {
          tenant: scope.tenant,
          env: scope.env,
          team: scope.team,
          provider_id: this.editingSecret.provider_id,
          key: this.editingSecret.key,
          value: this.editValue,
        });
        // Zeroize edit value in memory before clearing reference.
        this.editValue = '\0'.repeat(this.editValue.length);
        this.editValue = '';
        this.editingSecret = null;
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
      }
    },

    async deleteSecret(secret) {
      if (!confirm(Alpine.store('locale').t('ui.secrets.delete_confirm'))) return;
      const scope = Alpine.store('scope');
      try {
        await window.api.delete('/api/secrets', {
          tenant: scope.tenant,
          env: scope.env,
          team: scope.team,
          provider_id: secret.provider_id,
          key: secret.key,
        });
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
      }
    },

    cancelAdd() {
      this.showAddModal = false;
      this.newEntry = { provider_id: '', key: '', value: '' };
    },

    async addSecret() {
      if (!this.newEntry.key || !this.newEntry.value) return;
      const scope = Alpine.store('scope');
      try {
        await window.api.post('/api/secrets', {
          tenant: scope.tenant,
          env: scope.env,
          team: scope.team,
          provider_id: this.newEntry.provider_id,
          key: this.newEntry.key,
          value: this.newEntry.value,
        });
        // Zeroize value before clearing.
        this.newEntry.value = '\0'.repeat(this.newEntry.value.length);
        this.newEntry = { provider_id: '', key: '', value: '' };
        this.showAddModal = false;
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
      }
    },
  });
});
