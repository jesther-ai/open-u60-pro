'use strict';
(() => {
  const route = 'network/proxies', UI = OpenU60UI, API = OpenU60Console, M = OpenU60Mihomo;
  const {el, button, request, json, confirm, mutate, say} = UI;
  const GROUP_TEST_LIMIT = 30, TEST_PARALLEL = 3;

  const modePanel = UI.panel(route, 'proxies-mode', 'Clash proxies', 'Choose how Clash routes traffic and which node each group uses. Changes take effect for new connections; existing ones finish on their old route.');
  modePanel.classList.add('proxies-panel');
  const modeRow = el('div', 'proxies-mode-row'), modeLabel = el('label', '', 'Routing mode'), modeSelect = el('select');
  modeSelect.id = 'proxies-mode-select'; modeLabel.htmlFor = modeSelect.id;
  for (const [value, text] of M.modes) modeSelect.add(new Option(text, value));
  const modeHelp = el('small', 'field-help', 'Rule follows your profile’s rules. Global sends everything through the GLOBAL group. Direct bypasses nodes (this is not a kill switch).');
  const refresh = button('Read current state', () => load()), openClash = button('Open Clash / Mihomo', () => OpenU60Navigation.go('network/mihomo'));
  openClash.hidden = true;
  const zashboard = button('Open Zashboard', () => M.openDashboard(modeText, zashboard));
  zashboard.hidden = true;
  modeRow.append(modeLabel, modeSelect, refresh, zashboard, openClash);
  const modeText = UI.status(modePanel);
  modePanel.append(modeRow, modeHelp, modeText);

  const groupPanel = UI.panel(route, 'proxies-groups', 'Proxy groups', 'Selector groups let you pick a node. URL-test, fallback and load-balance groups choose by themselves. Delay tests fetch a small page through the node and use a little data.');
  const searchRow = el('div', 'proxies-search'), search = el('input');
  search.id = 'proxies-search'; search.type = 'search'; search.placeholder = 'Filter groups and nodes'; search.autocomplete = 'off'; search.spellcheck = false;
  search.setAttribute('aria-label', 'Filter groups and nodes');
  searchRow.append(search);
  const groupList = el('div', 'proxies-group-list'), groupText = UI.status(groupPanel);
  groupPanel.append(searchRow, groupList, groupText);

  let data = null, off = false, loading = false, attempted = false, busy = false, lastLoad = 0;
  let loadId = 0;
  const onRoute = () => OpenU60Navigation.current.key === route;
  const nodes = () => data?.nodes || {};

  function drawMode() {
    modeSelect.value = data?.mode || 'rule';
    modeSelect.disabled = !data || busy || !API.connected;
    openClash.hidden = !off;
    zashboard.hidden = !data;
    zashboard.disabled = !API.connected;
    refresh.disabled = loading || busy || !API.connected;
  }
  function drawGroups() {
    const groups = M.filterGroups(data?.groups || [], search.value);
    if (!data) {
      groupList.replaceChildren(el('p', 'empty', off ? 'Clash is off. Start it from Clash / Mihomo to see its proxy groups.' : 'Proxy groups appear here once they are read.'));
      return;
    }
    if (!groups.length) { groupList.replaceChildren(el('p', 'empty', data.groups.length ? 'No group or node matches the filter.' : 'The running configuration has no proxy groups.')); return; }
    groupList.replaceChildren(...groups.map(group => {
      const card = el('article', 'proxy-group'), head = el('div', 'proxy-group-head'), copy = el('div'), now = M.resolveNode(data.groups, group.name), node = nodes()[now];
      card.dataset.group = group.name;
      copy.append(el('h3', '', group.name), el('p', 'field-help', group.now ? `Now: ${group.now}${group.now !== now && now ? ` → ${now}` : ''}` : 'No node selected'));
      head.append(copy, el('span', 'tag', group.type));
      const delay = el('span', 'proxy-delay', node ? M.delayLabel(node.delay) : '');
      delay.dataset.level = node ? M.delayLevel(node.delay) : 'none';
      delay.hidden = !node;
      head.append(delay);
      const controls = el('div', 'form-actions');
      if (group.selectable) {
        const select = el('select');
        select.setAttribute('aria-label', `Node for ${group.name}`);
        for (const option of M.memberOptions(group, nodes())) select.add(new Option(option.label, option.name));
        select.value = group.now || '';
        select.addEventListener('change', () => choose(group, select.value, select));
        select.disabled = busy || !API.connected;
        controls.append(select);
      } else controls.append(el('span', 'field-help', 'Chosen automatically by Clash.'));
      const testOne = button('Test delay', () => test(group, [now])), testAll = button('Test group', () => test(group, group.all));
      testOne.disabled = busy || !API.connected || !now || !nodes()[now];
      testAll.disabled = busy || !API.connected || !group.all.some(name => nodes()[name]);
      controls.append(testOne, testAll);
      card.append(head, controls);
      return card;
    }));
  }
  function draw() { drawMode(); drawGroups(); }

  async function load(quiet = false) {
    if (loading || busy || API.locked || !API.connected) return;
    const epoch = UI.epoch, id = ++loadId;
    loading = true; attempted = true; lastLoad = Date.now(); refresh.disabled = true;
    let failure = '';
    try { const result = await request('/api/mihomo/proxies', {timeoutMs: 15000}); if (!UI.current(epoch) || id !== loadId) return; data = result; off = false; } catch (error) {
      if (!UI.current(epoch) || id !== loadId) return;
      data = null; off = error.status === 409;
      if (!off) failure = `Proxy groups unavailable. ${error.message}`;
    } finally {
      if (!UI.current(epoch) || id !== loadId) return;
      loading = false; draw();
      if (failure) say(modeText, failure, true); else if (!quiet) say(modeText, '');
    }
  }
  async function change(node, operation) {
    await mutate(node, async live => {
      loadId++; loading = false; busy = true; draw();
      try { await operation(live); }
      catch (error) { if (live()) { say(node, error.status ? error.message : `The router did not answer (${error.message}). The change may already have been applied; read the current state.`, true); if (error.status === 409) { data = null; off = true; } } }
      finally { busy = false; if (live()) draw(); }
    });
  }

  modeSelect.addEventListener('change', async () => {
    const mode = modeSelect.value, epoch = UI.epoch;
    modeSelect.value = data?.mode || 'rule';
    if (!await confirm(`Use ${M.modeLabel(mode)} mode?`, 'This changes routing for new LAN connections. Direct bypasses proxy nodes.', {label:'Change mode'}) || !UI.current(epoch)) return;
    change(modeText, async live => {
      say(modeText, 'Changing mode…');
      const result = await request('/api/mihomo/mode', {...json('POST', {mode}), timeoutMs: 15000});
      if (!live()) return;
      data = {...data, mode: result.mode};
      say(modeText, `Mode is now ${M.modeLabel(result.mode)}.`);
    }).then(() => { if (data) modeSelect.value = data.mode; });
  });
  async function choose(group, name, select) {
    const epoch = UI.epoch;
    select.value = group.now || '';
    if (!await confirm(`Switch ${group.name} to ${name}?`, 'New connections use the selected node; existing connections keep their current route.', {label:'Switch node'}) || !UI.current(epoch)) return;
    return change(groupText, async live => {
      say(groupText, `Switching ${group.name} to ${name}…`);
      try {
        const result = await request('/api/mihomo/proxies/select', {...json('POST', {group: group.name, name}), timeoutMs: 15000});
        if (!live()) return;
        const current = data?.groups.find(item => item.name === group.name);
        if (current) current.now = result.now;
        say(groupText, `${group.name} now uses ${result.now}.`);
      } catch (error) { select.value = group.now || ''; throw error; }
    });
  }
  function test(group, names) {
    const targets = [...new Set(names.filter(name => nodes()[name]))].slice(0, GROUP_TEST_LIMIT);
    if (!targets.length) return Promise.resolve();
    return change(groupText, async live => {
      let finished = 0;
      say(groupText, `Testing ${targets.length} node${targets.length === 1 ? '' : 's'}…`);
      await M.limited(targets, TEST_PARALLEL, async name => {
        if (!live()) return;
        const result = await request('/api/mihomo/proxies/delay', {...json('POST', {name}), timeoutMs: 20000});
        if (!live()) return;
        nodes()[name].delay = result.delay;
        say(groupText, `Tested ${++finished} of ${targets.length}…`);
      });
      if (live()) say(groupText, `Tested ${targets.length} node${targets.length === 1 ? '' : 's'}.${names.length > targets.length ? ` Only the first ${GROUP_TEST_LIMIT} were tested.` : ''}`);
    });
  }
  search.addEventListener('input', drawGroups);

  function refreshVisible() {
    if (document.hidden || loading || busy || API.locked || !API.connected || !onRoute() || Date.now() - lastLoad < 1000) return;
    load(true);
  }
  window.addEventListener('focus', refreshVisible);
  window.addEventListener('pageshow', refreshVisible);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refreshVisible(); });

  UI.track({
    route,
    get loaded() { return attempted; },
    get dirty() { return false; },
    load,
    clear() { loadId++; data = null; off = false; attempted = loading = busy = false; search.value = ''; say(modeText, ''); say(groupText, ''); draw(); }
  });
  draw();
})();
