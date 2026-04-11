document.addEventListener('alpine:init', () => {
  Alpine.store('capabilities', {
    loading: false,
    error: null,
    items: [],

    async refresh() {
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.get('/api/capabilities');
        this.items = data.capabilities || [];
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    async toggle(id, enabled) {
      try {
        await window.api.put('/api/capabilities/toggle', { id, enabled });
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
        await this.refresh(); // Reset checkbox to server state on error.
      }
    },
  });
});
