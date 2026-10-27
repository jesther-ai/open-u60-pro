'use strict';
// Shared by the browser and parser tests; modem field formats match the native apps.
(() => {
  const scalar = value => value == null || /^(?:--|N\/A|NONE|NULL)$/i.test(String(value).trim()) ? '' : String(value).trim();
  const metric = (value, power = false) => {
    const raw = scalar(value), n = Number(raw);
    return !raw || !Number.isFinite(n) || Math.abs(n) > 9000 || (power && n === 0) ? null : n;
  };
  const band = (value, rat) => {
    const raw = scalar(value);
    if (!raw || raw === '0') return '';
    const match = raw.match(/^(?:B|n)?(\d+)$/i);
    return match ? `${rat === 'nr' ? 'n' : 'B'}${Number(match[1])}` : raw;
  };
  const positive = value => scalar(value) !== '0' ? scalar(value) : '';
  function radio(data, rat) {
    const nr = rat === 'nr', prefix = nr ? 'nr5g' : 'lte';
    const primary = {
      role: 'PCC', band: band(nr ? data.nr5g_action_band : data.wan_active_band, rat),
      pci: scalar(data[`${prefix}_pci`]), channel: positive(nr ? data.nr5g_action_channel : data.wan_active_channel),
      bandwidth: positive(nr ? data.nr5g_bandwidth : ''), cellId: positive(nr ? data.nr5g_cell_id : data.cell_id),
      rsrp: metric(data[`${prefix}_rsrp`], true), rsrq: metric(data[`${prefix}_rsrq`]),
      sinr: metric(data[`${prefix}_snr`]), rssi: metric(data[`${prefix}_rssi`], true)
    };
    const rows = scalar(data[nr ? 'nrca' : 'lteca']).split(';').filter(row => row.trim()).map(row => {
      const parts = row.split(',').map(part => part.trim());
      if (parts.length < 5 || !positive(parts[1]) || !positive(parts[3])) return null;
      return {pci: scalar(parts[0]), band: band(parts[1], rat), channel: positive(parts[3]), bandwidth: positive(parts[4])};
    });
    const signals = scalar(data[nr ? 'nrcasig' : 'ltecasig']).split(';').filter(row => row.trim()).map(row => {
      const parts = row.split(',');
      return parts.length < 4 ? null : {rsrp: metric(parts[0], true), rsrq: metric(parts[1]), sinr: metric(parts[2]), rssi: metric(parts[3], true)};
    });
    // PCI is reused between cells: match the primary using PCI AND channel.
    const pccIndex = rows.findIndex(row => row && primary.pci && primary.channel && row.pci === primary.pci && row.channel === primary.channel);
    if (pccIndex >= 0) {
      primary.bandwidth = rows[pccIndex].bandwidth || primary.bandwidth;
      primary.band = primary.band || rows[pccIndex].band;
    }
    const carriers = [];
    if (primary.band || primary.channel || primary.cellId || primary.rsrp !== null) carriers.push(primary);
    const seen = new Set(carriers.map(carrier => `${carrier.pci}|${carrier.channel}|${carrier.band}`));
    let signalIndex = 0;
    for (const [index, row] of rows.entries()) {
      if (index === pccIndex) continue;
      const measurements = signals[signalIndex++];
      if (!row) continue;
      const key = `${row.pci}|${row.channel}|${row.band}`;
      if (seen.has(key)) continue;
      seen.add(key);
      carriers.push({role: `SCC ${carriers.filter(carrier => carrier.role !== 'PCC').length + 1}`, ...row,
        cellId: '', rsrp: null, rsrq: null, sinr: null, rssi: null, ...measurements});
    }
    return {carriers, measured: carriers.some(carrier => carrier.rsrp !== null), caInactive: !nr && scalar(data.lteca_state) === '0'};
  }
  function parse(data = {}) {
    const lte = radio(data, 'lte'), nr = radio(data, 'nr');
    const raw = scalar(data.network_type), normalized = raw.toUpperCase().replace(/[\s_-]/g, '');
    const nsa = normalized.includes('NSA') || normalized === 'ENDC';
    const sa = !nsa && (normalized === 'SA' || /(?:NR5G|5G|NR)SA/.test(normalized));
    const lteHint = /LTE|4G/.test(normalized);
    let mode = 'Not reported', explanation = '';
    if (sa) { mode = '5G SA'; explanation = 'Standalone NR · LTE is not an anchor in this mode.'; }
    else if (nr.measured && (nsa || lteHint || lte.measured)) {
      mode = '5G NSA'; explanation = nsa ? 'NR with an LTE anchor.' : 'NSA inferred from the reported LTE and NR measurements.';
    } else if (lte.measured || lteHint) {
      mode = !lte.measured ? '4G · no signal' : lte.carriers.length > 1 && !lte.caInactive ? '4G+' : '4G';
      explanation = !lte.measured ? 'Reported LTE mode; no serving signal is currently measured.' : nsa ? 'The modem reports NSA, but no NR signal is currently measured.' : 'LTE connection.';
    } else if (nr.measured) {
      mode = '5G · mode unreported'; explanation = 'NR is measured; the modem has not confirmed SA or NSA.';
    } else if (nsa) { mode = '5G NSA · no signal'; explanation = 'Reported access mode; no serving signal is currently measured.'; }
    else if (/NO.?SERVICE|OFFLINE|DISCONNECTED|SEARCH/.test(normalized)) { mode = 'No service'; }
    else if (raw) { mode = raw; }
    lte.inUse = !sa && lte.measured;
    nr.inUse = nr.measured;
    const primary = (nr.inUse ? nr : lte.inUse ? lte : {carriers:[]}).carriers.find(carrier => carrier.rsrp !== null) || null;
    return {lte, nr, mode, explanation, primary, sa, hasSignal: nr.measured || lte.inUse};
  }
  const api = {parse};
  if (typeof module === 'object' && module.exports) module.exports = api;
  else globalThis.OpenU60Cellular = api;
})();
