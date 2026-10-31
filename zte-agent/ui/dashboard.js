'use strict';
(() => {
  const $ = id => document.getElementById(id);
  const animated = new Set(['download','upload','battery','temperature','rsrp','sinr','rsrq','lte-count','nr-count','device-count','outage-count','received','sent','sms-count']);
  const text = (id,value) => { if (animated.has(id)) OpenU60Numbers.set($(id),value); else $(id).textContent = value ?? '—'; };
  const number = value => value !== null && value !== undefined && String(value).trim() !== '' && Number.isFinite(Number(value)) ? Number(value) : null;
  const sessionKey = 'openu60.session';
  let token = '', authEpoch = 0, locked = false, clients = [], history = [];
  try { const saved = sessionStorage.getItem(sessionKey); if (saved && saved.length <= 2048 && !/[\s\x00-\x1f]/.test(saved)) token = saved; } catch { /* The in-memory session still works when storage is blocked. */ }
  let controller, reconnectTimer, watchdog, epoch = 0, retryDelay = 1000, streamIncludesSms = false, streamKey = '';
  let messages = [];
  const readings = new Map(), lastGood = new Map();
  let transportLive = false, lastReceived = null, lastServing = null, outageStarted = null, outageCount = 0;
  let lastOutageDuration = 0, lastOutageReason = '';
  const dialog = $('login-dialog');
  text('address', location.host);
  text('settings-address', location.origin);
  $('signout').hidden = !token;

  async function api(path, options = {}) {
    const requestAuthEpoch = authEpoch;
    const {timeoutMs = 8000, ...fetchOptions} = options;
    const response = await fetch(path, {
      ...fetchOptions, cache: 'no-store', signal: AbortSignal.timeout(Math.min(Math.max(timeoutMs,1000),65000)),
      headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(options.body ? { 'Content-Type': 'application/json' } : {}) }
    });
    if (response.status === 401) {
      const error = new Error('Enter your agent password to continue.');
      error.auth = true;
      error.authEpoch = requestAuthEpoch;
      throw error;
    }
    const result = await response.json();
    if (!response.ok || !result.ok) { const error = new Error(result.error || 'Reading unavailable'); error.status = response.status; error.details=result; throw error; }
    return result.data ?? result;
  }
  function lock() {
    authEpoch++;
    try { sessionStorage.removeItem(sessionKey); } catch { /* Storage can be unavailable. */ }
    stopStream();
    token = ''; locked = true; clients = []; history = [];
    transportLive = false; readings.clear(); lastGood.clear(); lastReceived = null; lastServing = null;
    outageStarted = null; outageCount = 0; lastOutageDuration = 0; lastOutageReason = '';
    $('availability').hidden = true; $('cellular').dataset.stale = 'false';
    text('last-seen', '—'); text('outage-count', '0'); text('outage-duration', '—');
    setStreamState('locked', 'Locked');
    renderDevice(null); renderWan(null); renderSignal(null); renderModem(null); renderSpeed(null); renderBattery(null); renderWifi(null); renderClients(null);
    $('sms-search').value = ''; renderSMS(null); renderSMSCapacity(null);
    text('connection-title', 'Console locked');
    text('updated', 'Console locked'); text('notice', 'Unlock the console to view your network.');
    $('signout').hidden = true;
    window.dispatchEvent(new Event('dashboard:locked'));
    OpenU60Motion.showDialog(dialog);
  }
  const bytes = value => {
    const n = number(value);
    if (n === null || n < 0) return '—';
    const unit = n >= 1e9 ? 'GB' : n >= 1e6 ? 'MB' : n >= 1e3 ? 'KB' : 'B';
    return `${(n / ({ GB: 1e9, MB: 1e6, KB: 1e3, B: 1 }[unit])).toFixed(unit === 'B' ? 0 : 2)} ${unit}`;
  };
  function renderDevice(data) {
    text('hostname', data?.hostname ?? 'ZTE U60 Pro'); text('kernel', data?.kernel);
    text('load', Array.isArray(data?.load_avg) ? data.load_avg.join(' / ') : data?.load_avg);
    const seconds = number(data?.uptime_secs);
    text('uptime', seconds === null ? null : `${Math.floor(seconds / 86400)}d ${Math.floor(seconds % 86400 / 3600)}h ${Math.floor(seconds % 3600 / 60)}m`);
  }
  function renderWan(data) {
    const wan=lastGood.get('wan')?.data,wan6=lastGood.get('wan6')?.data;
    const addresses=OpenU60Addresses.collect(wan,wan6);
    text('wan-ip',addresses.ipv4.join('\n') || (wan || wan6 ? 'Not reported' : '—'));
    text('wan-ipv6',addresses.ipv6.join('\n') || (wan || wan6 ? 'Not reported' : '—'));
    text('wan-ipv6-status',addresses.ipv6.length && (readings.get('wan6')?.ok===false || wan6?.up===false) ? 'Last reported IPv6 address' : '');
  }
  function renderModem(data) { text('modem-state', data?.operate_mode); }
  function element(tag, className, value) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (value !== undefined) node.textContent = value;
    return node;
  }
  function carrierGroup(rat, group, model, historical = false) {
    const list = $(`${rat}-carriers`), existing = new Map([...list.querySelectorAll('.carrier-card')].map(card=>[card.dataset.carrierKey,card]));
    const count = group.carriers.length;
    let state = !count ? 'Not reported' : `${count} reported · ${count > 1 ? 'carrier aggregation' : 'single carrier'}`;
    if (group.caInactive && count > 1) state = `${count} reported · CA inactive`;
    if (rat === 'lte' && model.sa && count) state = `${count} reported · not used in SA`;
    else if (count && !group.measured) state = `${count} reported · no measured signal`;
    if (historical && count) state = `Last known · ${count} carriers`;
    text(`${rat}-status`, state);
    if (!count) { list.replaceChildren(element('p','carrier-empty',`No ${rat === 'nr' ? 'NR' : 'LTE'} carriers reported by the modem.`)); return; }
    const cards = [];
    for (const carrier of group.carriers) {
      const key = JSON.stringify([rat,carrier.pci,carrier.channel,carrier.band,carrier.cellId]);
      let card = existing.get(key);
      if (!card) {
        card = element('article','carrier-card'); card.dataset.carrierKey = key;
        const heading = element('div','carrier-card-heading'), identity = element('div','carrier-identity');
        identity.append(element('strong',''),element('span','carrier-role')); heading.append(identity,element('span','carrier-measurement-state'));
        const facts = element('dl','carrier-facts'), metrics = element('dl','carrier-metrics');
        for (const label of ['PCI',rat === 'nr' ? 'NR-ARFCN' : 'EARFCN','Bandwidth','Cell ID']) { const fact = element('div'); fact.append(element('dt','',label),element('dd')); facts.append(fact); }
        for (const label of ['RSRP','RSRQ','SINR','RSSI']) { const metric = element('div'); metric.append(element('dt','',label),element('dd')); metrics.append(metric); }
        card.append(heading,facts,metrics);
      }
      card.dataset.role = carrier.role;
      card.querySelector('.carrier-identity strong').textContent = carrier.band || 'Band unreported';
      card.querySelector('.carrier-role').textContent = carrier.role;
      card.querySelector('.carrier-measurement-state').textContent = carrier.rsrp === null ? 'Measurements unavailable' : 'Signal reported';
      const facts = [carrier.pci,carrier.channel,carrier.bandwidth ? `${carrier.bandwidth} MHz` : '',carrier.cellId];
      [...card.querySelectorAll('.carrier-facts dd')].forEach((node,index)=>{ node.textContent = facts[index] || '—'; });
      [...card.querySelectorAll('.carrier-metrics dd')].forEach((node,index)=>{
        const [key,unit] = [['rsrp','dBm'],['rsrq','dB'],['sinr','dB'],['rssi','dBm']][index];
        OpenU60Numbers.set(node,carrier[key] === null ? '—' : `${carrier[key]} ${unit}`);
      });
      cards.push(card);
    }
    const wanted = new Set(cards);
    for (const child of [...list.children]) if (!wanted.has(child)) child.remove();
    cards.forEach((card,index)=>{ if (list.children[index] !== card) list.insertBefore(card,list.children[index] || null); });
  }
  const signalQuality = rsrp => rsrp == null ? 'Unavailable' : rsrp >= -80 ? 'Excellent' : rsrp >= -95 ? 'Good' : rsrp >= -110 ? 'Fair' : 'Weak';
  function renderSignal(data) {
    const model = OpenU60Cellular.parse(data || {});
    text('operator', data?.network_provider ?? 'Operator unavailable'); text('network-type', data ? model.mode : '—');
    text('access-mode', data ? model.mode : '—'); text('mode-explanation', model.explanation || 'No serving signal reported by the modem.');
    text('lte-count', data ? model.lte.carriers.length : '—'); text('nr-count', data ? model.nr.carriers.length : '—');
    const primary = model.primary;
    text('primary-signal-label', primary ? `MEASURED CARRIER · ${primary.band || 'BAND UNREPORTED'} ${primary.role}` : 'SERVING SIGNAL');
    text('rsrp', primary?.rsrp); text('sinr', primary?.sinr); text('rsrq', primary?.rsrq);
    text('band', primary?.band); text('pci', primary?.pci);
    const rsrp = primary?.rsrp;
    text('signal-quality', signalQuality(rsrp));
    const historical = !model.hasSignal && lastServing;
    const inventory = historical ? OpenU60Cellular.parse(lastServing.data) : model;
    $('cellular').dataset.nrActive = String(inventory.nr.inUse);
    carrierGroup('lte', inventory.lte, inventory, Boolean(historical)); carrierGroup('nr', inventory.nr, inventory, Boolean(historical));
    text('cellular-freshness', data ? 'Live modem readings · Every reported carrier is listed below.' : 'No modem readings available.');
  }
  const clock = value => new Date(value).toLocaleTimeString([], {hour:'2-digit',minute:'2-digit',second:'2-digit'});
  function refreshOutageClock() {
    const seconds = Math.max(0, Math.floor((outageStarted === null ? lastOutageDuration : Date.now() - outageStarted) / 1000));
    text('outage-duration', `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2,'0')}s`);
  }
  function updateConnection() {
    if (locked) return;
    const wan = readings.get('wan'), modem = readings.get('modem'), signal = readings.get('signal');
    const radioMode = String(modem?.ok ? modem.data?.operate_mode ?? '' : '').trim().toUpperCase();
    let reason = '';
    if (!transportLive) reason = lastReceived ? 'Router unreachable' : 'Waiting for router';
    else if (radioMode && radioMode !== 'ONLINE') reason = `Radio offline · ${radioMode}`;
    else if (wan?.ok && wan.data?.up === false) reason = 'Mobile data offline';
    else if (signal?.ok && OpenU60Cellular.parse(signal.data || {}).mode === 'No service') reason = 'Cellular service unavailable';
    else if (!wan?.ok) reason = 'Mobile data status unavailable';
    const interrupted = reason && (transportLive || lastReceived !== null);
    if (interrupted && outageStarted === null) { outageStarted = Date.now(); outageCount++; }
    if (!interrupted && outageStarted !== null) { lastOutageDuration = Date.now() - outageStarted; outageStarted = null; }
    if (interrupted) lastOutageReason = reason;
    text('connection-title', reason || 'Cellular connection active');
    document.querySelector('.connection-panel').dataset.state = interrupted ? 'offline' : 'online';
    $('availability').hidden = !outageCount;
    $('history-empty').hidden = Boolean(outageCount);
    text('availability-title', interrupted ? reason : 'Connection restored');
    text('offline-cause', !transportLive ? 'Showing last known readings from this tab. Live rates are unavailable.' : interrupted ? 'The router is reachable. Local device statistics continue to update.' : `Recovered from: ${lastOutageReason}. History covers this tab only.`);
    text('last-seen', lastReceived ? clock(lastReceived) : '—');
    text('outage-count', outageCount); text('outage-duration-label', interrupted ? 'Current interruption' : 'Last interruption');
    refreshOutageClock();
    document.querySelectorAll('[data-rate-label]').forEach(label => { label.textContent = interrupted ? 'OFFLINE' : 'LIVE'; });
    if (interrupted) { text('download', '—'); text('upload', '—'); }
    const oldSignal = lastGood.get('signal');
    const noCurrentSignal = Boolean(signal?.ok && !OpenU60Cellular.parse(signal.data || {}).hasSignal && lastServing);
    const stale = Boolean(oldSignal && (!transportLive || !signal?.ok || noCurrentSignal || (radioMode && radioMode !== 'ONLINE')));
    $('cellular').dataset.stale = String(stale);
    if (stale) {
      text('cellular-freshness', `${noCurrentSignal ? 'No current serving signal. Last known carriers' : 'Last known modem readings'} · received ${clock(noCurrentSignal ? lastServing.at : oldSignal.at)}. These are not live measurements.`);
      text('signal-quality', 'Last known');
    } else if (signal?.ok) {
      text('cellular-freshness', `Modem readings received ${clock(oldSignal.at)} · All reported carriers shown.`);
      text('signal-quality', signalQuality(OpenU60Cellular.parse(signal.data || {}).primary?.rsrp));
    }
  }
  function renderSpeed(data) {
    const rx = number(data?.rx_speed), tx = number(data?.tx_speed);
    text('download', rx === null ? null : (rx * 8 / 1e6).toFixed(2));
    text('upload', tx === null ? null : (tx * 8 / 1e6).toFixed(2));
    text('received', bytes(data?.rx_bytes)); text('sent', bytes(data?.tx_bytes));
    history.push(rx === null || tx === null ? null : [rx, tx]);
    history = history.slice(-30);
    const ceiling = Math.max(1, ...history.filter(Boolean).flat());
    for (const [index, id] of ['download-line', 'upload-line'].entries()) {
      let drawing = '', penDown = false;
      history.forEach((sample, i) => {
        if (!sample) { penDown = false; return; }
        drawing += `${penDown ? 'L' : 'M'}${(i / 29 * 320).toFixed(1)},${(36 - sample[index] / ceiling * 32).toFixed(1)} `;
        penDown = true;
      });
      $(id).setAttribute('d', drawing);
    }
  }
  function renderBattery(data) {
    const raw = number(data?.capacity), capacity = raw !== null && raw >= 0 && raw <= 100 ? raw : null;
    text('battery', capacity); text('battery-status', data?.status ?? 'Battery data unavailable');
    $('battery-bar').style.width = `${capacity ?? 0}%`;
    const temperature = number(data?.temperature);
    text('temperature', temperature === null ? null : `${(temperature / 10).toFixed(1)} °C`);
  }
  function renderWifi(data) {
    for (const band of ['2g', '5g']) {
      text(`ssid-${band}`, data?.[`ssid_${band}`] || `${band === '2g' ? '2.4' : '5'} GHz network`);
      text(`channel-${band}`, data?.[`channel_${band}`]);
      const disabled = data?.[`radio${band === '2g' ? '2' : '5'}_disabled`];
      text(`radio-${band}`, !data || disabled === undefined ? '—' : String(data.wifi_onoff) === '0' || String(disabled) === '1' ? 'Off' : 'Enabled');
    }
  }
  function renderClients(data) {
    if (data && !data.hosts && !Array.isArray(data.dhcp_leases)) data = null;
    clients = [];
    if (data) {
      const hosts = data.hosts;
      if (hosts && typeof hosts === 'object') {
        for (const [mac, host] of Object.entries(hosts)) {
          if (!host || typeof host !== 'object') continue;
          clients.push({ name: host.name || 'Unnamed device', ip: host.ipaddrs?.join(', ') || host.ipv4?.join(', ') || '—', mac, source: 'Host hint' });
        }
      }
      for (const lease of data.dhcp_leases ?? []) {
        if (!clients.some(client => client.mac.toLowerCase() === String(lease.macaddr).toLowerCase())) {
          clients.push({ name: lease.hostname || 'Unnamed device', ip: lease.ipaddr || '—', mac: lease.macaddr || '—', source: 'DHCP lease' });
        }
      }
    }
    if (OpenU60Navigation.current.key !== 'network/clients') text('device-count', data ? clients.length : '—');
    drawClients(data ? undefined : 'Device data unavailable.');
  }
  function drawClients(message) {
    const query = $('device-search').value.trim().toLowerCase();
    const filtered = clients.filter(client => Object.values(client).some(value => String(value).toLowerCase().includes(query)));
    const body = $('client-list'); body.replaceChildren();
    if (!filtered.length) {
      const row = body.insertRow(), cell = row.insertCell();
      row.setAttribute('role', 'row'); cell.setAttribute('role', 'cell');
      cell.colSpan = 4; cell.className = 'empty';
      cell.textContent = message || (query ? 'No devices match your search.' : 'No devices reported by the router.');
    }
    for (const client of filtered) {
      const row = body.insertRow();
      row.setAttribute('role', 'row');
      for (const [key, label] of [['name', 'Device'], ['ip', 'IP address'], ['mac', 'MAC address'], ['source', 'Source']]) {
        const cell = row.insertCell();
        cell.textContent = client[key];
        cell.dataset.label = label;
        cell.setAttribute('role', 'cell');
      }
    }
  }
  function decodeMessage(raw) {
    const value = String(raw ?? '');
    if (!value || value.length % 4 || !/^[0-9a-f]+$/i.test(value)) return value;
    let decoded = '';
    for (let i = 0; i < value.length; i += 4) decoded += String.fromCharCode(parseInt(value.slice(i,i+4),16));
    return decoded;
  }
  function messageDate(value) {
    const raw = String(value ?? '');
    const parts = raw.split(',').map(Number);
    if (parts.length < 6 || parts.slice(0,6).some(value => !Number.isFinite(value))) return raw || 'Date unreported';
    const [year,month,day,hour,minute,second] = parts;
    const quarters = Number.isFinite(parts[6]) ? parts[6] : 0;
    if (month < 1 || month > 12 || day < 1 || day > 31 || Math.abs(quarters) > 56) return raw;
    const date = new Date(Date.UTC(year < 100 ? year+2000 : year,month-1,day,hour,minute,second) - quarters*15*60*1000);
    return Number.isNaN(date.getTime()) ? raw : date.toLocaleString();
  }
  function renderSMS(data) {
    messages = Array.isArray(data?.messages) ? data.messages.map(message => ({...message,body:decodeMessage(message.content)})) : [];
    text('sms-count', data ? messages.length : '—');
    text('sms-status', data ? 'Live messages · Router and SIM storage' : 'Messages unavailable. Open SMS while connected to the router.');
    drawMessages();
  }
  function drawMessages() {
    const list = $('message-list'); list.replaceChildren();
    const query = $('sms-search').value.trim().toLowerCase();
    const found = messages.filter(message => `${message.number} ${message.body}`.toLowerCase().includes(query));
    if (!found.length) list.append(element('p','carrier-empty',query ? 'No messages match your search.' : 'No messages to display.'));
    const tags = {0:'Read',1:'Unread',2:'Sent',3:'Failed',4:'Draft'};
    for (const message of found) {
      const card = element('article','message-card');
      const heading = element('div','message-card-heading');
      heading.append(element('strong','',message.number || 'Unknown sender'),element('span','',messageDate(message.date)));
      card.append(heading,element('p','',message.body),element('small','',`${tags[message.tag] || 'Message'} · ${message.mem_store === 'sim' ? 'SIM' : 'Internal storage'}`));
      if (globalThis.OpenU60Controls) OpenU60Controls.decorateMessage(card,message);
      list.append(card);
    }
  }
  function renderSMSCapacity(data) {
    const capacity = (used,total) => data ? `${data[used] ?? '—'} / ${data[total] ?? '—'} messages` : '—';
    text('sms-internal',capacity('sms_nvused_total','sms_nv_total'));
    text('sms-sim',capacity('sms_simused_total','sms_sim_total'));
    text('sms-unread',data?.sms_dev_unread_num);
  }
  const renderers = {
    device: renderDevice, wan: renderWan, wan6: renderWan, signal: renderSignal,
    client_details: () => {}, battery_details: () => {}, https_status: () => {}, dns_cache: () => {},
    esim_status: () => {},
    lan_config: () => {}, firewall_config: () => {}, upnp_config: () => {}, port_rules: () => {}, qos_config: () => {},
    operators: () => {}, forwarding: () => {}, resources: () => {}, neighbors: () => {}, sweep: () => {}, speedtest: () => {}, at_port: () => {}, guest: () => {}, mobile: () => {}, charging: () => {}, speed: renderSpeed, battery: renderBattery, wifi: renderWifi, clients: renderClients, modem: renderModem, sms: renderSMS, sms_capacity: renderSMSCapacity
  };
  function setStreamState(state, label) {
    $('stream-state').dataset.state = state;
    text('stream-state', label);
    text('settings-stream', label);
    window.dispatchEvent(new CustomEvent('dashboard:connection',{detail:{state,label}}));
  }
  function stopStream() {
    epoch++;
    clearTimeout(reconnectTimer); clearTimeout(watchdog);
    controller?.abort(); controller = undefined;
  }
  function markOffline() {
    transportLive = false;
    // Current rates must never masquerade as live while the connection is down.
    renderSpeed(null);
    const speed = lastGood.get('speed')?.data;
    text('received', bytes(speed?.rx_bytes)); text('sent', bytes(speed?.tx_bytes));
    if (streamIncludesSms) text('sms-status','Connection interrupted · Showing the last received messages.');
    updateConnection();
  }
  function applyReadings(message, snapshot) {
    if (snapshot) readings.clear();
    transportLive = true;
    for (const [name, reading] of Object.entries(message.readings || {})) {
      if (!Object.hasOwn(renderers, name) || ((name === 'sms' || name === 'sms_capacity') && !streamIncludesSms)) continue;
      readings.set(name, reading);
      if (reading?.ok) {
        lastReceived = Date.now();
        lastGood.set(name, {data:reading.data, at:lastReceived});
        renderers[name](reading.data);
      } else if (name === 'speed') {
        renderSpeed(null);
        text('received', bytes(lastGood.get('speed')?.data?.rx_bytes)); text('sent', bytes(lastGood.get('speed')?.data?.tx_bytes));
      } else {
        renderers[name](lastGood.get(name)?.data ?? null);
      }
    }
    const currentSignal = readings.get('signal');
    const radioMode = String(readings.get('modem')?.data?.operate_mode ?? 'ONLINE').toUpperCase();
    if (currentSignal?.ok && radioMode === 'ONLINE' && OpenU60Cellular.parse(currentSignal.data || {}).hasSignal) {
      lastServing = lastGood.get('signal');
    }
    if (streamIncludesSms && readings.has('sms') && !readings.get('sms')?.ok) text('sms-status','Messages unavailable · Last received messages are retained where available.');
    const failures = [...readings.values()].filter(reading => !reading?.ok).length;
    setStreamState('live', 'Live');
    text('notice', failures ? `${failures} ${failures === 1 ? 'reading is' : 'readings are'} unavailable. Last known values are retained where available.` : 'Live from your router · Changes arrive automatically');
    $('notice').classList.toggle('error', failures > 0);
    text('updated', lastReceived ? `Last received ${clock(lastReceived)}` : 'Waiting for readings');
    updateConnection();
    window.dispatchEvent(new CustomEvent('dashboard:readings',{detail:{readings:message.readings || {},snapshot}}));
  }
  function handleFrame(frame) {
    let event = 'message'; const data = [];
    for (const line of frame.split('\n')) {
      if (line.startsWith('event:')) event = line.slice(6).trim();
      if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
    }
    if (event === 'auth') { lock(); return; }
    if ((event === 'snapshot' || event === 'patch') && data.length) {
      applyReadings(JSON.parse(data.join('\n')), event === 'snapshot');
      retryDelay = 1000;
    }
  }
  function streamOptions() {
    const {group,key} = OpenU60Navigation.current;
    const topic = {'settings/https':'https','network/esim':'esim','network/clients':'clients','network/dns-cache':'dns','network/lan':'routing','network/firewall':'routing','network/ports':'routing','network/qos':'routing','network/operators':'operators','sms/forwarding':'forwarding','tools/battery':'power','tools/diagnostics':'health','tools/radio':'radio','tools/speedtest':'speedtest','tools/terminal':'terminal'}[key];
    const query = [...(group === 'sms' ? ['sms=1'] : []),...(topic ? [`topics=${topic}`] : [])].join('&');
    return {includeSms:group === 'sms',key:query ? `?${query}` : '',topic};
  }
  async function connect() {
    if (locked || document.hidden) return;
    stopStream();
    transportLive = false;
    const session = epoch;
    const options = streamOptions(); streamIncludesSms = options.includeSms; streamKey = options.key;
    controller = new AbortController();
    const connection = controller;
    const resetWatchdog = () => {
      clearTimeout(watchdog);
      watchdog = setTimeout(() => connection.abort(), 30000);
    };
    setStreamState('connecting', 'Connecting');
    try {
      resetWatchdog();
      // Fetch keeps bearer tokens out of URLs while supporting an SSE response body.
      const response = await fetch(`/api/dashboard/events${streamKey}`, {
        cache: 'no-store', signal: connection.signal,
        headers: { Accept: 'text/event-stream', ...(token ? { Authorization: `Bearer ${token}` } : {}) }
      });
      if (session !== epoch) return;
      if (response.status === 401) { lock(); return; }
      if (!response.ok || !response.headers.get('content-type')?.includes('text/event-stream') || !response.body) {
        throw new Error('Live stream unavailable');
      }
      const reader = response.body.getReader();
      const decoder = new TextDecoder(); let buffer = '';
      try {
        while (session === epoch) {
          const { done, value } = await reader.read();
          if (done) break;
          if (session !== epoch) return;
          resetWatchdog();
          buffer += decoder.decode(value, { stream: true });
          // A CRLF pair may itself be split between network chunks.
          let boundary;
          while ((boundary = /\r?\n\r?\n/.exec(buffer))) {
            const frame = buffer.slice(0, boundary.index).replace(/\r\n/g, '\n');
            buffer = buffer.slice(boundary.index + boundary[0].length);
            handleFrame(frame);
            if (session !== epoch) return;
          }
          if (buffer.length > 1024 * 1024) throw new Error('Event frame too large');
        }
      } finally {
        await reader.cancel().catch(() => {});
        reader.releaseLock();
      }
      if (session === epoch) throw new Error('Stream disconnected');
    } catch (error) {
      if (session !== epoch || locked || document.hidden) return;
      markOffline();
      setStreamState('retrying', 'Reconnecting');
      text('notice', 'Live connection interrupted. Reconnecting automatically…');
      $('notice').classList.add('error'); text('updated', lastReceived ? `Last received ${clock(lastReceived)} · offline` : 'Waiting for live readings');
      reconnectTimer = setTimeout(connect, retryDelay);
      retryDelay = Math.min(retryDelay * 2, 15000);
    } finally {
      if (session === epoch) clearTimeout(watchdog);
    }
  }
  $('login-form').addEventListener('submit', async event => {
    event.preventDefault(); const button = event.submitter; button.disabled = true; text('login-error', '');
    try {
      const data = await api('/api/auth/login', { method: 'POST', body: JSON.stringify({ password: $('password').value }) });
      if (typeof data.token !== 'string' || !data.token || data.token.length > 2048 || /[\s\x00-\x1f]/.test(data.token)) throw new Error('Invalid session response');
      token = data.token; authEpoch++;
      try { sessionStorage.setItem(sessionKey,token); } catch { /* Keep the session in memory. */ }
      locked = false; $('password').value = ''; await OpenU60Motion.closeDialog(dialog); $('signout').hidden = false; connect();
    } catch (error) { text('login-error', error.auth ? 'Incorrect password. Please try again.' : 'Could not unlock the router. Check your connection.'); }
    finally { button.disabled = false; }
  });
  dialog.addEventListener('cancel', event => event.preventDefault());
  $('signout').addEventListener('click', lock);
  $('device-search').addEventListener('input', () => drawClients());
  $('sms-search').addEventListener('input', drawMessages);
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      stopStream();
      if (!locked) {
        setStreamState('paused', 'Paused');
        text('notice', 'Updates paused while this tab is in the background.');
      }
    } else connect();
  });
  window.addEventListener('dashboard:navigate',event => {
    if (event.detail.key !== 'network/clients') text('device-count', lastGood.get('clients') ? clients.length : '—');
    const options = streamOptions(), includeSms = options.includeSms;
    const topics = {https_status:'https',battery_details:'power',esim_status:'esim',client_details:'clients',dns_cache:'dns',operators:'operators',forwarding:'forwarding',resources:'health',neighbors:'radio',sweep:'radio',speedtest:'speedtest',at_port:'terminal'};
    for (const [name,topic] of Object.entries(topics)) if (topic !== options.topic) { readings.delete(name); lastGood.delete(name); }
    if (!includeSms) {
      readings.delete('sms'); readings.delete('sms_capacity');
      lastGood.delete('sms'); lastGood.delete('sms_capacity');
      renderSMS(null); renderSMSCapacity(null);
    }
    if (options.key !== streamKey) {
      if (includeSms) text('sms-status','Connecting to message storage…');
      connect();
    }
  });
  globalThis.OpenU60Console = {
    request: async (path,options = {}) => {
      try { return await api(path,options); }
      catch (error) { if (error.auth && error.authEpoch === authEpoch) lock(); throw error; }
    },
    data: name => lastGood.get(name)?.data,
    messages: () => messages,
    redrawMessages: drawMessages,
    updateMessages: data => { if (!locked && streamIncludesSms) { lastGood.set('sms',{data,at:Date.now()}); readings.set('sms',{ok:true,data}); renderSMS(data); } },
    refresh: connect,
    get locked() { return locked; },
    get connected() { return transportLive && !locked; }
  };
  // Local elapsed-time display only; this timer never requests router data.
  setInterval(() => { if (outageStarted !== null && !document.hidden) refreshOutageClock(); }, 1000);
  connect();
})();
