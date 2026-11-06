'use strict';
(() => {
  const UI = OpenU60UI, API = OpenU60Console, A = OpenU60Advanced;
  const {el,button,say,json,confirm,mutate} = UI;
  let storedRules = [], editing = null, dirty = false, attempted = false, loading = false, choosing = false, loadedRevision = null, latest = null;
  const types = [['telegram','Telegram'],['webhook','Webhook'],['sms','SMS'],['ntfy','ntfy'],['discord','Discord'],['slack','Slack']];
  const typeLabel = type => types.find(([key])=>key === type)?.[1] || type;
  function privateError(error) {
    let message = String(error.message).replace(/https?:\/\/[^\s)]+/g,'[private endpoint]');
    const secrets = [...storedRules,editing].filter(Boolean).flatMap(rule=>{
      const dest = rule.destination || {};
      return [dest.bot_token,dest.token,dest.webhook_url,dest.url,...(dest.headers || []).map(header=>header.value)];
    });
    try { secrets.push(...A.headers(inputs.headers.value).map(header=>header.value)); } catch { /* Incomplete drafts can still be edited. */ }
    for (const input of document.querySelectorAll('#forward-rule input[type=password]')) secrets.push(input.value);
    for (const secret of secrets.filter(value=>value && String(value).length > 3).sort((a,b)=>b.length-a.length)) message = message.split(secret).join('[private]');
    error.message = message; return error;
  }
  async function request(...args) { try { return await UI.request(...args); } catch (error) { throw privateError(error); } }
  UI.makeForm({route:'sms/forwarding',id:'forward-config',title:'Forwarding service',description:'Forward new incoming messages using enabled rules. Delivery is event driven, with a fallback check if notifications are missed.',endpoint:'/api/sms/forward/config',source:'forwarding',liveData:data=>data.config,read:async()=>{ const data = await request('/api/sms/forward/config'); return data.config; },fields:[
    UI.choices('enabled','Forwarding service',[['false','Disabled'],['true','Enabled']],{boolean:true}),
    UI.field('poll_interval_secs','Fallback check interval (seconds)',{type:'number',min:10,max:86400}),
    UI.choices('mark_read_after_forward','After successful forwarding',[['false','Keep read status'],['true','Mark message read']],{boolean:true}),
    UI.choices('delete_after_forward','Stored message',[['false','Keep message'],['true','Delete after forwarding']],{boolean:true})
  ],warning:'Forwarding shares matching incoming messages, including any private content or verification codes, with your chosen destinations. SMS destinations may incur carrier charges. Deleting after forwarding permanently removes those messages.'});

  const section = UI.panel('sms/forwarding','forward-rules','Message routing rules','Choose what to match and where it goes. New rules start disabled, so you can test a destination before enabling it.');
  const toolbar = el('div','form-actions rule-toolbar'), newRule = button('New rule',()=>chooseRule(null),'primary'), reload = button('Read rules',()=>reloadRules());
  toolbar.append(newRule,reload); section.append(toolbar);
  const rulesList = el('div','rule-list'); section.append(rulesList);
  const feedback = UI.status(section);
  const editor = el('form','settings-form forward-editor'), heading = el('h3','form-group-heading','New rule'), fields = el('fieldset','form-grid'); fields.disabled = true;
  const inputs = {}, wrappers = {};
  const specs = [
    ['name','Rule name'],['enabled','Rule status',[['false','Disabled'],['true','Enabled']]],
    ['filter','Match messages',[['all','All incoming messages'],['sender','Sender'],['content','Message content'],['sender_and_content','Sender and content']]],
    ['patterns','Sender patterns',null,'textarea','Comma-separated numbers or sender names. A single * matches a prefix or suffix, for example +63917*. Matching ignores case.'],
    ['keywords','Message keywords',null,'textarea','Comma-separated keywords. Any keyword can match; matching ignores case.'],
    ['destination','Destination',types],
    ['bot_token','New bot token',null,'password','Leave blank to keep the saved token when editing.'],
    ['chat_id','Chat ID or @channel'],['silent','Telegram notification',[['false','Normal'],['true','Silent']]],
    ['url','New webhook address',null,'password','Leave blank to keep the saved address. Addresses can contain private tokens.'],
    ['method','Request method',[['POST','POST'],['PUT','PUT']]],
    ['headers','Replacement request headers',null,'textarea','One Name: value per line. Leave blank to keep saved headers; use Remove stored headers to clear them.'],
    ['clear_headers','Remove stored headers',null,'checkbox'],
    ['forward_number','Forwarding phone number'],
    ['ntfy_url','ntfy server address'],['topic','ntfy topic'],
    ['token','New access token',null,'password','Optional. Leave blank to keep the saved token.'],['clear_token','Remove stored access token',null,'checkbox'],
    ['webhook_url','New incoming webhook address',null,'password','Leave blank to keep the saved address.'],
  ];
  for (const [key,label,values,type,help] of specs) {
    const wrapper = el('div',`form-field ${type === 'checkbox' ? 'checkbox-field' : ''}`), labelNode = el('label','',label), input = el(values ? 'select' : type === 'textarea' ? 'textarea' : 'input');
    input.id = `forward-${key}`; input.name = key; labelNode.htmlFor = input.id;
    if (values) for (const [value,text] of values) input.add(new Option(text,value));
    else if (type !== 'textarea') input.type = type || 'text';
    input.autocomplete = type === 'password' ? 'new-password' : 'off';
    if (type !== 'checkbox' && !values) input.maxLength = key === 'headers' ? 4000 : 1000;
    wrapper.append(labelNode,input); if (help) wrapper.append(el('small','field-help',help));
    inputs[key] = input; wrappers[key] = wrapper; fields.append(wrapper);
  }
  const credentialNote = el('p','field-help','Stored tokens, webhook addresses and request headers are never filled into the editor. Blank replacements keep the saved value for this destination.');
  const actions = el('div','form-actions'), save = button('Save rule',null,'primary'); save.type = 'submit';
  const test = button('Test destination',testDestination), discard = button('Discard edits',()=>beginEdit(editing?.id ? storedRules.find(rule=>rule.id === editing.id) || null : null));
  actions.append(save,test,discard); editor.append(heading,fields,credentialNote,actions); section.append(editor);
  function values() { return Object.fromEntries(Object.entries(inputs).map(([key,input])=>[key,input.type === 'checkbox' ? input.checked : input.value])); }
  function showFields() {
    const filter = inputs.filter.value, type = inputs.destination.value;
    const visible = new Set(['name','enabled','filter','destination']);
    if (['sender','sender_and_content'].includes(filter)) visible.add('patterns');
    if (['content','sender_and_content'].includes(filter)) visible.add('keywords');
    for (const key of ({telegram:['bot_token','chat_id','silent'],webhook:['url','method','headers','clear_headers'],sms:['forward_number'],ntfy:['ntfy_url','topic','token','clear_token'],discord:['webhook_url'],slack:['webhook_url']}[type] || [])) visible.add(key);
    for (const [key,wrapper] of Object.entries(wrappers)) wrapper.hidden = !visible.has(key);
  }
  function beginEdit(rule) {
    editing = rule ? structuredClone(rule) : null;
    const filter = rule?.filter || {type:'all'}, dest = rule?.destination || {type:'telegram'};
    for (const input of Object.values(inputs)) { if (input.type === 'checkbox') input.checked = false; else input.value = ''; }
    Object.assign(inputs.name,{value:rule?.name || ''}); inputs.enabled.value = String(rule?.enabled || false);
    inputs.filter.value = filter.type; inputs.patterns.value = (filter.patterns || []).join(', '); inputs.keywords.value = (filter.keywords || []).join(', ');
    inputs.destination.value = dest.type; inputs.chat_id.value = dest.chat_id || ''; inputs.silent.value = String(dest.silent || false);
    inputs.method.value = dest.method || 'POST'; inputs.forward_number.value = dest.forward_number || '';
    inputs.ntfy_url.value = dest.type === 'ntfy' ? dest.url : 'https://ntfy.sh'; inputs.topic.value = dest.topic || '';
    heading.textContent = rule ? `Edit rule · ${rule.name}` : 'New rule';
    dirty = false; section.dataset.dirty = 'false'; save.disabled = true; fields.disabled = false; discard.disabled = true; showFields();
  }
  editor.addEventListener('input',()=>{ dirty = true; section.dataset.dirty = 'true'; save.disabled = discard.disabled = false; showFields(); say(feedback,'Unsaved rule edits · live updates keep your draft.'); });
  editor.addEventListener('change',()=>editor.dispatchEvent(new Event('input')));
  async function getConfig() {
    const data = await request('/api/sms/forward/config');
    if (!data?.config || !Array.isArray(data.config.rules)) throw new Error('The router returned no forwarding rules.');
    return data;
  }
  function drawRules(rules) {
    rulesList.replaceChildren();
    if (!rules.length) { rulesList.append(el('p','carrier-empty','No forwarding rules yet. Create a disabled rule and test its destination first.')); return; }
    for (const rule of rules) {
      const card = el('article','rule-card'), copy = el('div'), name = el('h3','',rule.name), meta = el('p','field-help',`${typeLabel(rule.destination_type || rule.destination?.type)} · ${rule.enabled ? 'Enabled' : 'Disabled'}`), controls = el('div','form-actions rule-actions');
      copy.append(name,meta);
      controls.append(button('Edit',()=>chooseRule(rule.id)),button(rule.enabled ? 'Disable' : 'Enable',()=>toggleRule(rule)),button('Delete',()=>deleteRule(rule),'danger'));
      card.append(copy,controls); rulesList.append(card);
    }
  }
  async function loadRules({silent = false} = {}) {
    if (loading || API.locked) return; loading = true; attempted = true; const epoch = UI.epoch;
    newRule.disabled = reload.disabled = true;
    try {
      const data = await getConfig(); if (!UI.current(epoch) || UI.saving || (loadedRevision !== null && data.revision < loadedRevision)) return;
      storedRules = data.config.rules; loadedRevision = data.revision;
      drawRules(storedRules);
      if (!dirty && !choosing) beginEdit(editing ? storedRules.find(rule=>rule.id === editing.id) || null : null);
      else say(feedback,'Rules were updated on the router. Your draft is kept; saving checks for conflicting edits.');
      if (!dirty && !silent) say(feedback,'Current rules loaded.');
    } catch (error) { if (UI.current(epoch)) say(feedback,`Rules unavailable. ${error.message}`,true); }
    finally { loading = false; if (UI.current(epoch)) { newRule.disabled = loadedRevision === null; reload.disabled = false; } }
  }
  async function reloadRules() {
    if (dirty && !await confirm('Discard rule edits?','Read the router’s current rules and replace this draft.',{label:'Discard and read'})) return;
    dirty = false; await loadRules();
  }
  async function chooseRule(id) {
    if (choosing) return;
    if (dirty && !await confirm('Replace rule draft?','This replaces your unsaved rule edits.',{label:'Replace draft'})) return;
    const epoch = UI.epoch; choosing = true; fields.disabled = true; newRule.disabled = true;
    try {
      const data = await getConfig(); if (!UI.current(epoch)) return; storedRules = data.config.rules; loadedRevision = data.revision;
      const rule = id === null ? null : storedRules.find(rule=>rule.id === id); if (id !== null && !rule) throw new Error('This rule no longer exists. Read the current rules.');
      beginEdit(rule); heading.scrollIntoView({block:'center',behavior:'smooth'}); inputs.name.focus({preventScroll:true});
    } catch (error) { if (UI.current(epoch)) say(feedback,error.message,true); }
    finally { choosing = false; if (UI.current(epoch)) { fields.disabled = loadedRevision === null; newRule.disabled = loadedRevision === null; } }
  }
  editor.addEventListener('submit',async event=>{
    event.preventDefault(); if (!dirty || fields.disabled) return;
    try {
      const body = A.rule(values(),editing);
      if (!await confirm('Save forwarding rule?',`Save “${body.name}” with ${typeLabel(body.destination.type)} as its destination. ${body.enabled ? 'Matching incoming messages can be forwarded while the service is enabled.' : 'This rule will remain disabled.'}`,{label:'Save rule'})) return;
      await mutate(feedback,async current=>{
        const fresh = await getConfig(); if (!current()) return;
        if (editing && A.canonical(fresh.config.rules.find(rule=>rule.id === editing.id)) !== A.canonical(editing)) throw new Error('This rule changed elsewhere. Read the current rules and review your draft before saving.');
        if (fresh.config.rules.some(rule=>rule.id !== editing?.id && rule.name.toLowerCase() === body.name.toLowerCase())) throw new Error('A rule with this name already exists. Edit it or use a different name.');
        const result = await request('/api/sms/forward/rules',json(editing ? 'PUT' : 'POST',body)); if (!current()) return;
        const check = await getConfig(); if (!current()) return;
        const saved = check.config.rules.find(rule=>rule.id === (editing?.id ?? result.id));
        storedRules = check.config.rules; loadedRevision = check.revision; drawRules(storedRules);
        if (!saved || A.canonical(saved) !== A.canonical({...body,id:saved.id})) { say(feedback,'The save request was accepted, but the router has not confirmed this rule. Your draft is kept.',true); return; }
        beginEdit(saved); say(feedback,'Rule saved and confirmed by the router.');
      });
    } catch (error) { say(feedback,privateError(error).message,true); }
  });
  function describe(dest) {
    if (dest.type === 'sms') return `SMS to ${dest.forward_number}`;
    if (dest.type === 'telegram') return `Telegram chat ${dest.chat_id}`;
    try { return `${typeLabel(dest.type)} at ${new URL(dest.url || dest.webhook_url).host}`; } catch { return typeLabel(dest.type); }
  }
  async function testDestination() {
    try {
      const destination = A.destination(values(),editing?.destination);
      if (!await confirm('Send a destination test?',`Send one fixed test message to ${describe(destination)}. This sends even if the rule or forwarding service is disabled. SMS destinations may incur carrier charges.`,{label:'Send test'})) return;
      await mutate(feedback,async current=>{ await request('/api/sms/forward/test',{...json('POST',{destination}),timeoutMs:25000}); if (current()) say(feedback,'The destination accepted the test request. SMS acceptance does not confirm handset delivery.'); });
    } catch (error) { say(feedback,privateError(error).message,true); }
  }
  async function toggleRule(rule) {
    if (!await confirm(rule.enabled ? 'Disable this rule?' : 'Enable this rule?',`${rule.enabled ? 'Stop forwarding through' : 'Forward matching incoming messages through'} “${rule.name}”. The forwarding service must also be enabled.`,{label:rule.enabled ? 'Disable rule' : 'Enable rule'})) return;
    await mutate(feedback,async current=>{ await request('/api/sms/forward/rules/toggle',json('PUT',{id:rule.id,enabled:!rule.enabled})); if (!current()) return; const data = await getConfig(); if (!current()) return; storedRules = data.config.rules; loadedRevision = data.revision; drawRules(storedRules); say(feedback,storedRules.some(item=>item.id === rule.id && item.enabled === !rule.enabled) ? 'Rule status confirmed.' : 'The router has not confirmed the rule status. Read rules before retrying.'); });
  }
  async function deleteRule(rule) {
    if (!await confirm('Delete forwarding rule?',`Permanently remove “${rule.name}”. Past log entries remain, but retries will no longer have this destination.`,{label:'Delete rule',destructive:true})) return;
    await mutate(feedback,async current=>{ await request('/api/sms/forward/rules',json('DELETE',{id:rule.id})); if (!current()) return; const data = await getConfig(); if (!current()) return; storedRules = data.config.rules; loadedRevision = data.revision; drawRules(storedRules); if (!storedRules.some(item=>item.id === rule.id)) { if (editing?.id === rule.id) beginEdit(null); say(feedback,'Rule deletion confirmed.'); } else say(feedback,'The delete request was accepted, but the rule is still reported. Read rules before retrying.',true); });
  }

  const logPanel = UI.panel('sms/forwarding','forward-log','Delivery activity','Live delivery outcomes. Held messages wait for connectivity and retry automatically.');
  const logToolbar = el('div','form-actions rule-toolbar'), logFilter = el('select'); logFilter.id = 'forward-log-filter'; logFilter.setAttribute('aria-label','Filter forwarding activity');
  for (const [value,label] of [['all','All activity'],['pending','Held for connectivity'],['failed','Failed'],['success','Accepted']]) logFilter.add(new Option(label,value));
  const clear = button('Clear settled activity',clearLog); logToolbar.append(logFilter,clear); logPanel.append(logToolbar);
  const logList = el('div','forward-log-list'); logPanel.append(logList); const logFeedback = UI.status(logPanel);
  function drawLog() {
    logList.replaceChildren(); const entries = latest?.log || [];
    const found = entries.filter(entry=>logFilter.value === 'all' || (logFilter.value === 'pending' ? entry.pending : logFilter.value === 'success' ? entry.success : !entry.success && !entry.pending));
    if (!found.length) logList.append(el('p','carrier-empty','No matching delivery activity.'));
    for (const entry of found) {
      const card = el('article','forward-log-entry'), top = el('div','message-card-heading');
      top.append(el('strong','',entry.rule_name || 'Deleted rule'),el('span','tag',entry.pending ? 'Held' : entry.success ? 'Accepted' : 'Failed'));
      card.append(top,el('p','',entry.content_preview || 'No message preview'),el('small','field-help',`${entry.sender || 'Unknown sender'} · ${typeLabel(entry.destination_type)} · ${new Date(entry.timestamp*1000).toLocaleString()}`));
      if (entry.error) card.append(el('p','action-status',entry.error));
      if (!entry.success && !entry.pending) { const actions = el('div','form-actions rule-actions'); actions.append(button('Retry once',()=>retry(entry))); card.append(actions); }
      logList.append(card);
    }
  }
  logFilter.addEventListener('change',drawLog);
  async function retry(entry) {
    const epoch = UI.epoch;
    try {
      const data = await getConfig(); if (!UI.current(epoch)) return;
      const rule = data.config.rules.find(rule=>rule.id === entry.rule_id);
      if (!rule) { say(logFeedback,'This rule no longer exists. Its old destination cannot be retried.',true); return; }
      if (!await confirm('Retry this forward once?',`Forward the stored message from ${entry.sender} to the current destination: ${describe(rule.destination)}. Preview: ${entry.content_preview || 'unavailable'}. This can create a duplicate if the destination accepted an earlier request.`,{label:'Retry once'})) return;
      await mutate(logFeedback,async current=>{ await request('/api/sms/forward/retry',{...json('POST',{index:entry.index,expected:{sms_id:entry.sms_id,rule_id:entry.rule_id,timestamp:entry.timestamp},expected_rule_revision:data.revision}),timeoutMs:25000}); if (current()) say(logFeedback,'The destination accepted the retry request. The live activity log will show the outcome.'); });
    } catch (error) { if (UI.current(epoch)) say(logFeedback,privateError(error).message,true); }
  }
  async function clearLog() {
    if (!await confirm('Clear settled forwarding activity?','Remove settled log entries where possible. Messages waiting to forward, and their related activity, stay in the log.',{label:'Clear activity',destructive:true})) return;
    await mutate(logFeedback,async current=>{ await request('/api/sms/forward/log/clear',json('POST',{})); if (current()) say(logFeedback,'Clear request accepted. Waiting messages and their related activity are retained; the live log will update automatically.'); });
  }
  function refreshChangedRules() {
    if (latest && (loadedRevision === null || latest.revision > loadedRevision) && !loading && !UI.saving && OpenU60Navigation.current.key === 'sms/forwarding') loadRules({silent:true});
  }
  window.addEventListener('dashboard:mutation-complete',refreshChangedRules);
  window.addEventListener('dashboard:readings',event=>{
    if (event.detail.snapshot && OpenU60Navigation.current.key === 'sms/forwarding') loadedRevision = null;
    const reading = event.detail.readings.forwarding;
    if (!reading) return;
    if (!reading.ok) { say(logFeedback,'Forwarding activity is unavailable. Last received entries are retained.',true); return; }
    latest = reading.data; drawLog(); drawRules(latest.rules || []);
    refreshChangedRules();
  });
  UI.track({route:'sms/forwarding',get loaded(){return attempted;},load:loadRules,get dirty(){return dirty;},clear:()=>{
    storedRules = []; editing = latest = null; loadedRevision = null; attempted = dirty = choosing = false; fields.disabled = true;
    for (const input of Object.values(inputs)) { if (input.type === 'checkbox') input.checked = false; else input.value = ''; }
    rulesList.replaceChildren(); logList.replaceChildren(); logFilter.value = 'all'; feedback.textContent = logFeedback.textContent = ''; section.dataset.dirty = 'false'; save.disabled = newRule.disabled = true;
  }});
  newRule.disabled = true; save.disabled = true; showFields();
})();
