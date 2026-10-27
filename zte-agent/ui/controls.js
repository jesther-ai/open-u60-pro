'use strict';
(() => {
  const consoleAPI = globalThis.OpenU60Console, A = globalThis.OpenU60Actions;
  const views = [], selected = new Set();
  let session = 0, mutationBusy = false;
  const el = (tag,className,text) => { const node = document.createElement(tag); if (className) node.className = className; if (text !== undefined) node.textContent = text; return node; };
  const button = (text,run,className = '') => { const node = el('button',`button ${className}`,text); node.type = 'button'; if (run) node.addEventListener('click',run); return node; };
  const say = (node,text,error = false) => { node.textContent = text; node.dataset.error = String(error); };
  const json = (method,body) => ({method,body:JSON.stringify(body)});
  const request = (...args) => consoleAPI.request(...args);
  const route = () => OpenU60Navigation.current.key;
  const delay = ms => new Promise(resolve => setTimeout(resolve,ms));
  function panel(key,id,title,description) {
    const section = el('section','panel control-panel'); section.id = id; section.dataset.views = key;
    section.dataset.viewHidden = String(route() !== key);
    const heading = el('div','section-heading'), copy = el('div');
    copy.append(el('h2','',title),el('p','section-subtitle',description)); heading.append(copy); section.append(heading);
    document.querySelector('#cellular').before(section); return section;
  }
  const status = section => { const node = el('p','action-status'); node.setAttribute('role','status'); node.setAttribute('aria-live','polite'); section.append(node); return node; };
  const confirmation = el('dialog','confirm-dialog');
  confirmation.setAttribute('aria-labelledby','confirm-title');
  const confirmForm = el('form'), confirmTitle = el('h2','','Review change'); confirmTitle.id = 'confirm-title';
  const confirmCopy = el('p','confirm-copy'), confirmLabel = el('label','','Type RESET to continue'), confirmInput = el('input');
  confirmInput.id = 'confirm-reset'; confirmInput.autocomplete = 'off'; confirmLabel.htmlFor = confirmInput.id;
  const confirmActions = el('div','form-actions'), cancel = button('Cancel'), proceed = button('Confirm',null,'primary');
  confirmActions.append(cancel,proceed); confirmForm.append(confirmTitle,confirmCopy,confirmLabel,confirmInput,confirmActions); confirmation.append(confirmForm); document.body.append(confirmation);
  let pendingConfirmation, confirmationPhrase='RESET', closingConfirmation=false, confirmationResult=false;
  function finishConfirmation(value) {
    if (!pendingConfirmation) return;
    if (closingConfirmation) { if (!value) confirmationResult=false; return; }
    closingConfirmation=true;confirmationResult=value;cancel.disabled=proceed.disabled=true;
    OpenU60Motion.closeDialog(confirmation).then(() => {
      const resolve=pendingConfirmation;pendingConfirmation=null;closingConfirmation=false;
      confirmInput.value='';confirmCopy.textContent='';resolve(confirmationResult);
    });
  }
  cancel.addEventListener('click',()=>finishConfirmation(false));
  proceed.addEventListener('click',()=>finishConfirmation(true));
  confirmation.addEventListener('cancel',event=>{ event.preventDefault(); finishConfirmation(false); });
  confirmForm.addEventListener('submit',event=>event.preventDefault());
  confirmInput.addEventListener('input',()=>{ proceed.disabled = confirmInput.value !== confirmationPhrase; });
  function confirm(title,text,{destructive = false,typed = false,label = 'Apply change',phrase='RESET'} = {}) {
    if (pendingConfirmation || mutationBusy) return Promise.resolve(false);
    confirmationPhrase=phrase;confirmLabel.textContent=`Type ${phrase} to continue`;
    confirmTitle.textContent = title; confirmCopy.textContent = text; confirmLabel.hidden = confirmInput.hidden = !typed;
    confirmInput.value = ''; proceed.textContent = label; proceed.disabled = typed; proceed.classList.toggle('danger',destructive);
    cancel.disabled=false;OpenU60Motion.showDialog(confirmation);cancel.focus();return new Promise(resolve=>{ pendingConfirmation = resolve; });
  }
  async function mutate(node,operation) {
    if (mutationBusy) { say(node,'Another change is in progress. Wait for it to finish.',true); return; }
    if (consoleAPI.locked || !consoleAPI.connected) { say(node,'Connect and unlock the console before making changes.',true); return; }
    mutationBusy = true; const epoch = session;
    document.documentElement.dataset.saving = 'true';
    const surfaces = [...document.querySelectorAll('.control-panel form,#apn-profile,.message-toolbar,.message-actions,.rule-toolbar,.rule-actions,.operator-list,.diagnostic-actions')];
    for (const surface of surfaces) surface.inert = true;
    try { await operation(()=>epoch === session && !consoleAPI.locked); }
    catch (error) { if (epoch === session && !consoleAPI.locked) say(node,`The change could not be confirmed. ${error.message}. Check the router state before retrying; the request may already have taken effect.`,true); }
    finally { mutationBusy = false; delete document.documentElement.dataset.saving; for (const surface of surfaces) surface.inert = false; window.dispatchEvent(new Event('dashboard:mutation-complete')); }
  }
  function field(key,label,options = {}) { return {key,label,...options}; }
  const choices = (key,label,values,options) => field(key,label,{values,...options});
  const onoff = (key,label,options) => choices(key,label,[['1','On'],['0','Off']],options);
  const disabled = (key,label,options) => choices(key,label,[['0','On'],['1','Off']],options);
  const security = [['psk2+ccmp','WPA2 · AES'],['sae-mixed','WPA2 / WPA3'],['sae','WPA3'],['none','Open · no password']];
  function makeForm(config) {
    const section = panel(config.route,config.id,config.title,config.description);
    const specs = config.fields.filter(spec=>!spec.heading);
    const form = el('form','settings-form'); form.autocomplete = 'off';
    const fields = el('fieldset','form-grid'); fields.disabled = true;
    const legend = el('legend','sr-only',config.title); fields.append(legend);
    const inputs = new Map(); let baseline = null, loading = false, dirty = false, attempted = false;
    for (const spec of config.fields) {
      if (spec.heading) { fields.append(el('h3','form-group-heading',spec.heading)); continue; }
      const wrapper = el('div','form-field'), label = el('label','',spec.label);
      const input = el(spec.values ? 'select' : spec.type === 'textarea' ? 'textarea' : 'input');
      input.id = `${config.id}-${spec.key}`; input.name = spec.key; label.htmlFor = input.id;
      if (spec.values) for (const [value,text] of spec.values) input.add(new Option(text,String(value)));
      else { input.type = spec.type || 'text'; if (spec.min !== undefined) input.min = spec.min; if (spec.max !== undefined) input.max = spec.max; if (spec.type === 'number') input.step = '1'; }
      input.autocomplete = spec.secret ? 'new-password' : 'off';
      if (spec.placeholder) input.placeholder = spec.placeholder;
      if (spec.required) input.required = true;
      if (spec.maxLength) input.maxLength = spec.maxLength;
      wrapper.append(label,input);
      if (spec.help || spec.secret) { const help = el('small','field-help',spec.help || 'Leave blank to keep the current password.'); help.id = `${input.id}-help`; input.setAttribute('aria-describedby',help.id); wrapper.append(help); }
      fields.append(wrapper); inputs.set(spec.key,input);
    }
    const actions = el('div','form-actions'), save = button(config.saveLabel || 'Save changes',null,'primary'); save.type = 'submit';
    const discard = button('Discard edits',()=>{ if (baseline) populate(baseline); });
    const reload = button('Read current settings',async()=>{ if (!dirty || await confirm('Discard unsaved edits?','Read the current router settings and replace this draft.',{label:'Discard and read'})) await load(); });
    actions.append(save,discard,reload); form.append(fields,actions); section.append(form); const feedback = status(section); save.disabled = discard.disabled = true;
    const readKey = spec => spec.readKey || spec.key;
    function value(spec) { const raw = baseline?.[readKey(spec)]; return raw === undefined || raw === null ? '' : String(raw); }
    function populate(data) {
      baseline = data; dirty = false;
      for (const spec of specs) {
        const input = inputs.get(spec.key), raw = spec.secret ? '' : value(spec);
        if (spec.values) { for (const option of [...input.options]) if (option.dataset.current) option.remove(); if (![...input.options].some(option=>option.value === raw)) { const option = new Option(raw ? `Current: ${raw}` : 'Not reported',raw); option.dataset.current = 'true'; input.add(option); } }
        input.value = raw;
      }
      fields.disabled = false; save.disabled = true; discard.disabled = true; section.dataset.dirty = 'false';
    }
    function changes() {
      const changed = {};
      for (const spec of specs) {
        const raw = inputs.get(spec.key).value;
        if (spec.secret && !raw) continue;
        if (raw !== value(spec)) changed[spec.key] = spec.type === 'number' ? Number(raw) : spec.boolean ? raw === 'true' : raw;
      }
      return changed;
    }
    form.addEventListener('input',()=>{ dirty = baseline && Object.keys(changes()).length > 0; save.disabled = !dirty; discard.disabled = !dirty; section.dataset.dirty = String(!!dirty); if (dirty) say(feedback,'Unsaved changes · live updates keep your draft.'); });
    form.addEventListener('change',()=>form.dispatchEvent(new Event('input')));
    async function read() {
      const data = await (config.read ? config.read() : request(config.endpoint));
      if (!data || typeof data !== 'object' || Array.isArray(data) || !specs.some(spec=>Object.hasOwn(data,readKey(spec)))) throw new Error('The router returned no usable settings');
      return data;
    }
    async function load() {
      if (loading || consoleAPI.locked) return; loading = true; attempted = true; const epoch = session; fields.disabled = true; save.disabled = discard.disabled = true; say(feedback,'Reading current settings…');
      try { const data = await read(); if (epoch !== session || consoleAPI.locked) return; populate(data); say(feedback,'Current settings loaded.'); }
      catch (error) { if (epoch === session && !consoleAPI.locked) { say(feedback,`Settings unavailable. ${error.message}. Use “Read current settings” to try again.`,true); fields.disabled = !baseline; save.disabled = !dirty; discard.disabled = !dirty; } }
      finally { loading = false; }
    }
    form.addEventListener('submit',async event=>{
      event.preventDefault(); if (!baseline || !form.reportValidity() || !dirty) return;
      try {
        const changed = changes(), all = Object.fromEntries(specs.map(spec=>[spec.key,inputs.get(spec.key).value]));
        for (const spec of specs) {
          if (!(spec.key in changed)) continue;
          const raw = all[spec.key];
          if (spec.type === 'number' && (raw === '' || !Number.isInteger(Number(raw)))) throw new Error(`Enter a whole number for ${spec.label}.`);
          if (spec.values && !spec.values.some(([value])=>String(value) === raw)) throw new Error(`Choose a supported value for ${spec.label}.`);
        }
        await config.validate?.(changed,all,baseline);
        const warning = typeof config.warning === 'function' ? config.warning(changed,all,baseline) : config.warning;
        if (warning && !await confirm(config.title,warning,{label:'Save changes'})) return;
        await mutate(feedback,async current=>{
          fields.disabled = true; save.disabled = discard.disabled = reload.disabled = true; say(feedback,'Applying changes…');
          try {
            const body = config.body ? config.body(changed,all,baseline) : changed;
            if (config.guardKeys) {
              const fresh = await read(); if (!current()) return;
              if (config.guardKeys.some(key=>String(fresh[key] ?? '') !== String(baseline[key] ?? ''))) throw new Error('These settings changed elsewhere. Read current settings and review your changes');
            }
            if (config.preflight) { await config.preflight(body,all,baseline); if (!current()) return; }
            config.onWriting?.(body);
            await request(config.writeEndpoint || config.endpoint,json(config.method || 'PUT',body));
            if (!current()) return;
            let latest, matched = false;
            for (const wait of [0,1000,2000]) {
              if (wait) await delay(wait); if (!current()) return;
              latest = await read(); if (!current()) return;
              matched = config.verify ? config.verify(latest,body) : Object.entries(changed).every(([key,val])=>String(latest[readKey(specs.find(spec=>spec.key === key))]) === String(val));
              if (matched) break;
            }
            if (matched) { populate(latest); say(feedback,'Saved and confirmed by the router.'); }
            else say(feedback,'Request accepted, but the router has not confirmed these values. Your draft is kept. Read current settings before retrying.',true);
          } finally { if (current()) { fields.disabled = false; save.disabled = !dirty; discard.disabled = !dirty; reload.disabled = false; } }
        });
      } catch (error) { say(feedback,error.message,true); }
    });
    const view = {route:config.route,section,load,clear:()=>{ baseline = null; dirty = false; attempted = false; fields.disabled = true; for (const input of inputs.values()) input.value = ''; save.disabled = discard.disabled = true; reload.disabled = false; section.dataset.dirty = 'false'; say(feedback,'Unlock the console to load settings.'); },get loaded(){return attempted;},get dirty(){return dirty;},sync(readings) {
      const source = config.source || {'/api/wifi/status':'wifi','/api/wifi/guest':'guest','/api/modem/data':'mobile','/api/modem/status':'modem','/api/network/signal':'signal','/api/device/charge-control':'charging'}[config.endpoint];
      const raw = readings[source]?.ok ? readings[source].data : null;
      const data = config.liveData && raw ? config.liveData(raw) : raw;
      if (!baseline || !data || mutationBusy || loading) return;
      const safe = Object.fromEntries(specs.filter(spec=>!spec.secret && Object.hasOwn(data,readKey(spec))).map(spec=>[readKey(spec),data[readKey(spec)]]));
      if (!Object.entries(safe).some(([key,value])=>String(value) !== String(baseline[key]))) return;
      if (dirty) say(feedback,'The router settings changed. Your unsaved draft is kept. Read current settings before applying it.',true);
      else { populate({...baseline,...safe}); say(feedback,'Updated from the router’s live stream.'); }
    }};
    views.push(view); return view;
  }
  const wifiWarning = 'Applying wireless settings may disconnect this device. Reconnect using the new network name or password, then reopen the console.';
  const wifiFields = [onoff('wifi_onoff','Wi-Fi'),onoff('wifi6_switch','Wi-Fi 6')];
  for (const [suffix,label,channels,widths] of [
    ['2g','2.4 GHz',['auto','1','2','3','4','5','6','7','8','9','10','11','12','13'],['auto','EHT20','EHT40']],
    ['5g','5 GHz',['auto','36','40','44','48','52','56','60','64','100','104','108','112','116','120','124','128','132','136','140','144','149','153','157','161','165'],['auto','EHT20','EHT40','EHT80','EHT160']]
  ]) wifiFields.push({heading:label},disabled(suffix === '2g' ? 'radio2_disabled' : 'radio5_disabled',`${label} radio`),field(`ssid_${suffix}`,`${label} name`,{required:true}),choices(`encryption_${suffix}`,`${label} security`,security),field(`key_${suffix}`,`${label} new password`,{type:'password',secret:true}),onoff(`hidden_${suffix}`,`${label} hidden network`),choices(`channel_${suffix}`,`${label} channel`,channels.map(value=>[value,value === 'auto' ? 'Automatic' : value]),{help:'Available channels depend on the router’s country setting.'}),choices(`htmode_${suffix}`,`${label} channel width`,widths.map(value=>[value,value === 'auto' ? 'Automatic' : `${value.slice(3)} MHz`])),field(`txpower_${suffix}`,`${label} transmit power (%)`,{type:'number',min:1,max:100}));
  function validateWiFi(changed,all,baseline,guest = false) {
    const prefixes = guest ? ['guest'] : ['2g','5g'];
    for (const suffix of prefixes) {
      const name = guest ? 'guest_ssid' : `ssid_${suffix}`, key = guest ? 'guest_key' : `key_${suffix}`, securityKey = guest ? 'guest_encryption' : `encryption_${suffix}`;
      if (name in changed) A.wifiText(all[name],'Network name');
      if (all[key]) { A.wifiPassword(all[key]); if (all[securityKey].startsWith('sae') && all[key].length > 63) throw new Error('WPA3 passwords must contain 8–63 bytes.'); }
      if (all[securityKey] !== 'none' && securityKey in changed && !all[key] && !baseline[guest ? 'key' : key]) throw new Error('Enter a password when enabling network security.');
    }
  }
  makeForm({route:'network/wifi',id:'wifi-config',title:'Your wireless networks',description:'Edit each radio separately. Passwords stay private until you replace them.',endpoint:'/api/wifi/status',writeEndpoint:'/api/wifi/settings',fields:wifiFields,warning:wifiWarning,validate:validateWiFi});
  makeForm({route:'network/wifi',id:'guest-config',title:'Guest network',description:'A shared name and password for visitors, with separate radio controls.',endpoint:'/api/wifi/guest',fields:[disabled('guest_disabled_2g','2.4 GHz guest radio',{readKey:'disabled_2g'}),disabled('guest_disabled_5g','5 GHz guest radio',{readKey:'disabled_5g'}),field('guest_ssid','Guest network name',{readKey:'ssid',required:true}),choices('guest_encryption','Security',security,{readKey:'encryption'}),field('guest_key','New password',{type:'password',secret:true,readKey:'key'}),onoff('guest_hidden','Hidden network',{readKey:'hidden'}),onoff('guest_isolate','Isolate guest devices',{readKey:'isolate'}),choices('guest_active_time','Automatically turn off',[[0,'No time limit'],[30,'After 30 minutes'],[60,'After 1 hour'],[120,'After 2 hours'],[240,'After 4 hours'],[480,'After 8 hours'],[720,'After 12 hours'],[1440,'After 24 hours']])],warning:wifiWarning,validate:(...args)=>validateWiFi(...args,true)});
  const mobileWarning = 'Changing mobile settings can interrupt internet access. Roaming may incur carrier charges. Keep a local connection to the router.';
  makeForm({route:'network/mobile',id:'mobile-config',title:'Mobile data',description:'Control the SIM data connection and roaming.',endpoint:'/api/modem/data',validate:(_,all,baseline)=>{ if (!['enable','roam_enable','connect_mode'].every(key=>['0','1'].includes(all[key]))) throw new Error('Choose a value for every mobile data setting before saving.'); if (all.roam_enable === '1' && String(baseline.roam_enable) !== '1' && all.enable !== '1') throw new Error('Enable mobile data when turning roaming on. The router requires it to establish a roaming connection.'); },fields:[choices('enable','Mobile data',[['1','Enabled'],['0','Disabled']]),choices('roam_enable','Data roaming',[['0','Disabled'],['1','Allowed · carrier charges may apply']]),choices('connect_mode','Connection mode',[['1','Automatic'],['0','Manual']])],warning:mobileWarning,body:(_,all)=>({cid:1,enable:Number(all.enable),roam_enable:Number(all.roam_enable),connect_mode:Number(all.connect_mode),...(all.enable === '0' ? {connect_status:'disconnected'} : {})}),verify:(data,body)=>['enable','roam_enable','connect_mode'].every(key=>String(data[key]) === String(body[key]))});
  makeForm({route:'network/mobile',id:'radio-config',title:'Modem radio',description:'Airplane mode disables cellular service. Wi-Fi can remain available.',endpoint:'/api/modem/status',writeEndpoint:'/api/modem/airplane',method:'POST',fields:[choices('operate_mode','Radio state',[['ONLINE','Online'],['LPM','Airplane mode']])],warning:'Airplane mode disconnects cellular data, calls and messages. Keep a local connection so you can turn the radio back on.'});
  makeForm({route:'network/mobile',id:'network-mode-config',title:'Preferred mobile network',description:'Automatic mode offers the widest fallback. SA requires carrier and SIM support.',endpoint:'/api/network/signal',writeEndpoint:'/api/modem/network-mode',fields:[choices('net_select','Network preference',[['WL_AND_5G','Automatic · 5G / 4G / 3G'],['LTE_AND_5G','5G NSA · LTE + NR'],['Only_5G','5G SA only'],['Only_LTE','4G LTE only'],['WCDMA_AND_LTE','4G / 3G'],['Only_WCDMA','3G only']])],warning:mobileWarning});
  makeForm({route:'network/apn',id:'apn-mode',title:'APN selection',description:'Use the carrier’s automatic profile, or select a manual profile below.',endpoint:'/api/router/apn/mode',fields:[choices('apn_mode','Profile mode',[['0','Automatic'],['1','Manual']])],body:changed=>({apn_mode:Number(changed.apn_mode)}),warning:mobileWarning});

  // Manual APN profiles are kept in memory only; passwords never populate visible fields.
  const apnPanel = panel('network/apn','apn-profiles','Manual APN profiles','Add, edit and activate a carrier access point. Changes can interrupt mobile data.');
  const profileList = el('div','profile-list'), apnActions = el('div','form-actions');
  const profileSelect = el('select'); profileSelect.id = 'apn-profile'; const profileLabel = el('label','','Profile'); profileLabel.htmlFor = profileSelect.id;
  apnPanel.append(profileLabel,profileSelect,profileList);
  const apnForm = el('form','settings-form'), apnFields = el('fieldset','form-grid'); apnFields.disabled = true;
  const apnInputs = {};
  for (const [key,label,values] of [['profilename','Profile name'],['wanapn','APN'],['pdpType','IP version',[['1','IPv4'],['2','IPv6'],['3','IPv4 + IPv6']]],['pppAuthMode','Authentication',[['0','None'],['1','PAP'],['2','CHAP']]],['username','Username'],['password','New password']]) {
    const wrapper = el('div','form-field'), input = el(values ? 'select' : 'input'), labelNode = el('label','',label); input.id = `apn-${key}`; labelNode.htmlFor = input.id;
    if (values) for (const [value,text] of values) input.add(new Option(text,value));
    else { input.type = key === 'password' ? 'password' : 'text'; input.autocomplete = key === 'password' ? 'new-password' : 'off'; input.maxLength = 128; }
    input.required = ['profilename','wanapn'].includes(key); wrapper.append(labelNode,input);
    if (key === 'password') wrapper.append(el('small','field-help','Leave blank to keep the selected profile’s password.'));
    apnInputs[key] = input; apnFields.append(wrapper);
  }
  let profiles = [], apnLoaded = false, apnDirty = false, apnAttempted = false, apnLoading = false;
  const apnFeedback = el('p','action-status'); apnFeedback.setAttribute('role','status');
  function editProfile() {
    const profile = profiles.find(item=>String(item.profileId) === profileSelect.value);
    for (const [key,input] of Object.entries(apnInputs)) input.value = key === 'password' ? '' : String(profile?.[key] ?? (key === 'pdpType' ? 3 : key === 'pppAuthMode' ? 0 : ''));
    profileList.textContent = profile ? `Profile ${profile.profileId} · ${String(profile.isEnable) === '1' ? 'Active' : 'Inactive'}` : 'New manual profile';
    apnDirty = false; apnPanel.dataset.dirty = 'false'; profileSelect.dataset.previous = profileSelect.value;
    activateAPN.disabled = deleteAPN.disabled = !profile; if (String(profile?.isEnable) === '1') deleteAPN.disabled = true;
  }
  async function loadProfiles(keep = false) {
    if (apnLoading) return; apnLoading = true; apnAttempted = true; apnFields.disabled = true; profileSelect.disabled = true; apnForm.inert = true; saveAPN.disabled = activateAPN.disabled = deleteAPN.disabled = readAPN.disabled = true;
    const epoch = session;
    try {
      const data = await request('/api/router/apn/profiles'); if (epoch !== session || consoleAPI.locked) return;
      if (!Array.isArray(data.apnListArray)) throw new Error('The router returned no profile list');
      profiles = data.apnListArray.filter(item=>item.profilename || item.wanapn); apnLoaded = true; apnFields.disabled = false;
      if (keep && apnDirty) return;
      const id = profileSelect.value; profileSelect.replaceChildren(new Option('Create a profile','new'));
      for (const item of profiles) profileSelect.add(new Option(`${item.profilename || item.wanapn}${String(item.isEnable) === '1' ? ' · Active' : ''}`,String(item.profileId)));
      profileSelect.value = profiles.some(item=>String(item.profileId) === id) ? id : 'new'; editProfile(); say(apnFeedback,'Profiles loaded.');
    } catch (error) { if (epoch === session && !consoleAPI.locked) say(apnFeedback,`Could not load APN profiles. ${error.message}`,true); }
    finally { apnLoading = false; if (epoch === session && !consoleAPI.locked) { apnFields.disabled = !apnLoaded; profileSelect.disabled = false; apnForm.inert = mutationBusy; saveAPN.disabled = !apnLoaded; readAPN.disabled = false; const profile = profiles.find(item=>String(item.profileId) === profileSelect.value); activateAPN.disabled = !profile; deleteAPN.disabled = !profile || String(profile.isEnable) === '1'; } }
  }
  const saveAPN = button('Save profile',null,'primary'); saveAPN.type = 'submit';
  const activateAPN = button('Activate profile',async()=>{
    const profile = profiles.find(item=>String(item.profileId) === profileSelect.value); if (!profile) return;
    if (!await confirm('Activate APN profile?',`Use “${profile.profilename}” (${profile.wanapn}) for mobile data. Unsaved profile edits are not included. Switch APN selection to Manual to use manual profiles.`,{label:'Activate profile'})) return;
    await mutate(apnFeedback,async current=>{ await request('/api/router/apn/profiles/activate',json('POST',{profileId:profile.profileId})); if (!current()) return; await loadProfiles(true); if (current()) say(apnFeedback,profiles.some(item=>String(item.profileId) === String(profile.profileId) && String(item.isEnable) === '1') ? 'Active profile confirmed by the router.' : 'Activation requested. Read profiles again to confirm.'); });
  });
  const deleteAPN = button('Delete profile',async()=>{
    const profile = profiles.find(item=>String(item.profileId) === profileSelect.value); if (!profile) return;
    if (!await confirm('Delete APN profile?',`Permanently delete “${profile.profilename}”.`,{destructive:true,label:'Delete profile'})) return;
    await mutate(apnFeedback,async current=>{
      const fresh = await request('/api/router/apn/profiles'); if (!current()) return;
      if ((fresh.apnListArray || []).some(item=>String(item.profileId) === String(profile.profileId) && String(item.isEnable) === '1')) throw new Error('The active profile cannot be deleted');
      await request('/api/router/apn/profiles/delete',json('POST',{profileId:profile.profileId})); if (!current()) return; apnDirty = false; await loadProfiles();
      if (current()) say(apnFeedback,profiles.some(item=>String(item.profileId) === String(profile.profileId)) ? 'Delete requested, but the profile is still present. Read profiles again before retrying.' : 'Profile deletion confirmed.');
    });
  },'danger');
  const readAPN = button('Read profiles',async()=>{ if (!apnDirty || await confirm('Discard profile edits?','Replace this draft with the current APN profile.',{label:'Discard and read'})) { apnDirty = false; await loadProfiles(); } });
  activateAPN.disabled = deleteAPN.disabled = true;
  apnActions.append(saveAPN,activateAPN,deleteAPN,readAPN); apnForm.append(apnFields,apnActions); apnPanel.append(apnForm,apnFeedback);
  profileSelect.addEventListener('change',async()=>{ if (apnDirty && !await confirm('Discard profile edits?','Selecting a profile replaces the current draft.',{label:'Discard edits'})) { profileSelect.value = profileSelect.dataset.previous || 'new'; return; } editProfile(); profileSelect.dataset.previous = profileSelect.value; });
  apnForm.addEventListener('input',()=>{ apnDirty = true; apnPanel.dataset.dirty = 'true'; say(apnFeedback,'Unsaved profile edits.'); });
  apnForm.addEventListener('submit',async event=>{
    event.preventDefault(); if (!apnLoaded || !apnForm.reportValidity()) return;
    const profile = profiles.find(item=>String(item.profileId) === profileSelect.value);
    const body = Object.fromEntries(Object.entries(apnInputs).map(([key,input])=>[key,['pdpType','pppAuthMode'].includes(key) ? Number(input.value) : key === 'password' ? input.value : input.value.trim()]));
    if (profiles.some(item=>String(item.profileId) !== String(profile?.profileId) && item.profilename === body.profilename)) { say(apnFeedback,'Use a unique profile name.',true); return; }
    if (profile) { body.profileId = profile.profileId; if (!apnInputs.password.value) body.password = profile.password || ''; }
    else body.password = apnInputs.password.value;
    if (!await confirm('Save APN profile?',`Save “${body.profilename}” with APN “${body.wanapn}”. Saving an active profile can interrupt mobile data.`,{label:'Save profile'})) return;
    await mutate(apnFeedback,async current=>{ await request('/api/router/apn/profiles',json(profile ? 'PUT' : 'POST',body)); if (!current()) return; await loadProfiles(true); if (!current()) return;
      const saved = profiles.find(item=>item.profilename === body.profilename && item.wanapn === body.wanapn && String(item.pdpType) === String(body.pdpType) && String(item.pppAuthMode) === String(body.pppAuthMode) && String(item.username || '') === body.username && String(item.password || '') === body.password);
      if (saved) { apnDirty = false; await loadProfiles(); profileSelect.value = String(saved.profileId); editProfile(); say(apnFeedback,'Profile saved and confirmed.'); } else say(apnFeedback,'Save requested, but profile values are not confirmed. Your draft is kept.',true);
    });
  });
  views.push({route:'network/apn',section:apnPanel,load:loadProfiles,get loaded(){return apnAttempted;},get dirty(){return apnDirty;},clear:()=>{ profiles = []; apnLoaded = apnDirty = apnAttempted = false; profileSelect.replaceChildren(); profileSelect.disabled = true; apnForm.inert = false; for (const input of Object.values(apnInputs)) input.value = ''; apnFields.disabled = true; activateAPN.disabled = deleteAPN.disabled = true; apnPanel.dataset.dirty = 'false'; profileList.textContent = ''; apnFeedback.textContent = ''; }});

  function commandForm(config) {
    const section = panel(config.route,config.id,config.title,config.description), form = el('form','settings-form'), fields = el('div','form-grid'), inputs = {};
    for (const spec of config.fields || []) {
      const wrapper = el('div','form-field'), label = el('label','',spec.label), input = el(spec.values ? 'select' : spec.type === 'textarea' ? 'textarea' : 'input'); input.id = `${config.id}-${spec.key}`; input.name = spec.key; label.htmlFor = input.id;
      if (spec.values) for (const [value,text] of spec.values) input.add(new Option(text,value)); else { if (spec.type !== 'textarea') input.type = spec.type || 'text'; input.required = spec.required !== false; if (spec.min !== undefined) input.min = spec.min; if (spec.max !== undefined) input.max = spec.max; if (spec.maxLength) input.maxLength = spec.maxLength; }
      if (spec.placeholder) input.placeholder = spec.placeholder; wrapper.append(label,input); if (spec.help) wrapper.append(el('small','field-help',spec.help)); fields.append(wrapper); inputs[spec.key] = input;
    }
    const actions = el('div','form-actions'), run = button(config.label,null,config.danger ? 'danger' : 'primary'); run.type = 'submit'; actions.append(run); form.append(fields,actions); section.append(form); const feedback = status(section);
    form.addEventListener('input',()=>{ section.dataset.dirty = String(Object.values(inputs).some(input=>input.value)); });
    form.addEventListener('submit',async event=>{
      event.preventDefault(); if (!form.reportValidity()) return;
      const values = Object.fromEntries(Object.entries(inputs).map(([key,input])=>[key,input.value]));
      try {
        const body = config.body ? config.body(values) : {};
        if (!await confirm(config.confirmTitle || config.title,typeof config.warning === 'function' ? config.warning(values) : config.warning,{label:config.label,destructive:config.danger,typed:config.typed})) return;
        await mutate(feedback,async current=>{ run.disabled = true; say(feedback,'Sending request…'); try { const result = await request(config.endpoint,json('POST',body)); if (!current()) return; if (result && typeof result === 'object' && 'result' in result && !(config.resultCodes || [0,'0',true,'success','SUCCESS']).includes(result.result)) throw new Error(`The modem returned result ${result.result}; the action is not confirmed`); if (config.success) await config.success(result,values); if (current()) { say(feedback,config.accepted || 'Request accepted. Check the live connection for the resulting state.'); if (config.clearOnSuccess) { form.reset(); form.dispatchEvent(new Event('reset-complete')); section.dataset.dirty = 'false'; } } } finally { run.disabled = false; } });
      } catch (error) { say(feedback,error.message,true); }
    });
    views.push({route:config.route,section,loaded:true,load:()=>{},get dirty(){return section.dataset.dirty === 'true';},clear:()=>{ form.reset(); for (const input of Object.values(inputs)) if (input.tagName !== 'SELECT') input.value = ''; section.dataset.dirty = 'false'; feedback.textContent = ''; }});
    return {section,form,inputs,feedback};
  }
  const lockWarning = 'Band and cell restrictions can leave the router without service. Keep a local connection and use Reset restrictions if service is lost.';
  commandForm({route:'network/bands',id:'lte-band',title:'LTE bands',description:'Restrict 4G to selected bands. The modem still chooses its serving carriers.',fields:[field('bands','LTE bands',{placeholder:'1, 3, 7, 28',help:'Comma-separated band numbers. Use the reset action to allow all bands.'})],label:'Apply LTE bands',endpoint:'/api/cell/band/lte',warning:lockWarning,body:values=>({is_lte_band:'1',lte_band_mask:A.bands(values.bands,88),is_gw_band:'0',gw_band_mask:''})});
  commandForm({route:'network/bands',id:'nr-band',title:'5G NR bands',description:'Set the band preference for the chosen 5G mode.',fields:[choices('mode','5G mode',[['nsa','NSA · LTE anchor'],['sa','Standalone']]),field('bands','NR bands',{placeholder:'41, 78',help:'Comma-separated NR band numbers.'})],label:'Apply NR bands',endpoint:'/api/cell/band/nr',warning:lockWarning,body:values=>({nr5g_type:values.mode,nr5g_band:A.bands(values.bands,1024)})});
  for (const [rat,label,max,channelMax] of [['lte','4G LTE',503,262143],['nr','5G NR',1007,3279165]]) commandForm({route:'network/bands',id:`${rat}-cell-lock`,title:`${label} cell lock`,description:'Use values from Signal & Cells. A carrier’s physical cell ID is not a physical site identifier.',fields:[field('pci','Physical cell ID',{type:'number',min:0,max}),field('earfcn',rat === 'nr' ? 'NR-ARFCN' : 'EARFCN',{type:'number',min:0,max:channelMax}),...(rat === 'nr' ? [field('band','NR band',{type:'number',min:1,max:1024,required:false})] : [])],label:`Lock ${label} cell`,endpoint:`/api/cell/lock/${rat}`,warning:lockWarning,body:values=>Object.fromEntries(Object.entries(values).filter(([,value])=>value !== ''))});
  commandForm({route:'network/bands',id:'band-reset',title:'Reset band restrictions',description:'Allow the modem to select supported bands automatically.',label:'Allow all bands',endpoint:'/api/cell/band/reset',warning:'Remove all LTE and NR band preferences. The modem may reconnect.'});
  commandForm({route:'network/bands',id:'cell-reset',title:'Reset cell locks',description:'Allow the modem to select its serving cells again.',label:'Unlock cells',endpoint:'/api/cell/lock/reset',warning:'Remove serving cell locks. The modem may reconnect.'});

  // ZTE WMS reports successful SMS submission with result 3, matching the agent forwarder.
  const composer = commandForm({resultCodes:[3,'3'],route:'sms/compose',id:'sms-compose',title:'New message',description:'Messages are sent from the router’s SIM. Carrier charges and multipart message limits apply.',fields:[field('number','Recipient',{type:'tel',placeholder:'+63…',maxLength:40}),field('message','Message',{type:'textarea',maxLength:1530,help:'Up to 1,530 characters. Unicode and long messages can use multiple SMS segments.'})],label:'Send message',confirmTitle:'Send this message?',warning:values=>`To: ${values.number}\n\n${values.message}\n\nThis sends from your router’s SIM and may incur carrier charges.`,endpoint:'/api/sms/send',body:values=>A.smsBody(values.number,values.message),accepted:'Send request accepted by the modem. Delivery is not confirmed; check the message list before retrying.',clearOnSuccess:true});
  const counter = el('p','field-help','0 characters'); composer.inputs.message.after(counter); composer.form.addEventListener('reset-complete',()=>{ counter.textContent = '0 characters'; }); composer.inputs.message.addEventListener('input',()=>{ const value = composer.inputs.message.value; counter.textContent = `${value.length.toLocaleString()} characters · ${A.smsBody('+1',value || ' ').encode_type === 'UNICODE' ? 'Unicode' : 'GSM alphabet'}`; });
  const smsPanel = document.querySelector('#messages-panel'), smsToolbar = el('div','message-toolbar'), selectionLabel = el('span','','0 selected');
  const smsFeedback = status(smsPanel); smsFeedback.id = 'sms-action-status';
  const refreshMessages = async current=>{
    const data = await request('/api/sms/list',json('POST',{page:0,data_per_page:500,mem_store:2,tags:10,order_by:'order by id desc'}));
    if (!Array.isArray(data.messages)) throw new Error('The router returned no message list');
    if (current()) consoleAPI.updateMessages(data); return data;
  };
  const selectVisible = button('Select visible',()=>{ for (const message of consoleAPI.messages()) { const query = document.querySelector('#sms-search').value.trim().toLowerCase(); if (`${message.number} ${message.body}`.toLowerCase().includes(query) && /^\d+$/.test(String(message.id))) selected.add(String(message.id)); } consoleAPI.redrawMessages(); updateSelection(); });
  const clearSelection = button('Clear selection',()=>{ selected.clear(); consoleAPI.redrawMessages(); updateSelection(); });
  async function smsAction(kind,ids) {
    let list = ids || [...selected];
    list = list.filter(id=>consoleAPI.messages().some(message=>String(message.id) === String(id) && (kind === 'delete' || [0,1].includes(Number(message.tag)))));
    if (!list.length) { say(smsFeedback,'Select received messages to change read status, or select messages to delete.',true); return; }
    const deletion = kind === 'delete';
    if (!await confirm(deletion ? 'Delete messages?' : 'Change read status?',`${deletion ? 'Permanently delete' : kind === 'read' ? 'Mark as read:' : 'Mark as unread:'} ${list.length} message${list.length === 1 ? '' : 's'}. ${deletion ? 'This cannot be undone.' : ''}`,{label:deletion ? 'Delete messages' : 'Apply',destructive:deletion})) return;
    await mutate(smsFeedback,async current=>{
      await request(`/api/sms/${deletion ? 'delete' : 'read'}`,json('POST',{id:A.ids(list),...(!deletion ? {tag:kind === 'read' ? 0 : 1} : {})})); if (!current()) return;
      const data = await refreshMessages(current); if (!current()) return;
      const confirmed = list.every(id=>deletion ? !(data.messages || []).some(message=>String(message.id) === id) : (data.messages || []).some(message=>String(message.id) === id && Number(message.tag) === (kind === 'read' ? 0 : 1)));
      if (confirmed) { for (const id of list) selected.delete(id); consoleAPI.redrawMessages(); updateSelection(); }
      say(smsFeedback,confirmed ? (deletion ? 'Message deletion confirmed.' : 'Read status confirmed.') : 'Request accepted, but the message list has not confirmed the change. Check before retrying.',!confirmed);
    });
  }
  const markRead = button('Mark read',()=>smsAction('read')), markUnread = button('Mark unread',()=>smsAction('unread')), deleteSMS = button('Delete',()=>smsAction('delete'), 'danger');
  smsToolbar.append(selectionLabel,selectVisible,clearSelection,markRead,markUnread,deleteSMS); document.querySelector('#message-list').before(smsToolbar);
  const newMessage = el('a','button primary','New message'); newMessage.href = '#sms/compose'; smsPanel.querySelector('.section-heading').append(newMessage);
  function updateSelection() { const existing = new Set(consoleAPI.messages().map(message=>String(message.id))); for (const id of selected) if (!existing.has(id)) selected.delete(id); selectionLabel.textContent = `${selected.size} selected`; clearSelection.disabled = markRead.disabled = markUnread.disabled = deleteSMS.disabled = !selected.size; }
  globalThis.OpenU60Controls = {decorateMessage(card,message) {
    const id = String(message.id); if (!/^\d+$/.test(id)) return;
    const actions = el('div','message-actions'), label = el('label','message-select'), check = el('input'); check.type = 'checkbox'; check.checked = selected.has(id); check.setAttribute('aria-label',`Select message ${id} from ${message.number}`); check.addEventListener('change',()=>{ check.checked ? selected.add(id) : selected.delete(id); updateSelection(); }); label.append(check,el('span','','Select'));
    const reply = button(Number(message.tag) === 2 ? 'Write again' : 'Reply',async()=>{
      if (composer.section.dataset.dirty === 'true' && !await confirm('Replace message draft?','Replying replaces your existing recipient and message.',{label:'Replace draft'})) return;
      composer.inputs.number.value = message.number || ''; composer.inputs.message.value = ''; composer.section.dataset.dirty = 'true'; OpenU60Navigation.go('sms/compose'); composer.inputs.message.focus();
    });
    actions.append(label,reply); if ([0,1].includes(Number(message.tag))) actions.append(button(Number(message.tag) === 1 ? 'Mark read' : 'Mark unread',()=>smsAction(Number(message.tag) === 1 ? 'read' : 'unread',[id])));
    actions.append(button('Delete',()=>smsAction('delete',[id]),'danger')); card.append(actions);
  }};
  updateSelection(); consoleAPI.redrawMessages();

  makeForm({route:'settings/device',id:'charge-config',title:'Battery charge limit',description:'Limit charging for long periods on USB power. Hysteresis controls when charging resumes below the limit.',endpoint:'/api/device/charge-control',fields:[choices('charge_limit_enabled','Charge limit',[['true','Enabled'],['false','Disabled']],{boolean:true}),field('charge_limit','Stop at (%)',{type:'number',min:50,max:100}),field('hysteresis','Resume below limit by (%)',{type:'number',min:1,max:20})]});
  makeForm({route:'settings/device',id:'manual-charge',title:'Manual charging control',description:'A manual stop overrides automatic charging policy until you allow charging again.',endpoint:'/api/device/charge-control',fields:[choices('charging_stopped','Charging',[['false','Allow charging'],['true','Stop charging']],{boolean:true})],warning:'Stopping charging can drain the battery while the router is running. This sets a manual override.'});
  makeForm({route:'settings/device',id:'power-save',title:'Power saver',description:'Reduce device power consumption using the router’s built-in mode.',endpoint:'/api/device/power-save',read:()=>request('/api/device/power-save',json('POST',{deviceInfoList:['power_saver_mode']})),fields:[onoff('power_saver_mode','Power saver')],body:changed=>({deviceInfoList:changed}),verify:(data,body)=>String(data.power_saver_mode) === String(body.deviceInfoList.power_saver_mode)});
  makeForm({route:'settings/device',id:'fast-boot',title:'Fast boot',description:'Choose whether the router uses its quick power-on setting.',endpoint:'/api/device/fast-boot',fields:[onoff('fast_boot','Fast boot')]});
  makeForm({route:'settings/device',id:'auto-sleep',title:'Automatic sleep',description:'Let the router sleep after inactivity. Sleeping can make this console unavailable until the device wakes.',endpoint:'/api/device/auto-sleep',fields:[choices('enabled','Automatic sleep',[['true','Enabled'],['false','Disabled']],{boolean:true}),field('timeout','Inactivity timeout (minutes)',{type:'number',min:-1,max:1440,help:'Use 1–1,440 minutes, or −1 for no timeout.'})],body:(changed,all)=>({enabled:all.enabled === 'true',timeout:all.timeout}),validate:(_,all)=>{ if (!['true','false'].includes(all.enabled)) throw new Error('Choose whether automatic sleep is enabled.'); if (!(all.timeout === '-1' || /^\d+$/.test(all.timeout) && Number(all.timeout) >= 1 && Number(all.timeout) <= 1440)) throw new Error('Enter 1–1,440 minutes, or −1.'); },warning:'Automatic sleep may disconnect clients and make the console unavailable. Wake the device using its physical controls if needed.'});
  commandForm({route:'settings/device',id:'device-reboot',title:'Restart router',description:'Restart the device. Connected clients will temporarily lose access.',endpoint:'/api/device/reboot',label:'Restart router',warning:'Restart the router now? Internet, Wi-Fi and this console will disconnect while it starts up.',danger:true,accepted:'Restart request accepted. Wait for the device to return; the live stream will reconnect automatically.'});
  commandForm({route:'settings/device',id:'device-reset',title:'Factory reset',description:'Erase router settings and restore factory defaults. You will need to set up the router again.',endpoint:'/api/device/factory-reset',label:'Erase and reset',warning:'This permanently erases router configuration, including network settings and Wi-Fi credentials. The console will disconnect. Type RESET to confirm.',danger:true,typed:true,accepted:'Factory reset requested. Reconnect using the factory network settings after the device restarts.'});

  function loadRoute() { if (consoleAPI.locked || !consoleAPI.connected) return; for (const view of views) if (view.route === route() && !view.loaded) view.load(); }
  window.addEventListener('dashboard:navigate',()=>{ loadRoute(); });
  window.addEventListener('dashboard:readings',event=>{ loadRoute(); for (const view of views) view.sync?.(event.detail.readings); updateSelection(); });
  window.addEventListener('dashboard:locked',()=>{ session++; finishConfirmation(false); selected.clear(); for (const view of views) view.clear(); smsFeedback.textContent = ''; counter.textContent = '0 characters'; updateSelection(); });
  window.addEventListener('beforeunload',event=>{ if (views.some(view=>view.dirty)) { event.preventDefault(); event.returnValue = ''; } });
  globalThis.OpenU60UI = {el,button,panel,status,say,json,request,confirm,mutate,makeForm,field,choices,onoff,
    track:view=>{ views.push(view); loadRoute(); },
    get epoch(){return session;}, get saving(){return mutationBusy;}, current:epoch=>epoch === session && !consoleAPI.locked
  };
  loadRoute();
})();
