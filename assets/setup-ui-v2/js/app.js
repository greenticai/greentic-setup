// App boot entry point. Runs after all stores are registered.
// Alpine's `alpine:init` fires before DOMContentLoaded, so by the time
// this file loads, the stores are already defined.

document.addEventListener('DOMContentLoaded', async () => {
  // Refresh overview data on initial load (if scope is set).
  const scope = Alpine.store('scope');
  if (scope && scope.tenant) {
    await Alpine.store('overview').refresh();
  }

  // Hash router — wire listener for future component use (Tasks 26-29).
  window.addEventListener('hashchange', () => {
    const { path } = window.router.current();
    Alpine.store('ui').currentView =
      path === '/' ? 'overview' :
      path.startsWith('/wizard/') ? 'wizard' :
      'overview';
  });
});
