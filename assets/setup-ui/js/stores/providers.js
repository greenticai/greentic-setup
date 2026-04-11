document.addEventListener('alpine:init', () => {
  Alpine.store('providers', {
    loading: false,
    error: null,
    items: [],
    showAddModal: false,
    newRef: '',

    async refresh() {
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.get('/api/providers');
        this.items = data.providers || [];
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    async add() {
      const ref = this.newRef.trim();
      if (!ref) return;
      try {
        await window.api.post('/api/providers', { oci_ref: ref });
        this.newRef = '';
        this.showAddModal = false;
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
      }
    },

    async remove(ociRef) {
      const locale = Alpine.store('locale');
      if (!confirm(locale.t('ui.providers.remove_confirm'))) return;
      try {
        await window.api.delete('/api/providers', { oci_ref: ociRef });
        Alpine.store('ui').pendingMutations = true;
        await this.refresh();
      } catch (err) {
        this.error = err;
      }
    },
  });
});
