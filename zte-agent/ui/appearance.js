'use strict';
// Runs before CSS so a saved dark preference is applied before the first paint.
(() => {
  const storageKey = 'openu60.appearance';
  const system = matchMedia('(prefers-color-scheme: dark)');
  const valid = value => ['system','light','dark'].includes(value);
  let preference = 'system', persistent = true;
  try {
    const saved = localStorage.getItem(storageKey);
    if (valid(saved)) preference = saved;
  } catch { persistent = false; }

  function apply() {
    const resolved = preference === 'system' ? (system.matches ? 'dark' : 'light') : preference;
    document.documentElement.dataset.theme = resolved;
    document.documentElement.dataset.appearance = preference;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.content = resolved === 'dark' ? '#111821' : '#f3f5f8';
    for (const input of document.querySelectorAll('input[name="appearance"]')) input.checked = input.value === preference;
    const status = document.querySelector('#appearance-status');
    if (status) status.textContent = preference === 'system'
      ? `Following your device appearance · ${resolved === 'dark' ? 'Dark' : 'Light'}`
      : `${resolved === 'dark' ? 'Dark' : 'Light'} appearance${persistent ? ' · Saved in this browser' : ' · Applied in this tab'}`;
  }
  function choose(value) {
    if (!valid(value)) return;
    preference = value;
    try { localStorage.setItem(storageKey,value); persistent = true; }
    catch { persistent = false; }
    apply();
  }
  system.addEventListener('change',() => { if (preference === 'system') apply(); });
  window.addEventListener('storage',event => {
    if (event.key === storageKey || event.key === null) {
      preference = valid(event.newValue) ? event.newValue : 'system'; apply();
    }
  });
  document.addEventListener('DOMContentLoaded',() => {
    for (const input of document.querySelectorAll('input[name="appearance"]')) {
      input.addEventListener('change',() => { if (input.checked) choose(input.value); });
    }
    apply();
  });
  apply();
})();
