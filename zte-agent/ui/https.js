'use strict';
(() => {
  const UI=OpenU60UI, API=OpenU60Console, {el,button}=UI, route='settings/https';
  let data=null, epoch=0;
  const panel=UI.panel(route,'https-settings','HTTPS & install','Secure local access with the router’s certificate authority. Each device trusts it once.');
  const facts=el('dl','settings-facts'), nodes={};
  for(const [key,label] of [['state','HTTPS'],['expires','Certificate valid until'],['clock','Router clock'],['authority','Certificate authority'],['scope','Can vouch for'],['fingerprint','SHA-256 fingerprint'],['sha1','SHA-1 fingerprint for Windows']]){
    const row=el('div'), value=el('dd',key==='fingerprint'||key==='sha1'?'mono https-fingerprint':'','—');value.id=`https-${key}`;row.append(el('dt','',label),value);facts.append(row);nodes[key]=value;
  }
  const addresses=el('div','form-actions'), actions=el('div','form-actions diagnostic-actions'), feedback=el('p','action-status'), caption=el('p','reading-caption','Waiting for HTTPS status.');feedback.setAttribute('role','status');
  async function change(kind){
    const current=epoch, snapshot=data;
    if(!snapshot||API.locked)return;
    const date=Number.isFinite(snapshot.clock_utc)?new Date(snapshot.clock_utc*1000).toLocaleString(): 'not reported';
    const details=kind==='reset'?'Remove the router’s HTTPS certificates and turn off its HTTPS listener. Remove the old authority from your devices afterward. If you are using HTTPS now, reopen the router’s HTTP address after this change.':`${kind==='on'?'Enable HTTPS using':'Renew the certificate from'} the router’s certificate authority. The router clock is ${date}${snapshot.clock_verified?' (verified).':' (not independently verified).'} Devices must trust the router’s authority before opening its HTTPS address.`;
    if(!await UI.confirm(kind==='reset'?'Reset HTTPS?':kind==='renew'?'Renew certificate?':'Turn on HTTPS?',details,{label:kind==='reset'?'Reset HTTPS':kind==='renew'?'Renew certificate':'Turn on HTTPS',destructive:kind==='reset',typed:kind==='reset'}))return;
    if(current!==epoch||API.locked||OpenU60Navigation.current.key!==route)return;
    await UI.mutate(feedback,async active=>{const result=await UI.request(`/api/https/${kind}`,UI.json('POST',{confirm:true}));if(active()&&current===epoch){draw(result);UI.say(feedback,kind==='reset'?'HTTPS was reset. Open the router’s HTTP address to continue.':kind==='renew'?'Certificate renewed.':'HTTPS enabled. Install the certificate on this device before opening an HTTPS address.');}});
  }
  const enable=button('Turn on HTTPS',()=>change('on'),'primary'), renew=button('Renew certificate',()=>change('renew')), reset=button('Reset HTTPS',()=>change('reset'),'danger');
  for(const node of [enable,renew,reset])node.disabled=true;
  actions.append(enable,renew,reset);panel.append(facts,addresses,actions,feedback,caption);
  const install=UI.panel(route,'https-install','Install on your devices','Install only this router’s public certificate, then open one of its HTTPS addresses. Private keys never leave the router.');
  const downloads=el('div','form-actions');
  for(const [label,href]of[['Profile for iPhone & iPad','/ui/https/u60-pro-ca.mobileconfig'],['Certificate for Android, Mac & Windows','/ui/https/u60-pro-ca.crt']]){const link=el('a','button',label);link.href=href;link.setAttribute('download','');downloads.append(link);}downloads.hidden=true;
  const trust=el('p','panel-note','Before trusting the certificate, compare the SHA-256 fingerprint above with the one shown on your device. For a trusted comparison, use this page over HTTPS on a device that already trusts the router, or compare with sha256sum /data/zte-agent-tls/ca.crt over SSH. Plain HTTP can be altered by someone on the network.');
  const steps=el('dl','settings-facts');
  for(const [name,text]of[['iPhone & iPad','Download the profile. Open Settings → Profile Downloaded → Install. Then Settings → General → About → Certificate Trust Settings: enable full trust for the router’s authority. In Safari, use Share → Add to Home Screen.'],['Android','Download the certificate. In Settings, search for Install a CA certificate, confirm with your screen lock and select the file. Open an HTTPS address above, then use your browser’s Install or Add to Home screen option.'],['Mac','Download and open the certificate in Keychain Access. Open its Trust section and select Always Trust. In Safari, open an HTTPS address above, then File → Add to Dock.'],['Windows','Open the certificate → Install Certificate → Current User → Place all certificates in Trusted Root Certification Authorities. Compare Windows’ SHA-1 Thumbprint with the value above. Open an HTTPS address in Chrome or Edge and choose Install.']]){const row=el('div');row.append(el('dt','',name),el('dd','',text));steps.append(row);}
  install.append(downloads,trust,steps,el('p','panel-note','Devices using private DNS, a VPN or browser DNS-over-HTTPS may not resolve the router’s local name; use its listed LAN address. Each origin keeps its own console sign-in.'));
  function draw(value){
    data=value;nodes.state.textContent=value.enabled?(value.listening?'On':'On · not listening'):'Off';
    nodes.expires.textContent=value.not_after?new Date(value.not_after*1000).toLocaleDateString()+(value.renew_due?' · renewal due':''):'Not reported';
    nodes.clock.textContent=Number.isFinite(value.clock_utc)?new Date(value.clock_utc*1000).toLocaleString()+(value.clock_verified?' · verified':' · not independently verified'):'Not reported';
    nodes.authority.textContent=value.ca_name||value.kept_ca||'Not created';nodes.scope.textContent=(value.ca_covers||[]).join(', ')||'Not reported';nodes.fingerprint.textContent=value.ca_sha256||'Not reported';nodes.sha1.textContent=value.ca_sha1||'Not reported';
    addresses.replaceChildren();for(const url of value.urls||[]){try{const parsed=new URL(url);if(parsed.protocol!=='https:'||parsed.username||parsed.password)continue;const link=el('a','button',url);link.href=parsed.href;link.rel='noreferrer';addresses.append(link);}catch{}}
    enable.hidden=!!value.enabled&&!!value.listening;enable.textContent=value.enabled?'Try HTTPS again':'Turn on HTTPS';renew.hidden=!value.enabled;for(const node of [enable,renew,reset])node.disabled=false;downloads.hidden=!value.enabled;
    caption.textContent=(API.connected?'Live HTTPS status':'Last received HTTPS status')+(value.error?` · ${value.error}`:'');caption.dataset.stale=String(!API.connected);
  }
  function clear(){epoch++;data=null;for(const value of Object.values(nodes))value.textContent='—';addresses.replaceChildren();downloads.hidden=true;for(const node of[enable,renew,reset])node.disabled=true;feedback.textContent='';caption.textContent='Waiting for HTTPS status.';}
  window.addEventListener('dashboard:readings',event=>{if(OpenU60Navigation.current.key!==route)return;const reading=event.detail.readings.https_status;if(reading?.ok)draw(reading.data);else if(reading){caption.textContent='HTTPS status unavailable.';caption.dataset.stale='true';}});
  window.addEventListener('dashboard:locked',clear);window.addEventListener('dashboard:navigate',()=>{if(OpenU60Navigation.current.key!==route)clear();});
  window.addEventListener('dashboard:connection',()=>{if(data){caption.dataset.stale=String(!API.connected);if(!API.connected)caption.textContent='Last received HTTPS status · connection interrupted.';}});
})();
