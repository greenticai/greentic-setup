// Display formatters used across components. Pure functions, no state.

window.fmt = {
  // Mask a secret string, keeping only last N characters.
  mask(value, keep = 4) {
    if (typeof value !== 'string' || value.length <= keep) return '••••';
    return '••••' + value.slice(-keep);
  },

  // Format a count with ICU plural forms.
  count(n, singularKey, pluralKey) {
    return (n === 1 ? singularKey : pluralKey).replace('#', n);
  },

  // Relative time ("5 min ago"). Accepts a Unix ms timestamp.
  relativeTime(ts) {
    const diff = Date.now() - ts;
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return mins + ' min ago';
    const hours = Math.floor(mins / 60);
    if (hours < 24) return hours + ' hr ago';
    return Math.floor(hours / 24) + ' day ago';
  },
};

// ICU-lite param substitution: "{name} has {count} items" + { name: 'x', count: 3 }
// Does NOT implement full ICU plural/select — that's added in Task 31 if needed.
window.fmt.interpolate = function(template, params) {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (_, key) =>
    params[key] !== undefined ? String(params[key]) : '{' + key + '}'
  );
};
