'use strict';
(() => {
  const UI = OpenU60UI, API = OpenU60Console, A = OpenU60Advanced;
  const {el,button,say,json,request,confirm,mutate} = UI;
  const section = UI.panel('network/operators','operator-scan','Available mobile networks','Scan from the router’s SIM, then choose a network. Scanning may interrupt mobile data for up to a few minutes.');
  const actions = el('div','form-actions'), scan = button('Scan networks',startScan,'primary'), automatic = button('Use automatic selection',useAutomatic);
  actions.append(scan,automatic); section.append(actions);
  const feedback = UI.status(section), state = el('p','field-help','Waiting for the modem’s scan status.'), list = el('div','operator-list');
  section.append(state,list);
  const scanJob = new A.Job('scan',120000), registerJob = new A.Job('registration',60000);
  let latest, selectedName = '', autoPending = false;
  function connected() { return API.connected && !API.locked; }
  function update(fresh = false) {
    const data = latest;
    const status = data?.status?.ok ? data.status.data?.m_netselect_status : undefined;
    const result = data?.registration?.ok ? data.registration.data?.m_netselect_result : undefined;
    const phase = fresh ? scanJob.observe(status) : scanJob.tick(), registration = registerJob.started !== null ? fresh ? registerJob.observe(result) : registerJob.tick() : 'idle';
    const scanning = ['running','waiting'].includes(phase) || scanJob.pending;
    scan.disabled = !connected() || scanning || registerJob.pending;
    automatic.disabled = !connected() || registerJob.pending;
    state.textContent = !data ? 'Waiting for scan readings…' : data.status?.ok ? `Modem scan status: ${status ?? 'not reported'} · ${data.sampled_at ? new Date(Number(data.sampled_at)).toLocaleTimeString() : 'live stream'}` : 'Scan status unavailable. Previously reported networks are retained.';
    if (registerJob.started !== null) {
      if (registration === 'complete') say(feedback,`The modem reports successful registration for ${selectedName}. Check Mobile Network for the resulting data connection.`);
      else if (registration === 'failed') say(feedback,`The modem reports registration failed for ${selectedName}. Try automatic selection or another allowed network.`,true);
      else if (registration === 'timeout') say(feedback,'Registration has not been confirmed within a minute. The modem may still be working; check its state before retrying.',true);
      else say(feedback,`Waiting for registration to ${selectedName}…`);
    } else if (scanJob.started !== null) {
      if (phase === 'complete') say(feedback,'The modem reports the scan is complete. Select an available network below. Results can include cached operator information.');
      else if (phase === 'failed') say(feedback,'The modem reports that the scan failed. Check the SIM and radio state before trying again.',true);
      else if (phase === 'timeout') say(feedback,'The scan has not been confirmed within two minutes. Scanning may still continue on the router.',true);
      else say(feedback,'Scanning mobile networks… Results arrive automatically.');
    }
    if (data?.results?.ok) draw(A.operators(data.results.data),scanning);
  }
  function draw(rows,scanning) {
    list.replaceChildren();
    if (!rows.length) { list.append(el('p','carrier-empty',scanning ? 'Waiting for scan results…' : 'No networks reported. Start a scan to look for operators.')); return; }
    for (const operator of rows) {
      const card = el('article','operator-card'), copy = el('div'), heading = el('div','title-count');
      heading.append(el('h3','',operator.name),el('span','tag',operator.status));
      copy.append(heading,el('p','mono',`${operator.plmn || 'PLMN unavailable'} · ${operator.rat || 'Access type unavailable'}`));
      const choose = button('Register',()=>register(operator));
      choose.disabled = !connected() || scanning || registerJob.pending || !operator.selectable;
      if (!operator.selectable) choose.title = 'The operator is forbidden or has no usable network identifier.';
      card.append(copy,choose); list.append(card);
    }
  }
  async function startScan() {
    if (!await confirm('Scan mobile networks?','The router will search available operators. Mobile data may disconnect during the scan. The scan continues if you leave this page.',{label:'Start scan'})) return;
    await mutate(feedback,async current=>{
      await request('/api/modem/scan',json('POST',{})); if (!current()) return;
      registerJob.clear(); scanJob.start(); autoPending = false; list.replaceChildren(el('p','carrier-empty','Waiting for new scan results…')); update(true);
    });
  }
  async function register(operator) {
    if (!operator.selectable || !await confirm('Register with this network?',`Select ${operator.name} (${operator.plmn}, ${operator.rat}). This can disconnect mobile data. Roaming charges may apply; your SIM must allow access.`,{label:'Register network'})) return;
    await mutate(feedback,async current=>{
      await request('/api/modem/register',json('POST',{m_mcc_mnc:operator.plmn,m_rat:operator.rat})); if (!current()) return;
      scanJob.clear(); registerJob.start(); selectedName = operator.name; autoPending = false; update(true);
    });
  }
  async function useAutomatic() {
    if (!await confirm('Use automatic network selection?','Let the SIM and modem choose the mobile operator again. Mobile data may reconnect.',{label:'Use automatic'})) return;
    await mutate(feedback,async current=>{
      await request('/api/modem/network-mode',json('PUT',{net_select_mode:'auto_select'})); if (!current()) return;
      registerJob.clear(); scanJob.clear(); autoPending = API.data('signal')?.net_select_mode !== 'auto_select'; say(feedback,autoPending ? 'Automatic selection requested. Waiting for the modem to confirm it…' : 'Automatic network selection confirmed by the modem.');
    });
  }
  window.addEventListener('dashboard:readings',event=>{
    if (event.detail.readings.operators?.ok) { latest = event.detail.readings.operators.data; update(true); }
    if (autoPending && event.detail.readings.signal?.ok && event.detail.readings.signal.data?.net_select_mode === 'auto_select') { autoPending = false; say(feedback,'Automatic network selection confirmed by the modem.'); }
  });
  const timer = setInterval(()=>{ if (!document.hidden && OpenU60Navigation.current.key === 'network/operators' && (scanJob.pending || registerJob.pending)) update(); },1000);
  window.addEventListener('dashboard:navigate',()=>{ if (OpenU60Navigation.current.key === 'network/operators') update(); });
  UI.track({route:'network/operators',loaded:true,load:()=>{},get dirty(){return false;},clear:()=>{ latest = null; selectedName = ''; autoPending = false; scanJob.clear(); registerJob.clear(); state.textContent = 'Unlock the console to read the modem status.'; list.replaceChildren(); feedback.textContent = ''; scan.disabled = automatic.disabled = true; }});
  window.addEventListener('pagehide',event=>{ if (!event.persisted) clearInterval(timer); });
  latest = API.data('operators'); update(true);
})();
