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

    async start(tenant, env, team, provider, prefill) {
      this.loading = true;
      this.error = null;
      // Seed the local answers map with any prefill values from --answers.
      // The server still validates on submit; prefill is purely a UX win.
      this.answers = prefill && typeof prefill === 'object' ? { ...prefill } : {};
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
      // Guard: refuse to advance if any required field is empty. The
      // server also validates but a client-side check avoids a round-trip
      // and gives instant feedback.
      if (!this.canContinue()) {
        this.error = {
          code: 'wizard.required_missing',
          key: 'ui.error.required_fields_missing',
        };
        return;
      }
      this.loading = true;
      this.error = null;
      try {
        const data = await window.api.post('/api/wizard/next', {
          session_id: this.sessionId,
          answers: stepAnswers || this.answers || {},
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

    /// Check whether every required field on the current step has a
    /// non-empty value. Returns true if the user can proceed to the next
    /// step (or execute if this is the last step).
    canContinue() {
      if (!this.step || !Array.isArray(this.step.fields)) return true;
      for (const field of this.step.fields) {
        if (!field.required) continue;
        const value = this.answers ? this.answers[field.name] : undefined;
        if (value === undefined || value === null) return false;
        if (typeof value === 'string' && value.trim() === '') return false;
      }
      return true;
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
