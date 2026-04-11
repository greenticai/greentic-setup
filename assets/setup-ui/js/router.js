// Minimal hash-based router. Routes:
//   #/                                 → overview
//   #/wizard/new?scope=t:e:m            → wizard for new scope
//   #/wizard/provider?scope=...&provider=X
//
// Stores parse their own query params from location.hash.

(function() {
  function current() {
    const hash = window.location.hash || '#/';
    const [path, query] = hash.slice(1).split('?');
    const params = {};
    if (query) {
      query.split('&').forEach(pair => {
        const [k, v] = pair.split('=');
        params[decodeURIComponent(k)] = decodeURIComponent(v || '');
      });
    }
    return { path, params };
  }

  function navigate(path, params) {
    const query = params
      ? '?' + Object.entries(params)
          .map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v))
          .join('&')
      : '';
    window.location.hash = path + query;
  }

  window.router = { current, navigate };
})();
