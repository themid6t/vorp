// Runs before the body paints so a stored theme never flashes the other one.
// Storage can be unavailable (private windows, blocked site data); the system
// preference is then the fallback.
(function () {
  var theme;
  try { theme = localStorage.getItem('vorp-theme'); } catch (e) { /* use the system preference */ }
  if (theme !== 'light' && theme !== 'dark') {
    theme = matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  document.documentElement.dataset.theme = theme;
})();
