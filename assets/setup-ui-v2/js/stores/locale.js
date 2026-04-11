// $store.locale — i18n catalog + t() helper.

document.addEventListener('alpine:init', () => {
  Alpine.store('locale', {
    code: 'en',
    strings: {},

    init(code, strings) {
      this.code = code || 'en';
      this.strings = strings || {};
      // Expose global t() for templates: x-text="t('key', { param: 'value' })"
      window.t = (key, params) => this.t(key, params);
    },

    t(key, params) {
      const template = this.strings[key] || key;
      return window.fmt ? window.fmt.interpolate(template, params) : template;
    },

    async switch(code) {
      const catalog = await window.api.get('/api/locale/' + encodeURIComponent(code));
      this.code = code;
      this.strings = catalog;
    },
  });
});
