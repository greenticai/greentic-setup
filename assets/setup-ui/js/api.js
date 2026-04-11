// Fetch helper — attaches bearer token, sets Origin-safe headers,
// handles error envelope.

(function() {
  let bearerToken = null;

  function boot(token) {
    bearerToken = token;
  }

  async function request(method, url, body) {
    const headers = {
      'accept': 'application/json',
      // Custom header that cross-origin scripts cannot set without a CORS
      // preflight. The server rejects any /api/* request that lacks it,
      // giving us CSRF protection that is independent of Origin/Referer
      // header behavior (which varies by browser and request mode).
      'x-requested-with': 'GreenticSetupDashboard',
    };
    if (bearerToken) {
      headers['authorization'] = 'Bearer ' + bearerToken;
    }
    const init = {
      method,
      headers,
      // same-origin prevents the browser from sending the request to any
      // other origin even if the URL is crafted malformed.
      credentials: 'same-origin',
    };
    if (body !== undefined) {
      headers['content-type'] = 'application/json';
      init.body = JSON.stringify(body);
    }
    const resp = await fetch(url, init);
    const contentType = resp.headers.get('content-type') || '';
    let data = null;
    if (contentType.includes('application/json')) {
      data = await resp.json();
    }
    if (!resp.ok) {
      const error = (data && data.error) || { code: 'unknown', key: 'ui.error.unknown' };
      throw { status: resp.status, ...error };
    }
    return data;
  }

  window.api = {
    boot,
    get: (url) => request('GET', url),
    post: (url, body) => request('POST', url, body),
    put: (url, body) => request('PUT', url, body),
    delete: (url, body) => request('DELETE', url, body),
  };
})();
