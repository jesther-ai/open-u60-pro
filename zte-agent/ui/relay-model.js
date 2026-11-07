'use strict';
((globalThis) => {
  const band = frequency => Number(frequency) < 3000 ? '2.4 GHz' : '5 GHz';
  const widthLabel = mode => mode === 'auto' ? 'Auto tuning' : 'Keep negotiated bandwidth';
  const securityLabel = network => !network.supported ? 'Unsupported' : network.security === 'open' ? 'Open' : 'WPA2';
  const level = dbm => { const value = Number(dbm); return value >= -55 ? 'strong' : value >= -67 ? 'good' : value >= -78 ? 'fair' : 'weak'; };
  const ipv6Labels = {
    configured: 'Configured (internet not verified)', detecting: 'Discovering upstream IPv6', no_upstream: 'No usable upstream IPv6; IPv4 only',
    unsupported: 'Unavailable; IPv4 only', expired: 'Upstream IPv6 expired; IPv4 only', error: 'IPv6 setup failed; IPv4 only'
  };
  const ipv6Label = state => ipv6Labels[state] || 'Off';

  // One place decides what the page calls the relay, so the hero, the actions
  // and the tests cannot disagree.
  function view(status) {
    const data = status || {};
    const connected = data.connected === true, restoring = !connected && typeof data.restoring === 'string' && data.restoring !== '' ? data.restoring : '';
    const recovery = data.recovery_required === true, autostart = data.autostart === true;
    const key = connected ? 'linked' : restoring ? 'restoring' : recovery ? 'recovery' : 'off';
    const chip = {linked: 'WI-FI LINKED', restoring: 'RECONNECTING', recovery: 'RECOVERY REQUIRED', off: 'RELAY OFF'}[key];
    const title = connected ? (data.ssid || 'Wi-Fi relay') : restoring || 'Share a Wi-Fi connection';
    const detail = {
      linked: 'Connected (internet not verified). Network authentication may still be required.',
      restoring: 'Trying the last selected remembered or open network. After a reboot this can take a few minutes. Stop reconnecting cancels this attempt and clears the restart choice.',
      recovery: 'Some relay settings still need cleanup. Retry without scanning or connecting to Wi-Fi.',
      off: 'Use nearby Wi-Fi as your U60’s internet connection.'
    }[key];
    const stop = connected || recovery || restoring || autostart;
    const stopLabel = connected ? 'Disconnect relay' : recovery ? 'Retry relay cleanup' : restoring ? 'Stop reconnecting' : 'Disconnect & clear restart choice';
    return {key, chip, title, detail, connected, restoring, recovery, autostart, stop, stopLabel};
  }

  function facts(status) {
    const data = status || {};
    if (data.connected !== true) return [];
    const ipv6 = data.ipv6 || {}, rows = [['Upstream IPv4', data.address || '—', true], ['Gateway', data.gateway || '—', true]];
    const frequency = Number(data.frequency), width = Number(data.width);
    rows.push(['Upstream link', Number.isFinite(frequency) && frequency > 0 ? [band(frequency), Number.isFinite(width) && width > 0 ? `${width} MHz` : '', data.width_state || ''].filter(Boolean).join(' · ') : '—', false]);
    rows.push(['Bandwidth policy', widthLabel(data.width_mode), false]);
    rows.push(['Upstream IPv6 / NAT66', ipv6Label(ipv6.state), false]);
    if (ipv6.upstream === true) rows.push(['Upstream IPv6', ipv6.address || '—', true], ['LAN IPv6 prefix', ipv6.lan_prefix || '—', true]);
    rows.push(['After a restart', data.autostart === true ? 'Automatic reconnect enabled' : data.autostart === false ? 'Stays off; no restart choice saved' : 'Unknown', false]);
    return rows;
  }

  const hex64 = /^[0-9a-fA-F]{64}$/;
  // Mirrors the router’s WPA2 rule so a typo is caught before a hotspot interruption.
  function passwordProblem(network, password, remembered) {
    const secret = String(password || '');
    if (!network) return 'Select a network first.';
    if (!network.supported) return 'This network is not supported.';
    if (network.security === 'open') return secret ? 'Open networks do not use a password.' : '';
    if (!secret) return remembered ? '' : 'Enter the Wi-Fi password.';
    if (hex64.test(secret)) return '';
    if (secret.length < 8 || secret.length > 63 || !/^[\x20-\x7e]+$/.test(secret)) return 'WPA2 passwords are 8–63 printable ASCII characters, or a 64-digit hex key.';
    return '';
  }

  function connectRequest({id, password, remember, ipv6, widthMode}) {
    const body = {network: id, password: String(password || ''), confirmed: true, ipv6: ipv6 !== false, remember: remember === true};
    if (widthMode === 'off' || widthMode === 'auto') body.width_mode = widthMode;
    return body;
  }

  const savedNote = entry => entry.security === 'open' ? 'Open network; no password needed' : entry.remembered ? 'Password remembered; leave it empty when connecting' : 'Password not saved; enter it when connecting';

  function consent(network, status) {
    const replacing = status && status.connected === true ? ` This replaces the current relay (${status.ssid || 'connected network'}).` : '';
    return `Connect the U60 to ${network.ssid} (${band(network.frequency)})? Hotspot clients, including this console if you reached it over Wi-Fi, may briefly reconnect. If the upstream drops, SIM data is used only when cellular data is already on.${replacing}`;
  }

  const usable = networks => (Array.isArray(networks) ? networks : []).filter(network => network && typeof network.id === 'string').map((network, index) => ({network, index}))
    .sort((a, b) => (b.network.supported === true) - (a.network.supported === true) || a.index - b.index).map(item => item.network);

  const api = {band, widthLabel, securityLabel, level, ipv6Label, view, facts, passwordProblem, connectRequest, savedNote, consent, usable};
  if (typeof module === 'object' && module.exports) module.exports = api; else globalThis.OpenU60Relay = api;
})(globalThis);
