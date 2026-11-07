'use strict';
(() => {
  const route = 'network/relay', UI = OpenU60UI, API = OpenU60Console, R = OpenU60Relay;
  const {el, button, request, json, confirm, mutate, say} = UI;
  const MINUTE = 60000;

  // ── Status ──────────────────────────────────────────────────────────────
  const statusPanel = UI.panel(route, 'relay-status', 'Wi-Fi relay', 'Use nearby Wi-Fi as the U60’s internet connection. Your hotspot keeps serving your devices.');
  statusPanel.classList.add('relay-panel');
  const hero = el('div', 'relay-hero'), glyph = el('div', 'relay-glyph'), copy = el('div'), eyebrow = el('span', 'eyebrow', 'UPSTREAM WI-FI'),
    title = el('h3', '', 'Share a Wi-Fi connection'), detail = el('p', '', ''), chip = el('span', 'relay-state', 'RELAY OFF');
  glyph.append(el('i'));
  copy.append(eyebrow, title, detail);
  hero.append(glyph, copy, chip);
  const path = el('p', 'relay-path');
  path.setAttribute('aria-label', 'Upstream Wi-Fi to U60 Pro to your devices');
  path.append(el('span', '', 'Wi-Fi'), el('b', '', '→'), el('span', '', 'U60 Pro'), el('b', '', '→'), el('span', '', 'Your devices'));
  const facts = el('dl', 'relay-facts');
  facts.hidden = true;
  const problem = el('p', 'relay-problem');
  problem.setAttribute('role', 'alert');
  problem.hidden = true;
  const clockBox = el('div', 'relay-clock'), clockText = el('p'), clockButton = button('Recheck time', recheckTime);
  clockBox.hidden = true;
  clockBox.append(el('strong', '', 'Wi-Fi connected; time not verified.'), clockText, clockButton, el('small', 'field-help', 'Retry after upstream internet or portal access is restored. This check does not reconnect Wi-Fi.'));
  const actions = el('div', 'form-actions'), refresh = button('Read current status', () => load()), stop = button('Disconnect relay', stopRelay, 'danger');
  stop.hidden = true;
  actions.append(refresh, stop);
  const caution = el('p', 'field-help relay-caution', 'Hotspots may briefly reconnect. If Wi-Fi drops, SIM data may be used only if cellular data is already on. Relay never turns it on. Hotel login is completed in a browser on a connected device, never by ignoring certificate warnings.');
  const statusText = UI.status(statusPanel);
  statusPanel.append(hero, path, facts, problem, clockBox, actions, caution, statusText);

  // ── Networks ────────────────────────────────────────────────────────────
  const netPanel = UI.panel(route, 'relay-networks', 'Upstream networks', 'Find nearby Wi-Fi and choose the one the U60 should use. Open and WPA2 networks are supported; WPA3-only, enterprise and hidden networks are not.');
  const scanBar = el('div', 'form-actions'), scan = button('Scan', scanNetworks, 'primary');
  scanBar.append(scan);
  const connectForm = el('form', 'settings-form relay-connect'), list = el('fieldset', 'relay-networks'), legend = el('legend', 'sr-only', 'Select upstream Wi-Fi');
  list.append(legend);
  const empty = el('p', 'empty', 'Your next connection starts here. Press Scan to discover networks.');
  const fields = el('div', 'form-grid relay-fields'), passwordField = el('div', 'form-field'), passwordLabel = el('label', '', 'Wi-Fi password'), password = el('input');
  password.id = 'relay-password'; password.type = 'password'; password.name = 'password'; password.maxLength = 64; password.autocomplete = 'new-password'; password.spellcheck = false; password.placeholder = 'Network password';
  passwordLabel.htmlFor = password.id;
  passwordField.append(passwordLabel, password, el('small', 'field-help', 'Leave empty for open or remembered networks. It is stored on the router only if you choose to remember it.'));
  const widthField = el('div', 'form-field'), widthLabel = el('label', '', 'Bandwidth'), width = el('select');
  width.id = 'relay-width'; width.name = 'width_mode'; widthLabel.htmlFor = width.id;
  for (const [value, text] of [['saved', 'Use saved choice'], ['off', R.widthLabel('off')], ['auto', R.widthLabel('auto')]]) width.add(new Option(text, value));
  widthField.append(widthLabel, width, el('small', 'field-help', 'New networks keep the negotiated bandwidth. Each Wi-Fi remembers its own choice.'));
  fields.append(passwordField, widthField);
  function choice(name, text, checked) {
    const label = el('label', 'relay-choice'), input = el('input');
    input.type = 'checkbox'; input.name = name; input.checked = checked;
    label.append(input, el('span', '', text));
    return {label, input};
  }
  const remember = choice('remember', 'Remember this network’s password after a successful connection.', true);
  const ipv6 = choice('ipv6', 'Use upstream IPv6 for Clash and enable LAN NAT66 when available (experimental). Without usable upstream IPv6 the relay stays IPv4-only.', true);
  const connectActions = el('div', 'form-actions'), connect = button('Connect to Wi-Fi', null, 'primary');
  connect.type = 'submit';
  connectActions.append(connect);
  connectForm.append(list, empty, fields, remember.label, ipv6.label, connectActions);
  connectForm.hidden = true;
  const netText = UI.status(netPanel);
  netPanel.append(scanBar, connectForm, netText);

  // ── Saved networks ──────────────────────────────────────────────────────
  const savedPanel = UI.panel(route, 'relay-saved', 'Saved Wi-Fi networks', 'Bandwidth choices are kept separately from passwords. Scan and select a network to connect. Forget removes a choice and any remembered password without disconnecting an active relay.');
  const savedList = el('div', 'relay-saved-list'), savedText = UI.status(savedPanel);
  savedPanel.append(savedList, savedText);

  // ── Notes ───────────────────────────────────────────────────────────────
  const notes = UI.panel(route, 'relay-notes', 'Compatibility & safety', 'What the relay changes, and what it leaves alone.');
  notes.classList.add('relay-notes');
  for (const text of [
    'Keep both main hotspots enabled and Guest WiFi off. Do not change WiFi, LAN or USB settings while connected; the router refuses most of them.',
    'Open and WPA2-Personal networks on 2.4 GHz and 5 GHz channels the radio allows, including DFS. A DFS channel can delay the hotspot while it checks for radar.',
    'With IPv6 on, the LAN gets SLAAC/NAT66 through the upstream. Cellular IPv6 stays blocked while the relay is connected, so nothing leaks around it.',
    'Internet access and hotel authorization are not verified automatically. A dropped link is not reconnected while the agent runs; after an agent restart or reboot the saved choice is retried.'
  ]) notes.append(el('p', 'field-help', text));

  // ── State ───────────────────────────────────────────────────────────────
  let current = null, loading = false, attempted = false, busy = false, selectedId = '', lastLoad = 0, pollTimer = 0, settleTimer = 0;
  let loadId = 0, settleId = 0;
  const onRoute = () => OpenU60Navigation.current.key === route;
  const dirty = () => password.value !== '';

  function selectedNetwork() { return R.usable(current?.networks).find(network => network.id === selectedId); }
  function drawStatus() {
    const view = R.view(current);
    statusPanel.dataset.state = view.key;
    glyph.dataset.state = view.key;
    chip.textContent = view.chip;
    title.textContent = view.title;
    detail.textContent = view.detail;
    const rows = R.facts(current);
    facts.hidden = rows.length === 0;
    facts.replaceChildren(...rows.map(([label, value, literal]) => {
      const pair = el('div');
      pair.append(el('dt', '', label), el('dd', literal ? 'mono' : '', value));
      return pair;
    }));
    const messages = [current?.error, current?.ipv6?.error].filter(Boolean);
    problem.hidden = messages.length === 0;
    problem.textContent = messages.join(' ');
    clockBox.hidden = !(view.connected && current?.clock_error);
    clockText.textContent = current?.clock_error || '';
    clockButton.disabled = busy || !API.connected;
    stop.hidden = !view.stop;
    stop.textContent = view.stopLabel;
    stop.disabled = busy || loading || !API.connected;
    refresh.disabled = loading || !API.connected;
  }
  function drawNetworks() {
    const networks = R.usable(current?.networks);
    if (!networks.some(network => network.id === selectedId)) selectedId = '';
    const rows = networks.map(network => {
      const row = el('label', 'relay-network'), radio = el('input'), name = el('span', 'relay-network-name'), meta = el('small', '');
      radio.type = 'radio'; radio.name = 'network'; radio.value = network.id; radio.checked = network.id === selectedId; radio.disabled = network.supported !== true;
      radio.addEventListener('change', () => { selectedId = network.id; drawConnect(); });
      meta.textContent = `${R.band(network.frequency)} · ${network.signal} dBm · ${R.securityLabel(network)}`;
      const bars = el('span', 'relay-signal');
      bars.dataset.level = R.level(network.signal);
      bars.setAttribute('aria-hidden', 'true');
      bars.append(el('i'), el('i'), el('i'), el('i'));
      name.append(el('strong', '', network.ssid), meta);
      if (network.supported === true) name.append(el('small', '', `Saved bandwidth: ${R.widthLabel(network.width_mode)}`));
      row.append(radio, bars, name);
      if (network.remembered === true) row.append(el('span', 'tag', 'Saved'));
      return row;
    });
    list.replaceChildren(legend, ...rows);
    empty.hidden = rows.length > 0;
    list.hidden = rows.length === 0;
    connectForm.hidden = false;
    fields.hidden = rows.length === 0;
    remember.label.hidden = ipv6.label.hidden = connectActions.hidden = rows.length === 0;
    scan.textContent = rows.length ? 'Rescan' : 'Scan';
    drawConnect();
  }
  function drawConnect() {
    const network = selectedNetwork(), open = network?.security === 'open';
    password.disabled = !network || open;
    if (open) password.value = '';
    remember.input.disabled = !network || open;
    connect.disabled = busy || !network || !API.connected;
    scan.disabled = busy || !API.connected;
  }
  function drawSaved() {
    const saved = Array.isArray(current?.saved_networks) ? current.saved_networks : [];
    const rows = saved.map(entry => {
      const active = current?.connected === true && current.saved_network_id === entry.id;
      const card = el('article', 'relay-saved-item'), head = el('div', 'relay-saved-head'), copyBox = el('div'), select = el('select'), actionsRow = el('div', 'form-actions');
      copyBox.append(el('h3', '', entry.ssid), el('p', 'field-help', R.savedNote(entry)));
      head.append(copyBox);
      if (active) head.append(el('span', 'tag', 'Connected'));
      select.setAttribute('aria-label', `Bandwidth for ${entry.ssid}`);
      for (const mode of ['off', 'auto']) select.add(new Option(R.widthLabel(mode), mode));
      select.value = entry.width_mode === 'auto' ? 'auto' : 'off';
      const apply = button('Save bandwidth', () => saveBandwidth(entry, select.value, active)), forget = button('Forget', () => forgetNetwork(entry), 'danger');
      apply.disabled = busy || select.value === (entry.width_mode === 'auto' ? 'auto' : 'off');
      select.addEventListener('change', () => { apply.disabled = busy || select.value === (entry.width_mode === 'auto' ? 'auto' : 'off'); });
      forget.disabled = busy;
      actionsRow.append(select, apply, forget);
      card.append(head, actionsRow);
      return card;
    });
    savedList.replaceChildren(...(rows.length ? rows : [el('p', 'empty', current?.saved_error || 'No saved Wi-Fi networks.')]));
  }
  function draw() { drawStatus(); drawNetworks(); drawSaved(); }

  // ── Loading ─────────────────────────────────────────────────────────────
  function schedulePoll() {
    clearTimeout(pollTimer);
    // Only a restore in progress changes by itself; an idle page never polls.
    if (R.view(current).restoring && onRoute() && !document.hidden) pollTimer = setTimeout(() => load(true), 5000);
  }
  async function load(quiet = false) {
    if (loading || API.locked || !API.connected) return;
    const epoch = UI.epoch, id = ++loadId;
    loading = true; attempted = true; lastLoad = Date.now(); refresh.disabled = true;
    let failure = '';
    try { const data = await request('/api/wifi-relay/status'); if (UI.current(epoch) && id === loadId) current = data; } catch (error) { failure = `Relay status unavailable. ${error.message}`; }
    finally {
      if (!UI.current(epoch) || id !== loadId) return;
      loading = false; draw(); schedulePoll();
      if (failure) say(statusText, failure, true);
      else if (!quiet) say(statusText, '');
    }
  }
  // After a request that never answered, the operation may still be running on
  // the router: read its state again instead of guessing.
  function settle() {
    clearTimeout(settleTimer);
    const epoch = UI.epoch, id = ++settleId;
    const live = () => UI.current(epoch) && id === settleId;
    let attempts = 0;
    const tick = async () => {
      if (!live() || !API.connected) return;
      try { const data = await request('/api/wifi-relay/status'); if (!live()) return; current = data; draw(); schedulePoll(); say(statusText, 'Router state re-read after the interrupted request.'); return; } catch { /* keep waiting */ }
      if (live() && ++attempts < 8) settleTimer = setTimeout(tick, 6000);
    };
    settleTimer = setTimeout(tick, 6000);
  }

  // One place for every state-changing call: definite refusals show the router’s
  // reason; a request that never answered is reported as uncertain.
  async function act(node, pathName, body, {timeoutMs, working, done}) {
    await mutate(node, async live => {
      busy = true; draw(); say(node, working);
      try {
        const data = await request(pathName, {...json('POST', body), timeoutMs});
        if (!live()) return;
        current = data; say(node, typeof done === 'function' ? done(data) : done);
      } catch (error) {
        if (!live()) return;
        if (error.status) {
          say(node, error.message, true);
          try { const data = await request('/api/wifi-relay/status'); if (live()) current = data; } catch { /* the message above stands */ }
        } else {
          say(node, `The router did not answer (${error.message}). The operation may still be running; its state will be re-read shortly.`, true);
          settle();
        }
      } finally {
        busy = false;
        if (live()) { draw(); schedulePoll(); }
      }
    });
  }

  async function scanNetworks() {
    await act(netText, '/api/wifi-relay/scan', {}, {timeoutMs: 2 * MINUTE, working: 'Scanning. This can take up to a minute; 5 GHz channels are checked one at a time.', done: data => `Found ${R.usable(data?.networks).length} network${R.usable(data?.networks).length === 1 ? '' : 's'}.`});
  }
  async function recheckTime() {
    await act(statusText, '/api/wifi-relay/clock', {}, {timeoutMs: MINUTE, working: 'Verifying time over Wi-Fi…', done: 'Time verified over the relay.'});
  }
  async function stopRelay() {
    const view = R.view(current);
    const text = view.connected ? 'The relay is removed, hotspot settings are restored and the restart choice is cleared. The hotspot may take up to a minute to return while it checks for radar.' : view.restoring ? 'Cancel the pending reconnection and clear the restart choice.' : view.recovery ? 'Retry the cleanup without scanning or connecting.' : 'Clear the saved restart choice.';
    if (!await confirm(`${view.stopLabel}?`, text, {label: view.stopLabel, destructive: true})) return;
    await act(statusText, '/api/wifi-relay/disconnect', {}, {timeoutMs: 3 * MINUTE, working: 'Restoring network settings…', done: 'Relay is off and network settings are restored.'});
  }
  async function saveBandwidth(entry, mode, active) {
    const text = active ? 'This network is connected. Applying a different bandwidth mode reconnects Wi-Fi, and the hotspot may briefly reconnect.' : 'The choice is saved now and used the next time you connect to this network.';
    if (!await confirm('Save bandwidth choice?', text, {label: 'Save bandwidth'})) return;
    await act(savedText, '/api/wifi-relay/tuning', {id: entry.id, width_mode: mode, confirmed: true}, {timeoutMs: 5 * MINUTE, working: active ? 'Applying and reconnecting…' : 'Saving…', done: 'Bandwidth choice saved.'});
  }
  async function forgetNetwork(entry) {
    if (!await confirm(`Forget ${entry.ssid}?`, 'Its bandwidth choice and any remembered password are removed. An active relay stays connected.', {label: 'Forget network', destructive: true})) return;
    await act(savedText, '/api/wifi-relay/forget', {id: entry.id}, {timeoutMs: MINUTE, working: 'Forgetting…', done: 'Network forgotten.'});
  }
  connectForm.addEventListener('submit', async event => {
    event.preventDefault();
    const network = selectedNetwork();
    const issue = R.passwordProblem(network, password.value, network?.remembered === true);
    if (issue) { say(netText, issue, true); return; }
    if (!await confirm(`Connect to ${network.ssid}?`, R.consent(network, current), {label: 'Connect'})) return;
    const body = R.connectRequest({id: network.id, password: password.value, remember: remember.input.checked && network.security !== 'open', ipv6: ipv6.input.checked, widthMode: width.value});
    await act(netText, '/api/wifi-relay/connect', body, {
      timeoutMs: 5 * MINUTE,
      working: 'Connecting. This can take up to two minutes; the hotspot may briefly reconnect.',
      done: data => { password.value = ''; return `Connected to ${data?.ssid || network.ssid}. Internet is not verified; complete any hotel login in a browser first.`; }
    });
  });

  function refreshVisible() {
    if (document.hidden || dirty() || loading || busy || API.locked || !API.connected || !onRoute() || Date.now() - lastLoad < 1000) return;
    load(true);
  }
  window.addEventListener('focus', refreshVisible);
  window.addEventListener('pageshow', refreshVisible);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) { refreshVisible(); schedulePoll(); } });
  window.addEventListener('dashboard:navigate', () => { clearTimeout(pollTimer); });

  UI.track({
    route,
    get loaded() { return attempted; },
    get dirty() { return dirty(); },
    load,
    clear() {
      clearTimeout(pollTimer); clearTimeout(settleTimer);
      loadId++; settleId++;
      current = null; attempted = false; loading = false; busy = false; selectedId = '';
      password.value = ''; width.value = 'saved'; remember.input.checked = ipv6.input.checked = true;
      for (const node of [statusText, netText, savedText]) say(node, '');
      draw();
    }
  });
  draw();
})();
