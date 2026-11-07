'use strict';
((globalThis) => {
  const MAX_CONFIG = 512 * 1024, MAX_EXTENSION = 128 * 1024, MAX_NAME = 128;
  const intervals = [[0, 'Manual only'], [1, 'Every hour'], [6, 'Every 6 hours'], [12, 'Every 12 hours'], [24, 'Every day'], [72, 'Every 3 days'], [168, 'Every week']];
  const bytes = text => new TextEncoder().encode(String(text ?? '')).length;

  // ── Lifecycle ───────────────────────────────────────────────────────────
  function view(status) {
    const data = status || {};
    const running = data.running === true, recovery = data.recovery_required === true, installed = data.installed === true, configured = data.configured === true, autostart = data.autostart === true;
    const key = running ? 'running' : recovery ? 'recovery' : 'off';
    const chip = {running: 'RUNNING', recovery: 'RECOVERY REQUIRED', off: 'OFF'}[key];
    const title = running ? 'Clash is running' : recovery ? 'Cleanup required' : 'Clash is off';
    const detail = running
      ? 'LAN traffic is intercepted by Mihomo over a private TUN. Turning it off returns to direct routing; it is not a kill switch.'
      : recovery ? 'Some proxy network changes remain. Retry cleanup before starting or reinstalling. This also clears automatic start.'
      : !installed ? 'Install the pinned Mihomo core first.' : !configured ? 'Add and select a configuration profile first.' : 'Ready to start. Experimental LAN TCP/UDP, IPv4/IPv6 interception.';
    const canStart = running || (!recovery && installed && configured);
    return {
      key, chip, title, detail, running, recovery, installed, configured, autostart, canStart,
      toggleLabel: running ? 'Stop Clash' : 'Start Clash',
      toggleHint: running || canStart ? '' : recovery ? 'Retry cleanup first.' : !installed ? 'Install the core first.' : 'Add a configuration profile first.',
      showInstall: !running && !recovery,
      installLabel: installed ? 'Reinstall pinned core' : 'Install Mihomo core',
      showClearAutostart: !running && autostart && !recovery
    };
  }
  const autostartLabel = status => status?.autostart === true ? 'Automatic start enabled' : status?.autostart === false ? 'Stays off' : 'Unknown';
  function facts(status) {
    const data = status || {};
    return [
      ['Core', data.installed === true ? `Installed${data.version ? ` · ${data.version}` : ''}` : 'Not installed'],
      ['Configuration', data.configured === true ? 'Saved privately' : 'Not configured'],
      ['After a restart', autostartLabel(data)],
      ['Scope', 'LAN TCP/UDP · IPv4/IPv6']
    ];
  }

  // ── Profiles ────────────────────────────────────────────────────────────
  const updatedLabel = (seconds, now = Date.now() / 1000) => {
    const at = Number(seconds);
    if (!Number.isFinite(at) || at <= 0 || at > 253402300800) return 'Never updated';
    const age = Math.max(0, now - at);
    return age < 60 ? 'Updated just now' : age < 3600 ? `Updated ${Math.floor(age / 60)} min ago` : age < 86400 ? `Updated ${Math.floor(age / 3600)} h ago` : `Updated ${Math.floor(age / 86400)} d ago`;
  };
  const intervalLabel = hours => (intervals.find(([value]) => value === Number(hours)) || [0, 'Manual only'])[1];

  // The router refuses anything else, so a mistake is caught before a download starts.
  function subscriptionProblem(raw) {
    const text = String(raw || '').trim();
    if (!text) return 'Enter the subscription URL.';
    if (text.length > 4096) return 'The subscription URL is too long.';
    let url;
    try { url = new URL(text); } catch { return 'Enter a valid HTTPS URL.'; }
    if (url.protocol !== 'https:' || !url.hostname) return 'Use an HTTPS URL.';
    if (url.username || url.password) return 'Use an HTTPS URL without a username or password.';
    if (url.hash) return 'Use an HTTPS URL without a #fragment.';
    return '';
  }
  function profileProblem({mode, type, name, url, yaml, hasSavedYaml}) {
    const label = String(name || '').trim();
    if (!label || bytes(label) > MAX_NAME || /[\u0000-\u001f\u007f]/.test(label)) return 'Enter a profile name (1–128 bytes).';
    if (type === 'remote') return subscriptionProblem(url);
    if (type !== 'local') return 'Choose a subscription or a pasted configuration.';
    if (!String(yaml || '').trim()) return mode === 'edit' && hasSavedYaml ? '' : 'Paste or choose a YAML configuration.';
    if (bytes(yaml) > MAX_CONFIG) return 'The configuration is larger than 512 KiB.';
    return '';
  }
  function profileRequest({mode, id, type, name, url, yaml, interval}) {
    const remote = type === 'remote';
    const body = {action: mode, name: String(name || '').trim(), source_type: remote ? 'remote' : 'local', url: remote ? String(url || '').trim() : '', yaml: remote ? '' : String(yaml || ''), interval_hours: remote ? Number(interval) || 0 : 0};
    if (mode === 'edit') body.id = id;
    return body;
  }
  const profileAction = (action, id) => ({action, id});
  const scheduleRequest = (id, hours) => ({action: 'schedule', id, interval_hours: Number(hours) || 0});

  function confirmation(action, profile, running) {
    const name = profile?.name || 'this profile', reconnect = running ? ' Clash briefly reconnects.' : '';
    return {
      select: [`Use ${name}?`, `It becomes the active configuration after it validates.${reconnect}`, 'Use profile'],
      refresh: [`Update ${name}?`, `The subscription is downloaded and validated before it replaces the saved copy. A failed download keeps the old one.${reconnect}`, 'Update now'],
      edit: [`Save ${name}?`, `The configuration is validated first; an invalid one is rejected and the saved profile stays.${reconnect}`, 'Save profile'],
      add: [`Add ${name}?`, 'The configuration is downloaded or checked first and saved only if it validates. A subscription download uses Clash when it runs, otherwise the current uplink; cellular data is never turned on.', 'Add profile'],
      delete: [`Delete ${name}?`, 'The saved source and subscription URL are removed. Turn Clash off or select another profile before deleting the running one.', 'Delete profile']
    }[action];
  }

  function extensionsProblem({merge, script}) {
    if (bytes(merge) > MAX_EXTENSION || bytes(script) > MAX_EXTENSION) return 'Each extension is limited to 128 KiB.';
    return '';
  }
  const extensionsRequest = ({merge_enabled, script_enabled, merge, script}) => ({action: 'extensions', extensions: {merge_enabled: merge_enabled === true, script_enabled: script_enabled === true, merge: String(merge ?? ''), script: String(script ?? '')}});

  // ── Proxy groups ────────────────────────────────────────────────────────
  // The core records a failed test as 0 ms; null or a missing value means it was never tested.
  const delayLabel = delay => typeof delay !== 'number' ? 'Untested' : delay > 0 ? `${delay} ms` : 'Timeout';
  const delayLevel = delay => typeof delay !== 'number' ? 'none' : delay <= 0 ? 'slow' : delay < 150 ? 'good' : delay < 400 ? 'fair' : 'slow';
  const modes = [['rule', 'Rule'], ['global', 'Global'], ['direct', 'Direct']];
  const modeLabel = mode => (modes.find(([value]) => value === mode) || ['', 'Rule'])[1];

  // A group’s `now` can be another group; follow it to the node that carries traffic.
  function resolveNode(groups, name, depth = 8) {
    const byName = new Map((groups || []).map(group => [group.name, group]));
    let current = name;
    for (let step = 0; step < depth; step++) {
      const group = byName.get(current);
      if (!group) return current || '';
      if (!group.now) return '';
      current = group.now;
    }
    return '';
  }
  function memberOptions(group, nodes) {
    return (group.all || []).map(name => {
      const node = nodes?.[name];
      return {name, label: node ? `${name} · ${delayLabel(node.delay)}` : name};
    });
  }
  function filterGroups(groups, query) {
    const needle = String(query || '').trim().toLowerCase();
    if (!needle) return groups || [];
    return (groups || []).filter(group => group.name.toLowerCase().includes(needle) || (group.all || []).some(name => name.toLowerCase().includes(needle)));
  }
  // Run `worker` over `items` with at most `limit` in flight; results keep order.
  async function limited(items, limit, worker) {
    const results = new Array(items.length);
    let next = 0, failed = false, failure;
    const lane = async () => {
      while (!failed && next < items.length) {
        const index = next++;
        try { results[index] = await worker(items[index], index); }
        catch (error) { if (!failed) failure = error; failed = true; }
      }
    };
    await Promise.all(Array.from({length: Math.max(1, Math.min(limit, items.length))}, lane));
    if (failed) throw failure;
    return results;
  }

  // The only address the router may send the browser to: its own Zashboard.
  const DASHBOARD = '/ui/mihomo/dashboard/';
  function dashboardTarget(data) {
    const url = data && typeof data.url === 'string' ? data.url : '';
    return url.startsWith(DASHBOARD) && !/[\\\s]/.test(url) ? url : '';
  }

  const api = {dashboardTarget, intervals, view, autostartLabel, facts, updatedLabel, intervalLabel, subscriptionProblem, profileProblem, profileRequest, profileAction, scheduleRequest, confirmation, extensionsProblem, extensionsRequest, delayLabel, delayLevel, modes, modeLabel, resolveNode, memberOptions, filterGroups, limited, MAX_CONFIG, MAX_EXTENSION};
  if (typeof module === 'object' && module.exports) module.exports = api; else globalThis.OpenU60Mihomo = api;
})(globalThis);
