document.addEventListener('alpine:init', () => {
  Alpine.store('scopeForm', {
    loading: false,
    saving: false,
    error: null,
    saveError: null,
    providers: [],        // [{ id, display_name, form_spec, current_values, question_extras }]
    answers: {},          // { provider_id: { field_key: value } }
    fieldErrors: {},      // { provider_id: { field_key: error_key } }
    questionExtras: {},   // { provider_id: { question_id: { placeholder, docs_url, group } } }
    currentStep: 0,       // 0-indexed; maps to providers[currentStep], or review if === providers.length
    manualSteps: [],      // [{ provider_name, steps }] — populated after save when manual portal steps are needed

    get totalSteps() {
      return this.providers.length + 1; // +1 for review step
    },

    isReviewStep() {
      return this.currentStep === this.providers.length;
    },

    currentProvider() {
      return this.providers[this.currentStep] || null;
    },

    reset() {
      this.currentStep = 0;
      this.saveError = null;
      this.fieldErrors = {};
      this.questionExtras = {};
      this.manualSteps = [];
    },

    async refresh() {
      this.loading = true;
      this.error = null;
      const scope = Alpine.store('scope');
      try {
        const data = await window.api.get(
          '/api/scope/form?tenant=' + encodeURIComponent(scope.tenant) +
          '&env=' + encodeURIComponent(scope.env) +
          '&team=' + encodeURIComponent(scope.team)
        );
        this.providers = data.providers || [];
        // Initialize answers from current_values for each provider.
        const init = {};
        const extras = {};
        for (const p of this.providers) {
          init[p.id] = { ...(p.current_values || {}) };
          extras[p.id] = p.question_extras || {};
        }
        this.answers = init;
        this.questionExtras = extras;
        this.fieldErrors = {};
        this.currentStep = 0;
      } catch (err) {
        this.error = err;
      } finally {
        this.loading = false;
      }
    },

    /// Get a single extra metadata field for a question.
    /// Returns null when extras are absent for that provider or question.
    getExtra(providerId, questionId, field) {
      const pe = this.questionExtras[providerId];
      if (!pe) return null;
      const qe = pe[questionId];
      if (!qe) return null;
      return qe[field] || null;
    },

    fieldValue(providerId, fieldKey) {
      return (this.answers[providerId] || {})[fieldKey] || '';
    },

    setFieldValue(providerId, fieldKey, value) {
      if (!this.answers[providerId]) this.answers[providerId] = {};
      this.answers[providerId][fieldKey] = value;
      // Clear field-level error on edit.
      if (this.fieldErrors[providerId] && this.fieldErrors[providerId][fieldKey]) {
        delete this.fieldErrors[providerId][fieldKey];
      }
    },

    /// Evaluate whether a question should be visible based on its visible_if expression.
    /// Returns true if the field should be shown.
    ///
    /// Supports two formats:
    ///   "field_name==value"  → show if answers[field_name] === value
    ///   "field_name"         → show if answers[field_name] is truthy (non-empty, non-null, non-"false")
    isVisible(providerId, questionId) {
      const vis = this.getExtra(providerId, questionId, 'visible_if');
      if (!vis) return true; // No condition → always visible.

      const pa = this.answers[providerId] || {};

      if (vis.includes('==')) {
        const [field, expected] = vis.split('==', 2);
        const actual = String(pa[field] || '');
        return actual === expected;
      }

      // Truthy check: non-empty, non-null, not literally "false".
      const val = pa[vis];
      if (val === undefined || val === null || val === '' || val === 'false' || val === false) {
        return false;
      }
      return true;
    },

    /// Display a value for the review step: masked if secret, raw if not.
    displayValue(providerId, fieldKey, isSecret) {
      const v = this.fieldValue(providerId, fieldKey);
      if (v === undefined || v === null || v === '') return '(empty)';
      const s = String(v);
      if (isSecret) {
        if (s.length <= 4) return '••••';
        return '••••' + s.slice(-4);
      }
      return s;
    },

    /// Whether the user can advance to the next step (required fields filled).
    canAdvance() {
      if (this.isReviewStep()) return this.canSave();
      const provider = this.currentProvider();
      if (!provider) return false;
      const pa = this.answers[provider.id] || {};
      for (const q of (provider.form_spec && provider.form_spec.questions) || []) {
        if (!q.required) continue;
        if (!this.isVisible(provider.id, q.id)) continue; // Skip hidden fields.
        const v = pa[q.id];
        if (v === undefined || v === null) return false;
        if (typeof v === 'string' && v.trim() === '') return false;
      }
      return true;
    },

    nextStep() {
      if (this.canAdvance() && this.currentStep < this.providers.length) {
        this.currentStep++;
      }
    },

    prevStep() {
      if (this.currentStep > 0) {
        this.currentStep--;
      }
    },

    async save() {
      this.saving = true;
      this.saveError = null;
      this.fieldErrors = {};
      const scope = Alpine.store('scope');
      try {
        const data = await window.api.post('/api/scope/form', {
          scope: { tenant: scope.tenant, env: scope.env, team: scope.team },
          by_provider: this.answers,
        });
        // Store any manual post-setup instructions returned by the backend.
        this.manualSteps = data.manual_steps || [];
        // Refresh overview to reflect new state.
        if (Alpine.store('overview')) {
          await Alpine.store('overview').refresh();
        }
        console.info('[scopeForm] saved', data);
        return data;
      } catch (err) {
        this.saveError = err;
        // If err.fields is populated, distribute to fieldErrors by provider.
        if (err && err.fields && typeof err.fields === 'object') {
          for (const k of Object.keys(err.fields)) {
            // Field keys are formatted as "provider_id.field_key".
            const dotIdx = k.indexOf('.');
            if (dotIdx > -1) {
              const pid = k.substring(0, dotIdx);
              const fk = k.substring(dotIdx + 1);
              if (!this.fieldErrors[pid]) this.fieldErrors[pid] = {};
              this.fieldErrors[pid][fk] = err.fields[k];
            }
          }
        }
      } finally {
        this.saving = false;
      }
    },

    /// Whether all required fields across all providers have non-empty values.
    canSave() {
      for (const provider of this.providers) {
        const pa = this.answers[provider.id] || {};
        const questions = (provider.form_spec && provider.form_spec.questions) || [];
        for (const q of questions) {
          if (!q.required) continue;
          if (!this.isVisible(provider.id, q.id)) continue; // Skip hidden fields.
          const v = pa[q.id];
          if (v === undefined || v === null) return false;
          if (typeof v === 'string' && v.trim() === '') return false;
        }
      }
      return true;
    },
  });
});
