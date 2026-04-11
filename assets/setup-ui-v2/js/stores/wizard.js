document.addEventListener('alpine:init', () => {
  Alpine.store('wizard', {
    sessionId: null,
    scope: null,
    provider: null,
    currentStep: 1,
    totalSteps: 0,
    step: null,
    answers: {},
    loading: false,
    error: null,

    async start(tenant, env, team, provider) {
      this.loading = true;
      this.error = null;
      const query = new URLSearchParams({ tenant, env, team });
      if (provider) query.set('provider', provider);
      try {
        const data = await window.api.get('/api/wizard/start?' + query);
        this._apply(data);
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    async next(stepAnswers) {
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.post('/api/wizard/next', {
          session_id: this.sessionId,
          answers: stepAnswers || {},
        });
        this._apply(data);
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    async execute() {
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.post('/api/wizard/execute', {
          session_id: this.sessionId,
        });
        // Reset state on success.
        this.sessionId = null;
        this.step = null;
        this.answers = {};
        return data;
      } catch (err) {
        this.error = err;
        throw err;
      } finally {
        this.loading = false;
      }
    },

    _apply(data) {
      this.sessionId = data.id;
      this.scope = data.scope;
      this.provider = data.provider;
      this.currentStep = data.current_step;
      this.totalSteps = data.total_steps;
      this.step = data.step;
    },
  });
});
