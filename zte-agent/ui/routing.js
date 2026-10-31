'use strict';
(() => {
  const UI = OpenU60UI, API = OpenU60Console, R = OpenU60Routing;
  const {el,button,field,onoff,choices,say,request,json,confirm,mutate} = UI;
  const lanKeys = ['lan_ipaddr','lan_netmask','dhcp_enable','dhcp_start','dhcp_end','dhcp_lease_time'];
  const samples = new Map();
  const enabled = value => ['1','true','on'].includes(String(value).toLowerCase());
  const state = value => value === undefined || value === null || value === '' ? 'Not reported' : enabled(value) ? 'Enabled' : ['0','false','off'].includes(String(value).toLowerCase()) ? 'Disabled' : String(value);
  const readLAN = async()=>R.normalizeLAN(await request('/api/router/lan'));
  function setting(endpoint,keys) {
    return async()=>{
      const data = await request(endpoint);
      if (!data || !keys.every(key=>Object.hasOwn(data,key))) throw new Error('The router returned incomplete settings');
      return R.normalizeSwitches(data);
    };
  }
  function summary(route,id,title,description,definitions) {
    const section = UI.panel(route,id,title,description), list = el('dl','routing-stats'), nodes = {};
    for (const [key,label,numeric] of definitions) {
      const pair = el('div'), value = el('dd',numeric ? 'mono' : '', '—');
      pair.append(el('dt','',label),value); list.append(pair); nodes[key] = {value,numeric};
    }
    const caption = el('p','reading-caption','Waiting for live router settings.'); section.append(list,caption);
    return {section,nodes,caption};
  }
  function values(summary,data) { for (const [key,{value,numeric}] of Object.entries(summary.nodes)) {
    const text = data[key] ?? '—'; if (numeric) OpenU60Numbers.set(value,text); else value.textContent=text;
  } }
  function stamp(summary,names) {
    const available = names.map(name=>samples.get(name));
    const received = available.filter(Boolean).map(sample=>sample.at);
    const unavailable = available.some(sample=>!sample || sample.unavailable);
    summary.caption.textContent = received.length ? `${API.connected && !unavailable ? 'Live settings' : 'Last received settings'} · ${new Date(Math.min(...received)).toLocaleTimeString()}${unavailable ? ' · some readings unavailable' : ''}` : 'Waiting for live router settings.';
    summary.caption.dataset.stale = String(!API.connected || unavailable);
  }
  const lanSummary = summary('network/lan','lan-overview','Your local network','Local addresses stay separate from your mobile internet connection.',[['address','Router address'],['dhcp','DHCP server'],['pool','Addresses in pool',true],['leases','Reported leases',true]]);
  const lanForm = UI.makeForm({route:'network/lan',id:'lan-config',title:'LAN address & DHCP',description:'Choose the local gateway and address pool. Pool addresses must belong to the same subnet and exclude the router.',endpoint:'/api/router/lan',read:readLAN,source:'lan_config',liveData:data=>{try{return R.normalizeLAN(data);}catch{return null;}},guardKeys:lanKeys,
    fields:[field('lan_ipaddr','Router IPv4 address',{required:true,placeholder:'192.168.0.1'}),field('lan_netmask','Subnet mask',{required:true,placeholder:'255.255.255.0'}),onoff('dhcp_enable','DHCP server'),field('dhcp_start','First pool address',{help:'A full IPv4 address, for example 192.168.0.100.'}),field('dhcp_end','Last pool address'),field('dhcp_lease_time','Lease duration',{help:'Seconds or a duration such as 12h or 1d. Existing units are preserved.'})],
    validate:(_,all)=>R.lanBody(all),body:(_,all)=>R.lanBody(all),
    warning:(_,all,baseline)=>`Changing LAN or DHCP settings can disconnect devices. ${all.lan_ipaddr!==baseline.lan_ipaddr ? `The console’s new local address will be ${new URL(R.reconnectURL(location.origin,all.lan_ipaddr)).host}. ` : ''}${all.dhcp_enable==='0' ? 'With DHCP off, devices need a manually assigned address or another DHCP server. ' : ''}Reconnect to the local network after saving.`,
    onWriting:body=>{ reconnect.href=R.reconnectURL(location.origin,body.lan_ipaddr); reconnect.textContent=`Open console at ${new URL(reconnect.href).host}`; reconnectNote.hidden=false; },
    verify:(data,body)=>lanKeys.every(key=>key==='dhcp_lease_time' ? R.lease(data[key])===R.lease(body[key]) : String(data[key])===String(body[key]))
  });
  const reconnectNote = el('div','routing-reconnect'); reconnectNote.hidden=true;
  const reconnect = el('a','button'); reconnectNote.append(el('p','','If the console disconnects, reconnect to the router’s network and open this address. A lost response does not confirm the change.'),reconnect); lanForm.section.append(reconnectNote);
  const leases = UI.panel('network/lan','lan-leases','DHCP leases','Reported leases may include devices that recently disconnected. Use a stable device address for DMZ and port-forward destinations.');
  const leaseWrap = el('div','table-scroll diagnostic-table'), leaseTable = el('table'), leaseHead = el('thead'), leaseHeader = el('tr'), leaseBody = el('tbody'); leaseTable.setAttribute('aria-label','DHCP leases');
  for (const text of ['DEVICE','ADDRESS','MAC ADDRESS']) { const th=el('th','',text); th.scope='col'; leaseHeader.append(th); }
  leaseHead.append(leaseHeader); leaseTable.append(leaseHead,leaseBody); leaseWrap.append(leaseTable); leases.append(leaseWrap);
  function drawLeases() {
    const data = samples.get('clients')?.data;
    const entries = Array.isArray(data?.dhcp_leases) ? data.dhcp_leases : null;
    if (!entries?.length) { const row=el('tr'), cell=el('td','empty',entries ? 'No DHCP leases reported.' : 'Lease readings unavailable.'); cell.colSpan=3; row.append(cell); leaseBody.replaceChildren(row); return; }
    const existing = new Map([...leaseBody.children].map(row=>[row.dataset.key,row])), ordered=[];
    for (const [index,entry] of entries.entries()) {
      const key=JSON.stringify([entry.macaddr,entry.ipaddr,index]); let row=existing.get(key);
      if (!row) {row=el('tr');row.dataset.key=key;for(const label of ['Device','Address','MAC address']){const cell=el('td',label==='Device' ? '' : 'mono');cell.dataset.label=label;row.append(cell);}}
      [entry.hostname,entry.ipaddr,entry.macaddr].forEach((text,index)=>{row.children[index].textContent=text || '—';}); ordered.push(row);
    }
    const wanted=new Set(ordered); for(const row of [...leaseBody.children])if(!wanted.has(row))row.remove();
    ordered.forEach((row,index)=>{if(leaseBody.children[index]!==row)leaseBody.insertBefore(row,leaseBody.children[index] || null);});
  }
  const firewallSummary = summary('network/firewall','firewall-overview','Inbound access','Review which services can accept connections from outside your local network.',[['firewall','Firewall'],['nat','NAT'],['upnp','UPnP'],['dmz','DMZ host']]);
  function simple({id,title,description,key,endpoint='/api/router/firewall',writeEndpoint,source='firewall_config',warning,values}) {
    return UI.makeForm({route:'network/firewall',id,title,description,endpoint,writeEndpoint,source,read:setting(endpoint,[key]),liveData:R.normalizeSwitches,guardKeys:[key],fields:[values ? choices(key,title,values) : onoff(key,title)],validate:(_,all)=>values ? undefined : R.switchValue(all[key],title),warning});
  }
  simple({id:'firewall-switch',title:'Firewall protection',description:'Enable or disable the router’s inbound firewall.',key:'firewall_switch',writeEndpoint:'/api/router/firewall/switch',warning:'Changing firewall protection can affect incoming connections. Disabling it removes the router’s inbound filtering. Apply this change?'});
  simple({id:'firewall-level',title:'Protection level',description:'Use the filtering level provided by your router’s firmware.',key:'firewall_level',writeEndpoint:'/api/router/firewall/level',values:[['low','Low'],['medium','Medium'],['high','High']],warning:'A different firewall level may interrupt existing connections or block services. Apply this level?'});
  simple({id:'nat-config',title:'Network address translation',description:'NAT allows local devices to share the router’s internet address.',key:'nat_switch',writeEndpoint:'/api/router/firewall/nat',warning:'Changing NAT may interrupt internet access for local devices. Keep a local connection to the router.'});
  simple({id:'upnp-config',title:'UPnP',description:'Let applications on local devices request incoming port mappings automatically.',key:'upnp_switch',endpoint:'/api/router/firewall/upnp',source:'upnp_config',warning:'UPnP lets applications request inbound access automatically. Apply this change for devices on your local network?'});
  UI.makeForm({route:'network/firewall',id:'dmz-config',title:'DMZ host',description:'Send otherwise unmatched incoming connections to one local device. Use a device with its own firewall and a stable address.',endpoint:'/api/router/firewall',writeEndpoint:'/api/router/firewall/dmz',read:setting('/api/router/firewall',['dmz_enabled','dmz_ip']),liveData:R.normalizeSwitches,source:'firewall_config',guardKeys:['dmz_enabled','dmz_ip'],fields:[onoff('dmz_enabled','DMZ'),field('dmz_ip','Destination IPv4 address',{placeholder:'192.168.0.10'})],
    validate:async(_,all)=>{R.switchValue(all.dmz_enabled,'DMZ');if(all.dmz_ip.trim())R.ipv4(all.dmz_ip,'DMZ address');if(all.dmz_enabled==='1')R.host(all.dmz_ip,await readLAN(),'DMZ address');},
    body:(_,all)=>({dmz_enabled:all.dmz_enabled,dmz_ip:all.dmz_ip.trim()}),preflight:async body=>{if(body.dmz_enabled==='1')R.host(body.dmz_ip,await readLAN(),'DMZ address');},
    warning:(_,all)=>all.dmz_enabled==='1' ? `DMZ exposes ${all.dmz_ip} to otherwise unmatched incoming traffic. Confirm this device has its own firewall before enabling DMZ.` : 'Disable the DMZ host? Incoming connections to it may stop.'
  });
  const portsSummary = summary('network/ports','ports-overview','Explicit incoming connections','A rule maps one external port to one local device. Mobile carriers may block incoming traffic or use carrier-grade NAT.',[['state','Port forwarding'],['rules','Reported rules',true],['enabled','Enabled rules',true],['upnp','UPnP']]);
  UI.makeForm({route:'network/ports',id:'ports-switch',title:'Port forwarding service',description:'The global switch controls whether configured port-forward rules are active.',endpoint:'/api/router/firewall',writeEndpoint:'/api/router/firewall/port-forward/switch',read:setting('/api/router/firewall',['port_forward_switch']),liveData:R.normalizeSwitches,source:'firewall_config',guardKeys:['port_forward_switch'],fields:[onoff('port_forward_switch','Port forwarding')],validate:(_,all)=>R.switchValue(all.port_forward_switch,'port forwarding'),warning:'Changing the global switch affects all configured port-forward rules. Apply this change?'});
  const ports = UI.panel('network/ports','port-rules','Forwarding rules','Add a mapping or remove a reported rule. External and internal ports must be between 1 and 65535.'), toolbar = el('div','form-actions rule-toolbar');
  const reload = button('Read current rules',()=>loadRules()); toolbar.append(reload); ports.append(toolbar);
  const ruleList = el('div','rule-list'), portCaption = el('p','reading-caption','Waiting for live rule readings.'); ports.append(ruleList,portCaption);
  const form = el('form','settings-form forward-editor'); form.autocomplete='off';
  const fields = el('fieldset','form-grid'); fields.disabled=true; fields.append(el('legend','form-group-heading','New port-forward rule'));
  const inputs = {};
  for (const [key,label,options] of [['name','Rule name'],['protocol','Protocol',[['tcp','TCP'],['udp','UDP'],['tcp+udp','TCP + UDP']]],['wan_port','External port'],['lan_ip','Local device address'],['lan_port','Internal port'],['enabled','Rule status',[['1','Enabled'],['0','Disabled']]]]) {
    const wrapper=el('div','form-field'), labelNode=el('label','',label), input=el(options ? 'select' : 'input');input.id=`port-rule-${key}`;input.name=key;labelNode.htmlFor=input.id;
    if(options)for(const [value,text]of options)input.add(new Option(text,value));else{input.type='text';input.required=true;input.autocomplete='off';if(key.endsWith('_port')){input.inputMode='numeric';input.maxLength=5;}if(key==='name')input.maxLength=64;}
    wrapper.append(labelNode,input);fields.append(wrapper);inputs[key]=input;
  }
  const actions=el('div','form-actions'),add=button('Add rule',null,'primary'),discard=button('Discard draft',resetDraft);add.type='submit';actions.append(add,discard);form.append(fields,actions);ports.append(form);const feedback=UI.status(ports);
  let rawRules=null, ruleEntries=[], attempted=false, loading=false, dirty=false, healthy=false;
  const draft=()=>Object.fromEntries(Object.entries(inputs).map(([key,input])=>[key,input.value]));
  function resetDraft(){form.reset();dirty=false;ports.dataset.dirty='false';updateActions();}
  function updateActions(){fields.disabled=!rawRules || !healthy || !API.connected;add.disabled=!dirty || !rawRules || !healthy || !API.connected;discard.disabled=!dirty;reload.disabled=loading || !API.connected;for(const card of ruleList.children)if(card.deleteButton)card.deleteButton.disabled=!API.connected || !healthy || !card.rule.removable;}
  form.addEventListener('input',()=>{dirty=!!(inputs.name.value || inputs.wan_port.value || inputs.lan_ip.value || inputs.lan_port.value);ports.dataset.dirty=String(dirty);updateActions();if(dirty)say(feedback,'Unsaved rule · live updates keep your draft.');});
  form.addEventListener('change',()=>form.dispatchEvent(new Event('input')));
  function applyRules(data){ruleEntries=R.rules(data);rawRules=data.rule_list;healthy=true;drawRules();updateActions();renderSummaries();}
  function drawRules(){
    if(!ruleEntries.length){ruleList.replaceChildren(el('p','empty','No port-forward rules configured.'));return;}
    const existing=new Map([...ruleList.children].filter(node=>node.dataset.ruleId).map(node=>[node.dataset.ruleId,node])),ordered=[];
    for(const [index,rule]of ruleEntries.entries()){
      const key=JSON.stringify([rule.id,index]);let card=existing.get(key);
      if(!card){card=el('article','rule-card');card.dataset.ruleId=key;const copy=el('div'),top=el('div','title-count'),title=el('h3'),tag=el('span','tag'),mapping=el('p','mono'),buttons=el('div','form-actions rule-actions'),remove=button('Delete',()=>deleteRule(card.rule),'danger');top.append(title,tag);copy.append(top,mapping);buttons.append(remove);card.append(copy,buttons);Object.assign(card,{heading:title,tag,mapping,deleteButton:remove});}
      card.rule=rule;card.heading.textContent=rule.name || 'Unnamed rule';card.tag.textContent=state(rule.enabled);card.mapping.textContent=`${String(rule.protocol || 'Not reported').toUpperCase()} · ${rule.wan_port || '—'} → ${rule.lan_ip || '—'}:${rule.lan_port || '—'}`;card.deleteButton.setAttribute('aria-label',`Delete ${rule.name || 'unnamed rule'}`);ordered.push(card);
    }
    const wanted=new Set(ordered);for(const card of [...ruleList.children])if(!wanted.has(card))card.remove();ordered.forEach((card,index)=>{if(ruleList.children[index]!==card)ruleList.insertBefore(card,ruleList.children[index] || null);});
  }
  async function loadRules(){
    if(loading || API.locked || !API.connected || UI.saving)return;loading=true;attempted=true;updateActions();const epoch=UI.epoch;
    try{const data=await request('/api/router/firewall/port-forward');if(!UI.current(epoch))return;applyRules(data);say(feedback,'Current rules loaded.');}
    catch(error){if(UI.current(epoch)){healthy=false;updateActions();say(feedback,`Rules unavailable. ${error.message}. Read current rules to try again.`,true);}}
    finally{loading=false;updateActions();}
  }
  async function verifiedRules(check,current){
    for(const wait of [0,700,1400]){
      if(wait)await new Promise(resolve=>setTimeout(resolve,wait));if(!current())return false;
      const data=await request('/api/router/firewall/port-forward');if(!current())return false;applyRules(data);if(check(ruleEntries))return true;
    }
    return false;
  }
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(!dirty || !healthy || !form.reportValidity() || UI.saving)return;const epoch=UI.epoch;
    try{
      const all=draft(),body=R.portBody(all,await readLAN(),ruleEntries);if(!UI.current(epoch))return;
      if(!await confirm('Add port-forward rule',`Forward incoming ${body.protocol.toUpperCase()} port ${body.wan_port} to ${body.lan_ip}:${body.lan_port}? The rule will be ${body.enabled==='1' ? 'enabled' : 'disabled'}.`,{label:'Add rule'}))return;
      await mutate(feedback,async current=>{
        const [latest,lan]=await Promise.all([request('/api/router/firewall/port-forward'),readLAN()]);if(!current())return;
        R.portBody(all,lan,R.rules(latest));say(feedback,'Adding rule…');healthy=false;updateActions();
        await request('/api/router/firewall/port-forward',json('POST',body));if(!current())return;
        const matched=await verifiedRules(entries=>entries.some(rule=>['name','protocol','wan_port','lan_ip','lan_port','enabled'].every(key=>String(rule[key])===String(body[key]))),current);if(!current())return;
        if(matched){resetDraft();say(feedback,'Rule added and confirmed by the router.');}else{healthy=false;say(feedback,'Request accepted, but the rule is not confirmed. Your draft is kept. Read current rules before retrying.',true);}
      });
    }catch(error){if(UI.current(epoch))say(feedback,error.message,true);}finally{updateActions();}
  });
  async function deleteRule(rule){
    if(!rawRules || !healthy || !rule.removable || UI.saving)return;
    const expected=JSON.parse(JSON.stringify(rawRules));
    if(!await confirm('Delete port-forward rule',`Remove ${rule.name || 'this rule'} (${rule.protocol || 'protocol unreported'} port ${rule.wan_port || 'unreported'})? Incoming connections using it will stop.`,{label:'Delete rule',destructive:true}))return;
    await mutate(feedback,async current=>{
      const latest=await request('/api/router/firewall/port-forward');if(!current())return;R.rules(latest);
      if(R.canonical(latest.rule_list)!==R.canonical(expected)){applyRules(latest);say(feedback,'The rule list changed. Review the current rule before deleting it.',true);return;}
      healthy=false;updateActions();say(feedback,'Removing rule…');
      await request('/api/router/firewall/port-forward',json('POST',{action:'delete',id:rule.id,expected_rules:expected}));if(!current())return;
      const matched=await verifiedRules(entries=>entries.length===expected.length-1 && R.canonical(entries.map(entry=>{const {id,...raw}=entry.raw;return raw;}))===R.canonical(expected.filter((_,index)=>ruleEntriesId(expected[index],index)!==rule.id).map(entry=>{const {id,...raw}=entry;return raw;})),current);if(!current())return;
      if(matched)say(feedback,'Rule removed and confirmed by the router.');else{healthy=false;say(feedback,'Removal is not confirmed. Read current rules before another change.',true);}
    });updateActions();
  }
  const ruleEntriesId=(rule,index)=>String(rule.id ?? index);
  UI.track({route:'network/ports',get loaded(){return attempted;},get dirty(){return dirty;},load:loadRules,clear(){rawRules=null;ruleEntries=[];attempted=false;loading=false;healthy=false;resetDraft();ruleList.replaceChildren();say(feedback,'Unlock the console to load rules.');updateActions();}});
  UI.makeForm({route:'network/qos',id:'qos-config',title:'Traffic prioritization',description:'Use the router’s built-in traffic policy. Individual device priorities and custom bandwidth limits are unavailable in this console.',endpoint:'/api/router/qos',read:setting('/api/router/qos',['qos_switch']),liveData:R.normalizeSwitches,source:'qos_config',guardKeys:['qos_switch'],fields:[onoff('qos_switch','Quality of service')],validate:(_,all)=>R.switchValue(all.qos_switch,'quality of service'),warning:'Changing traffic prioritization may affect active transfers. Apply the router’s QoS setting?'});
  UI.makeForm({route:'network/ttl',id:'ttl-config',title:'Outbound packet lifetime',description:'Rewrite outbound packets on the cellular software path. Qualcomm hardware acceleration may bypass these rules on established high-throughput connections.',endpoint:'/api/router/ttl',
    fields:[choices('enabled','TTL rewriting',[['true','On'],['false','Off']],{boolean:true}),field('ipv4_ttl','IPv4 TTL',{type:'number',min:1,max:255,help:'The value placed on IPv4 packets leaving the cellular interface.'}),field('ipv6_hop_limit','IPv6 hop limit',{type:'number',min:1,max:255,help:'The value placed on IPv6 packets leaving the cellular interface.'})],
    validate:(_,all)=>{for(const [key,label] of [['ipv4_ttl','IPv4 TTL'],['ipv6_hop_limit','IPv6 hop limit']]){const value=Number(all[key]);if(!Number.isInteger(value)||value<1||value>255)throw new Error(`${label} must be a whole number from 1 to 255.`);}},
    warning:(_,all)=>all.enabled==='true' ? `Enable outbound packet rewriting with IPv4 TTL ${all.ipv4_ttl} and IPv6 hop limit ${all.ipv6_hop_limit}? This changes packet headers only; it does not restart the firewall or disable hardware acceleration.` : 'Disable the agent’s packet TTL and hop-limit rules?'
  });
  const qosSummary=summary('network/qos','qos-overview','Router traffic policy','Live status of the firmware’s built-in policy.',[['state','Quality of service']]);
  function renderSummaries(){
    let lan=samples.get('lan_config')?.data;try{lan=R.normalizeLAN(lan);}catch{lan=null;}
    const clientData=samples.get('clients')?.data,firewall=samples.get('firewall_config')?.data,upnp=samples.get('upnp_config')?.data,qos=samples.get('qos_config')?.data;
    let pool='—';if(lan?.dhcp_enable==='0')pool=0;else if(lan)try{const start=R.host(lan.dhcp_start,lan),end=R.host(lan.dhcp_end,lan);if(end>=start)pool=end-start+1;}catch{/* Invalid ranges stay unreported. */}
    values(lanSummary,{address:lan?.lan_ipaddr,dhcp:state(lan?.dhcp_enable),pool,leases:Array.isArray(clientData?.dhcp_leases) ? clientData.dhcp_leases.length : '—'});stamp(lanSummary,['lan_config','clients']);
    values(firewallSummary,{firewall:state(firewall?.firewall_switch),nat:state(firewall?.nat_switch),upnp:state(upnp?.upnp_switch),dmz:firewall?.dmz_enabled===undefined ? 'Not reported' : enabled(firewall.dmz_enabled) ? firewall.dmz_ip || 'Address unreported' : 'Disabled'});stamp(firewallSummary,['firewall_config','upnp_config']);
    values(portsSummary,{state:state(firewall?.port_forward_switch),rules:rawRules ? ruleEntries.length : '—',enabled:rawRules ? ruleEntries.filter(rule=>enabled(rule.enabled)).length : '—',upnp:state(upnp?.upnp_switch)});stamp(portsSummary,['firewall_config','port_rules','upnp_config']);portCaption.textContent=portsSummary.caption.textContent;portCaption.dataset.stale=portsSummary.caption.dataset.stale;
    values(qosSummary,{state:state(qos?.qos_switch)});stamp(qosSummary,['qos_config']);
  }
  window.addEventListener('dashboard:readings',event=>{
    if(event.detail.snapshot)for(const name of ['lan_config','firewall_config','upnp_config','port_rules','qos_config'])if(samples.has(name))samples.get(name).unavailable=true;
    for(const name of ['lan_config','firewall_config','upnp_config','port_rules','qos_config','clients']){
      const reading=event.detail.readings[name];if(!reading)continue;
      const previous=samples.get(name);let usable=!!reading.ok && !!reading.data;
      if(usable)try{if(name==='lan_config')R.normalizeLAN(reading.data);if(name==='port_rules')R.rules(reading.data);if(name==='clients' && !Array.isArray(reading.data.dhcp_leases))usable=false;}catch{usable=false;}
      if(usable)samples.set(name,{data:reading.data,at:Date.now(),unavailable:false});else if(previous)previous.unavailable=true;
      if(name==='port_rules'){
        if(usable)try{if(!UI.saving)applyRules(reading.data);}catch{healthy=false;if(previous)previous.unavailable=true;}
        else healthy=false;
      }
    }
    renderSummaries();drawLeases();updateActions();
  });
  window.addEventListener('dashboard:mutation-complete',updateActions);
  window.addEventListener('dashboard:connection',()=>{renderSummaries();updateActions();});
  window.addEventListener('dashboard:locked',()=>{samples.clear();reconnectNote.hidden=true;reconnect.removeAttribute('href');reconnect.textContent='';drawLeases();renderSummaries();});
  renderSummaries();drawLeases();
})();
