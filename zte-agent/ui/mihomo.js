'use strict';
(() => {
  const route = 'network/mihomo', UI = OpenU60UI, API = OpenU60Console, M = OpenU60Mihomo;
  const {el, button, request, json, confirm, mutate, say} = UI;
  const MINUTE = 60000;

  // Zashboard opens in its own tab. The tab is opened before the request so the
  // click still counts as a user gesture; the pass cookie arrives with the reply.
  async function openDashboard(node, trigger) {
    if (API.locked || !API.connected) { say(node, 'Connect and unlock the console first.', true); return; }
    const epoch = UI.epoch;
    const tab = window.open('', '_blank');
    if (tab) tab.opener = null;
    trigger.disabled = true;
    say(node, 'Opening Zashboard…');
    try {
      const target = M.dashboardTarget(await request('/api/mihomo/dashboard', {...json('POST', {}), timeoutMs: 15000}));
      if (!UI.current(epoch)) { if (tab) tab.close(); return; }
      if (!target) throw new Error('The router returned an unexpected dashboard address');
      if (tab) { tab.location.href = target; say(node, 'Zashboard is open in a new tab. It connects to Clash by itself.'); return; }
      say(node, 'Your browser blocked the new tab. ');
      const link = el('a', '', 'Open Zashboard');
      link.href = target; link.target = '_blank'; link.rel = 'noopener';
      node.append(link);
    } catch (error) {
      if (tab) tab.close();
      if (!UI.current(epoch)) return;
      say(node, error.status === 409 ? 'Clash is off. Start it first.' : `Zashboard could not be opened. ${error.message}`, true);
    } finally { if (UI.current(epoch)) trigger.disabled = false; }
  }
  M.openDashboard = openDashboard;

  // ── Status ──────────────────────────────────────────────────────────────
  const statusPanel = UI.panel(route, 'mihomo-status', 'Clash / Mihomo', 'Experimental LAN TCP/UDP, IPv4/IPv6 interception through the pinned Mihomo core. Node UDP support and IPv6 exit reachability still apply. No subscription or paid node is included.');
  statusPanel.classList.add('mihomo-panel');
  const hero = el('div', 'mihomo-hero'), glyph = el('div', 'mihomo-glyph'), copy = el('div'), title = el('h3', '', 'Clash is off'), detail = el('p', ''), chip = el('span', 'mihomo-state', 'OFF');
  glyph.append(el('i'));
  copy.append(el('span', 'eyebrow', 'TRANSPARENT PROXY'), title, detail);
  hero.append(glyph, copy, chip);
  const facts = el('dl', 'mihomo-facts');
  const problem = el('p', 'relay-problem');
  problem.setAttribute('role', 'alert');
  problem.hidden = true;
  const actions = el('div', 'form-actions'), toggle = button('Start Clash', toggleCore, 'primary'), install = button('Install Mihomo core', installCore),
    cleanup = button('Retry proxy cleanup', () => stopCore('cleanup')), clearStart = button('Stop automatic start & keep off', () => stopCore('autostart')), refresh = button('Read current status', () => load());
  const zashboard = button('Open Zashboard', () => openDashboard(statusText, zashboard));
  actions.append(toggle, zashboard, install, cleanup, clearStart, refresh);
  const hint = el('p', 'field-help');
  const statusText = UI.status(statusPanel);
  statusPanel.append(hero, facts, problem, actions, hint, statusText);

  // ── Profiles ────────────────────────────────────────────────────────────
  const profilePanel = UI.panel(route, 'mihomo-profiles', 'Configuration profiles', 'Switching or editing the active profile briefly reconnects Clash. An invalid download, Merge or Script keeps the previous configuration. Downloads use Clash when it runs, otherwise the current uplink; cellular data is never turned on.');
  const profileBar = el('div', 'form-actions'), addProfile = button('Add configuration', () => openEditor('add'), 'primary');
  profileBar.append(addProfile);
  const profileList = el('div', 'mihomo-profile-list');
  const form = el('form', 'settings-form mihomo-editor');
  form.hidden = true;
  form.autocomplete = 'off';
  const formTitle = el('h3', '', 'Add configuration');
  function field(labelText, input, helpText) {
    const wrapper = el('div', 'form-field'), label = el('label', '', labelText);
    input.id = input.id || `mihomo-${Math.random().toString(36).slice(2, 8)}`;
    label.htmlFor = input.id;
    wrapper.append(label, input);
    if (helpText) wrapper.append(el('small', 'field-help', helpText));
    return wrapper;
  }
  const nameInput = el('input'), typeInput = el('select'), urlInput = el('input'), yamlInput = el('textarea', 'mono'), intervalInput = el('select'), fileInput = el('input');
  nameInput.id = 'mihomo-profile-name'; nameInput.maxLength = 128; nameInput.name = 'name'; nameInput.autocomplete = 'off';
  typeInput.id = 'mihomo-profile-type'; typeInput.name = 'source_type';
  typeInput.add(new Option('Subscription URL', 'remote')); typeInput.add(new Option('Pasted or imported YAML', 'local'));
  urlInput.id = 'mihomo-profile-url'; urlInput.name = 'url'; urlInput.type = 'text'; urlInput.inputMode = 'url'; urlInput.spellcheck = false; urlInput.autocomplete = 'off'; urlInput.placeholder = 'https://example.com/subscription';
  yamlInput.id = 'mihomo-profile-yaml'; yamlInput.name = 'yaml'; yamlInput.rows = 12; yamlInput.spellcheck = false; yamlInput.autocomplete = 'off'; yamlInput.placeholder = 'proxies:\n  - …\nrules:\n  - MATCH,DIRECT';
  intervalInput.id = 'mihomo-profile-interval'; intervalInput.name = 'interval_hours';
  for (const [hours, text] of M.intervals) intervalInput.add(new Option(text, String(hours)));
  fileInput.type = 'file'; fileInput.id = 'mihomo-profile-file'; fileInput.accept = '.yaml,.yml,.txt,text/yaml,text/plain';
  const urlField = field('Subscription URL', urlInput, 'HTTPS only, without a username, password or #fragment. The URL is stored on the router with owner-only permissions and shown only when you edit this profile.');
  const intervalField = field('Update schedule', intervalInput, 'Scheduled updates run in the background and keep the old copy if a download or validation fails.');
  const yamlField = field('Clash YAML', yamlInput);
  const yamlHelp = el('small', 'field-help', 'Up to 512 KiB. Credentials in the YAML stay on the router and are never shown again.');
  const fileField = field('Or choose a YAML file', fileInput);
  yamlField.append(yamlHelp);
  const formFields = el('div', 'form-grid');
  formFields.append(field('Name', nameInput), field('Source', typeInput), urlField, intervalField, yamlField, fileField);
  const formActions = el('div', 'form-actions'), saveProfile = button('Validate & save', null, 'primary'), cancelEdit = button('Cancel', closeEditor);
  saveProfile.type = 'submit';
  formActions.append(saveProfile, cancelEdit);
  form.append(formTitle, formFields, formActions);
  const profileText = UI.status(profilePanel);
  profilePanel.append(profileBar, profileList, form, profileText);

  // ── Global Merge & Script ───────────────────────────────────────────────
  const extPanel = UI.panel(route, 'mihomo-extensions', 'Global Merge & Script', 'Optional preprocessing applied to every profile before it is validated: a YAML Merge overlay and a sandboxed JavaScript function. All profiles are re-validated on save; a failure keeps the old configuration.');
  const extBar = el('div', 'form-actions'), openExt = button('Edit Merge & Script', openExtensions);
  extBar.append(openExt);
  const extForm = el('form', 'settings-form mihomo-editor');
  extForm.hidden = true;
  extForm.autocomplete = 'off';
  const mergeInput = el('textarea', 'mono'), scriptInput = el('textarea', 'mono');
  mergeInput.id = 'mihomo-merge'; mergeInput.rows = 9; mergeInput.spellcheck = false; mergeInput.placeholder = 'dns:\n  enable: true';
  scriptInput.id = 'mihomo-script'; scriptInput.rows = 9; scriptInput.spellcheck = false; scriptInput.placeholder = 'function main(config, profileName) {\n  return config;\n}';
  function toggleField(text) {
    const label = el('label', 'relay-choice'), input = el('input');
    input.type = 'checkbox';
    label.append(input, el('span', '', text));
    return {label, input};
  }
  const mergeOn = toggleField('Apply Merge to every profile'), scriptOn = toggleField('Run the Script on every profile');
  const extFields = el('div', 'form-grid');
  extFields.append(field('Merge (YAML)', mergeInput, 'Mappings merge; arrays replace. Use prepend-/append- keys to add to a list. Use {} to clear saved content.'), field('Script (JavaScript)', scriptInput, 'A fresh sandbox per run with no network, file or timer access and strict time and memory limits. Define main(config, profileName) and return the config.'), mergeOn.label, scriptOn.label);
  const extActions = el('div', 'form-actions'), saveExt = button('Validate all & save', null, 'primary'), cancelExt = button('Cancel', closeExtensions);
  saveExt.type = 'submit';
  extActions.append(saveExt, cancelExt);
  extForm.append(extFields, extActions);
  const extText = UI.status(extPanel);
  extPanel.append(extBar, extForm, extText);

  // ── State ───────────────────────────────────────────────────────────────
  let status = null, library = null, libraryError = '', loading = false, attempted = false, busy = false, lastLoad = 0;
  let editor = null;       // {mode, id, hasSavedYaml, baseline}
  let extOpen = false, extBaseline = null;
  let loadId = 0, editorId = 0, extensionId = 0;
  const onRoute = () => OpenU60Navigation.current.key === route;
  const editorDirty = () => !!editor && (nameInput.value !== editor.baseline.name || typeInput.value !== editor.baseline.type || urlInput.value !== editor.baseline.url || yamlInput.value !== '' || intervalInput.value !== editor.baseline.interval);
  const extDirty = () => extOpen && !!extBaseline && (mergeInput.value !== extBaseline.merge || scriptInput.value !== extBaseline.script || mergeOn.input.checked !== extBaseline.merge_enabled || scriptOn.input.checked !== extBaseline.script_enabled);
  const dirty = () => editorDirty() || extDirty();
  const profiles = () => Array.isArray(library?.profiles) ? library.profiles : [];

  function drawStatus() {
    const view = M.view(status);
    statusPanel.dataset.state = view.key;
    glyph.dataset.state = view.key;
    chip.textContent = view.chip;
    title.textContent = view.title;
    detail.textContent = view.detail;
    facts.replaceChildren(...M.facts(status).map(([label, value]) => { const pair = el('div'); pair.append(el('dt', '', label), el('dd', '', value)); return pair; }));
    problem.hidden = !status?.error;
    problem.textContent = status?.error || '';
    const disabled = busy || loading || !API.connected;
    toggle.textContent = view.toggleLabel;
    toggle.className = `button ${view.running ? 'danger' : 'primary'}`;
    toggle.hidden = view.recovery;
    toggle.disabled = disabled || !view.canStart;
    zashboard.hidden = !view.running;
    zashboard.disabled = disabled;
    install.hidden = !view.showInstall;
    install.textContent = view.installLabel;
    install.disabled = disabled;
    cleanup.hidden = !view.recovery;
    cleanup.disabled = disabled;
    clearStart.hidden = !view.showClearAutostart;
    clearStart.disabled = disabled;
    refresh.disabled = loading || !API.connected;
    hint.textContent = view.toggleHint;
    hint.hidden = !view.toggleHint;
  }
  function drawProfiles() {
    const view = M.view(status), usable = view.installed && !view.recovery && API.connected && !busy;
    addProfile.disabled = !usable || !!editor;
    const rows = profiles().map(profile => {
      const card = el('article', 'mihomo-profile'), head = el('div', 'mihomo-profile-head'), copyBox = el('div'), selected = library.selected === profile.id;
      copyBox.append(el('h3', '', profile.name), el('p', 'field-help', [profile.remote ? `Subscription · ${M.intervalLabel(profile.interval_hours)}` : 'Local configuration', M.updatedLabel(profile.updated)].join(' · ')));
      head.append(copyBox);
      if (selected) head.append(el('span', 'tag', 'Selected'));
      card.append(head);
      if (profile.error) { const message = el('p', 'relay-problem', profile.error); card.append(message); }
      const row = el('div', 'form-actions');
      if (!selected) row.append(button('Use profile', () => profileAction('select', profile)));
      if (profile.remote) row.append(button('Update now', () => profileAction('refresh', profile)));
      row.append(button('Edit', () => openEditor('edit', profile)), button('Delete', () => profileAction('delete', profile), 'danger'));
      for (const control of row.querySelectorAll('button')) control.disabled = !usable;
      card.append(row);
      return card;
    });
    const message = libraryError || (!view.installed ? 'Install the Mihomo core to add configuration profiles.' : view.recovery ? 'Retry proxy cleanup before changing profiles.' : '');
    profileList.replaceChildren(...(rows.length ? rows : [el('p', 'empty', message || 'No saved profiles.')]), ...(rows.length && message ? [el('p', 'field-help', message)] : []));
    openExt.disabled = !usable || extOpen;
    saveProfile.disabled = busy || !API.connected;
    saveExt.disabled = busy || !API.connected;
  }
  function draw() { drawStatus(); drawProfiles(); }
  function drawSource() {
    const remote = typeInput.value === 'remote';
    urlField.hidden = intervalField.hidden = !remote;
    yamlField.hidden = fileField.hidden = remote;
    urlInput.required = false;
  }
  typeInput.addEventListener('change', drawSource);

  // ── Loading ─────────────────────────────────────────────────────────────
  async function load(quiet = false) {
    if (loading || API.locked || !API.connected) return;
    const epoch = UI.epoch, id = ++loadId;
    loading = true; attempted = true; lastLoad = Date.now(); refresh.disabled = true;
    let failure = '';
    const [state, list] = await Promise.allSettled([request('/api/mihomo/status'), request('/api/mihomo/profiles')]);
    if (!UI.current(epoch) || id !== loadId) return;
    if (state.status === 'fulfilled') status = state.value; else failure = `Clash status unavailable. ${state.reason.message}`;
    if (list.status === 'fulfilled') { library = list.value; libraryError = ''; } else libraryError = `Profile library unavailable. ${list.reason.message}`;
    loading = false; draw();
    if (failure) say(statusText, failure, true); else if (!quiet) say(statusText, '');
  }
  async function reload() { loading = false; await load(true); }

  // One place for every state-changing call, as on the relay page.
  async function act(node, method, pathName, body, {timeoutMs, working, done, after}) {
    await mutate(node, async live => {
      busy = true; draw(); say(node, working);
      let ok = false;
      try {
        const data = await request(pathName, {...json(method, body), timeoutMs});
        if (!live()) return;
        ok = true;
        if (data && typeof data === 'object' && 'running' in data) status = data;
        if (data && typeof data === 'object' && 'profiles' in data) library = data;
        say(node, typeof done === 'function' ? done(data) : done);
      } catch (error) {
        if (!live()) return;
        say(node, error.status ? error.message : `The router did not answer (${error.message}). Check its state before repeating this change; it may already have taken effect.`, true);
      } finally {
        busy = false;
        if (live()) { draw(); await reload(); if (ok && live()) after?.(); }
      }
    });
  }

  async function toggleCore() {
    const view = M.view(status);
    if (!view.running && !view.canStart) return;
    const [heading, text, label] = view.running
      ? ['Stop Clash?', 'Interception is removed and traffic returns to direct routing immediately. Automatic start is cleared.', 'Stop Clash']
      : ['Start Clash?', 'LAN TCP/UDP traffic (IPv4 and IPv6) is routed through the Mihomo core. Connections may briefly reset. Turning Clash off returns to direct routing; it is not a kill switch.', 'Start Clash'];
    if (!await confirm(heading, text, {label, destructive: view.running})) return;
    await act(statusText, 'POST', '/api/mihomo/switch', {enabled: !view.running}, {timeoutMs: 3 * MINUTE, working: view.running ? 'Stopping Clash…' : 'Starting Clash. Validating the configuration and the interception first…', done: view.running ? 'Clash is off.' : 'Clash is running.'});
  }
  async function stopCore(kind) {
    const [heading, text, label] = kind === 'cleanup'
      ? ['Retry proxy cleanup?', 'Remove the remaining proxy network changes. This also clears automatic start.', 'Retry cleanup']
      : ['Keep Clash off?', 'Cancel any pending restoration and clear the saved restart choice. Clash stays off after future restarts.', 'Keep off'];
    if (!await confirm(heading, text, {label, destructive: true})) return;
    await act(statusText, 'POST', '/api/mihomo/switch', {enabled: false}, {timeoutMs: 3 * MINUTE, working: 'Working…', done: kind === 'cleanup' ? 'Cleanup finished.' : 'Clash stays off; automatic start cleared.'});
  }
  async function installCore() {
    const view = M.view(status);
    if (!await confirm(view.installed ? 'Reinstall the pinned core?' : 'Install the Mihomo core?', 'Downloads about 22 MB from pinned official releases, verifies their SHA-256 and saves them under /data/zte-agent-mihomo. This may use cellular data. It does not start the proxy.', {label: view.installed ? 'Reinstall' : 'Install'})) return;
    await act(statusText, 'POST', '/api/mihomo/install', {}, {timeoutMs: 10 * MINUTE, working: 'Downloading and verifying. This can take several minutes…', done: 'Core installed. Add a configuration profile, then start Clash.'});
  }

  // ── Profile editor ──────────────────────────────────────────────────────
  function closeEditor() {
    editorId++;
    editor = null; form.hidden = true;
    nameInput.value = urlInput.value = yamlInput.value = ''; fileInput.value = '';
    typeInput.value = 'remote'; intervalInput.value = '0';
    drawSource(); drawProfiles();
  }
  async function openEditor(mode, profile) {
    const epoch = UI.epoch, id = ++editorId;
    const current = () => UI.current(epoch) && id === editorId;
    if (editorDirty() && !await confirm('Replace the draft?', 'Opening another editor discards the unsaved profile draft.', {label: 'Discard draft'})) return;
    if (!current()) return;
    let saved = {name: '', type: 'remote', url: '', interval: '0'};
    if (mode === 'edit') {
      say(profileText, 'Reading the profile…');
      try {
        const data = await request(`/api/mihomo/profiles/${encodeURIComponent(profile.id)}`);
        if (!current()) return;
        saved = {name: data.name || profile.name, type: data.source_type === 'local' ? 'local' : 'remote', url: data.url || '', interval: String(data.interval_hours || 0)};
      } catch (error) { if (!current()) return; say(profileText, `Cannot load this configuration. ${error.message}`, true); return; }
      say(profileText, '');
    } else say(profileText, '');
    editor = {mode, id: profile?.id || '', hasSavedYaml: mode === 'edit' && saved.type === 'local', baseline: saved};
    formTitle.textContent = mode === 'edit' ? `Edit ${saved.name}` : 'Add configuration';
    nameInput.value = saved.name; typeInput.value = saved.type; urlInput.value = saved.url; intervalInput.value = saved.interval; yamlInput.value = ''; fileInput.value = '';
    yamlInput.placeholder = editor.hasSavedYaml ? 'Leave empty to keep the saved YAML, or paste a replacement.' : 'proxies:\n  - …\nrules:\n  - MATCH,DIRECT';
    saveProfile.textContent = mode === 'edit' ? 'Validate & save changes' : 'Validate & add';
    form.hidden = false; drawSource(); drawProfiles();
    nameInput.focus();
  }
  fileInput.addEventListener('change', async () => {
    const file = fileInput.files?.[0];
    if (!file) return;
    if (file.size > M.MAX_CONFIG) { say(profileText, 'That file is larger than 512 KiB.', true); fileInput.value = ''; return; }
    const epoch = UI.epoch, id = editorId;
    try { const content = await file.text(); if (!UI.current(epoch) || id !== editorId) return; yamlInput.value = content; say(profileText, `Loaded ${file.name}. It is validated when you save.`); } catch { if (UI.current(epoch) && id === editorId) say(profileText, 'The file could not be read.', true); }
  });
  form.addEventListener('submit', async event => {
    event.preventDefault();
    if (!editor) return;
    const input = {mode: editor.mode, id: editor.id, type: typeInput.value, name: nameInput.value, url: urlInput.value, yaml: yamlInput.value, interval: intervalInput.value, hasSavedYaml: editor.hasSavedYaml};
    const issue = M.profileProblem(input);
    if (issue) { say(profileText, issue, true); return; }
    const [heading, text, label] = M.confirmation(editor.mode, {name: input.name.trim()}, status?.running === true);
    if (!await confirm(heading, text, {label})) return;
    await act(profileText, 'POST', '/api/mihomo/profiles', M.profileRequest(input), {timeoutMs: 3 * MINUTE, working: input.type === 'remote' ? 'Downloading and validating…' : 'Validating…', done: 'Profile saved.', after: closeEditor});
  });
  async function profileAction(action, profile) {
    const [heading, text, label] = M.confirmation(action, profile, status?.running === true);
    if (!await confirm(heading, text, {label, destructive: action === 'delete'})) return;
    await act(profileText, 'POST', '/api/mihomo/profiles', M.profileAction(action, profile.id), {timeoutMs: 3 * MINUTE, working: action === 'refresh' ? 'Downloading and validating…' : 'Working…', done: {select: 'Profile selected.', refresh: 'Profile updated.', delete: 'Profile deleted.'}[action]});
  }

  // ── Extensions editor ───────────────────────────────────────────────────
  function closeExtensions() {
    extensionId++;
    extOpen = false; extBaseline = null; extForm.hidden = true;
    mergeInput.value = scriptInput.value = ''; mergeOn.input.checked = scriptOn.input.checked = false;
    drawProfiles();
  }
  async function openExtensions() {
    const epoch = UI.epoch, id = ++extensionId;
    const current = () => UI.current(epoch) && id === extensionId;
    say(extText, 'Reading the saved Merge & Script…');
    try {
      const data = await request('/api/mihomo/extensions');
      if (!current()) return;
      extBaseline = {merge: data.merge || '', script: data.script || '', merge_enabled: data.merge_enabled === true, script_enabled: data.script_enabled === true};
    } catch (error) { if (!current()) return; say(extText, `Cannot load the saved Merge & Script. ${error.message}. The saved configuration is unchanged.`, true); return; }
    say(extText, '');
    mergeInput.value = extBaseline.merge; scriptInput.value = extBaseline.script; mergeOn.input.checked = extBaseline.merge_enabled; scriptOn.input.checked = extBaseline.script_enabled;
    extOpen = true; extForm.hidden = false; drawProfiles();
  }
  extForm.addEventListener('submit', async event => {
    event.preventDefault();
    const draft = {merge: mergeInput.value, script: scriptInput.value, merge_enabled: mergeOn.input.checked, script_enabled: scriptOn.input.checked};
    const issue = M.extensionsProblem(draft);
    if (issue) { say(extText, issue, true); return; }
    if (!await confirm('Save Merge & Script?', `Every profile is re-validated with these settings. ${status?.running ? 'Clash briefly reconnects if the active configuration changes. ' : ''}If any profile fails, nothing changes.`, {label: 'Validate all & save'})) return;
    await act(extText, 'POST', '/api/mihomo/profiles', M.extensionsRequest(draft), {timeoutMs: 5 * MINUTE, working: 'Validating every profile…', done: 'Global Merge & Script saved and applied.', after: closeExtensions});
  });

  function refreshVisible() {
    if (document.hidden || dirty() || loading || busy || API.locked || !API.connected || !onRoute() || Date.now() - lastLoad < 1000) return;
    load(true);
  }
  window.addEventListener('focus', refreshVisible);
  window.addEventListener('pageshow', refreshVisible);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshVisible(); });

  UI.track({
    route,
    get loaded() { return attempted; },
    get dirty() { return dirty(); },
    load,
    clear() {
      loadId++;
      status = library = null; libraryError = ''; attempted = loading = busy = false;
      closeEditor(); closeExtensions();
      for (const node of [statusText, profileText, extText]) say(node, '');
      draw();
    }
  });
  drawSource();
  draw();
})();
