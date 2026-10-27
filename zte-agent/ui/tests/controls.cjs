// Real-browser configuration tests. All writes terminate at this local fake router.
const {chromium} = require('playwright');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
(async()=>{
  const requests = [], streams = new Set(); let failNext = '/api/wifi/guest', mismatch = false, dropNext = '', rejectAuth = false;
  const wifi = {wifi_onoff:'1',wifi6_switch:'1',radio2_disabled:'0',radio5_disabled:'0',ssid_2g:'Home',ssid_5g:'Home 5G',key_2g:'existingpass',key_5g:'existingpass',encryption_2g:'psk2+ccmp',encryption_5g:'psk2+ccmp',hidden_2g:'0',hidden_5g:'0',channel_2g:'auto',channel_5g:'149',htmode_2g:'EHT20',htmode_5g:'EHT80',txpower_2g:'100',txpower_5g:'100'};
  const guest = {ssid:'Visitors',key:'guestpassword',encryption:'psk2+ccmp',disabled_2g:'1',disabled_5g:'1',hidden:'0',isolate:'1',guest_active_time:'0'};
  const mobile = {enable:1,roam_enable:0,connect_mode:1,connect_status:'ipv4_ipv6_connected'};
  const modem = {operate_mode:'ONLINE'}, signal = {net_select:'WL_AND_5G',network_type:'LTE-NSA'};
  const charging = {charge_limit_enabled:false,charge_limit:80,hysteresis:5,charging_stopped:false};
  const power = {power_saver_mode:'0'}, fast = {fast_boot:'0'}, sleep = {enabled:false,timeout:'10'};
  const apnMode = {apn_mode:0}; const profiles = [{profileId:'1',profilename:'Carrier',wanapn:'internet',pdpType:3,pppAuthMode:0,username:'',password:'private',isEnable:'1'}];
  let messages = [{id:'2',number:'+639170001234',content:'00480065',tag:'1'},{id:'1',number:'Carrier',content:'00480069',tag:'0'}];
  const readings = () => ({wifi:{ok:true,data:Object.fromEntries(Object.entries(wifi).filter(([key])=>!key.startsWith('key_')))},signal:{ok:true,data:signal},mobile:{ok:true,data:mobile},modem:{ok:true,data:modem},charging:{ok:true,data:charging},sms:{ok:true,data:{messages}}});
  function push() { for (const stream of streams) stream.write(`event: patch\ndata: ${JSON.stringify({readings:readings()})}\n\n`); }
  const send = (res,data,status=200) => { res.writeHead(status,{'Content-Type':'application/json'}); res.end(JSON.stringify(status < 300 ? {ok:true,data} : {ok:false,error:data})); };
  const server = http.createServer((req,res)=>{
    if (!req.url.startsWith('/api/')) {
      const file = req.url === '/' ? 'index.html' : /^\/(?:ui|console\/a)\/[\w.-]+$/.test(req.url) ? req.url.split('/').pop() : '';
      const full = path.join(__dirname,'..',file); if (!file || !fs.existsSync(full)) {res.writeHead(404);res.end();return;}
      res.setHeader('Content-Type',file.endsWith('.css') ? 'text/css' : file.endsWith('.js') ? 'text/javascript' : 'text/html');
      res.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'"); res.end(fs.readFileSync(full)); return;
    }
    if (rejectAuth) {send(res,'unauthorized',401);return;}
    if (req.url.startsWith('/api/dashboard/events')) { res.writeHead(200,{'Content-Type':'text/event-stream'}); streams.add(res);res.on('close',()=>streams.delete(res));res.write(`event: snapshot\ndata: ${JSON.stringify({readings:readings()})}\n\n`);return; }
    let raw = '';req.on('data',chunk=>raw+=chunk);req.on('end',()=>{
      const body = raw ? JSON.parse(raw) : null; requests.push({url:req.url,method:req.method,body});
      if (failNext === req.url) {failNext='';send(res,'fixture rejected the request',503);return;}
      if (dropNext === req.url) {dropNext='';res.writeHead(200,{'Content-Type':'application/json'});res.flushHeaders();return;}
      const read = (endpoint,data) => {if (req.url !== endpoint || req.method !== 'GET') return false;send(res,data);return true;};
      if (read('/api/wifi/status',wifi) || read('/api/wifi/guest',guest) || read('/api/modem/data',mobile) || read('/api/modem/status',modem) || read('/api/network/signal',signal) || read('/api/device/charge-control',charging) || read('/api/device/fast-boot',fast) || read('/api/device/auto-sleep',sleep) || read('/api/router/apn/mode',apnMode) || read('/api/router/apn/profiles',{apnListArray:profiles})) return;
      if (req.url === '/api/device/power-save' && req.method === 'POST') {send(res,power);return;}
      if (req.url === '/api/sms/list') {send(res,{messages});return;}
      if (req.url === '/api/sms/read') for (const id of body.id.split(';').filter(Boolean)) {const message=messages.find(item=>item.id===id); if(message) message.tag=String(body.tag);}
      else if (req.url === '/api/sms/delete') messages=messages.filter(item=>!body.id.split(';').includes(item.id));
      if (!mismatch) {
        if (req.url === '/api/wifi/settings') Object.assign(wifi,body);
        if (req.url === '/api/wifi/guest') for (const [key,val] of Object.entries(body)) guest[key === 'guest_active_time' ? key : key.slice(6)]=val;
        if (req.url === '/api/modem/data') Object.assign(mobile,body);
        if (req.url === '/api/modem/airplane') Object.assign(modem,body);
        if (req.url === '/api/modem/network-mode') Object.assign(signal,body);
        if (req.url === '/api/device/charge-control') Object.assign(charging,body);
        if (req.url === '/api/device/power-save') Object.assign(power,body.deviceInfoList);
        if (req.url === '/api/device/fast-boot') Object.assign(fast,body);
        if (req.url === '/api/device/auto-sleep') Object.assign(sleep,body);
        if (req.url === '/api/router/apn/mode') Object.assign(apnMode,body);
        if (req.url === '/api/router/apn/profiles') { if(req.method === 'POST') profiles.push({...body,profileId:'2',isEnable:'0'}); else Object.assign(profiles.find(item=>item.profileId===body.profileId),body); }
        if (req.url === '/api/router/apn/profiles/activate') for (const profile of profiles) profile.isEnable=profile.profileId===body.profileId ? '1' : '0';
        if (req.url === '/api/router/apn/profiles/delete') {const index=profiles.findIndex(item=>item.profileId===body.profileId);if(index>=0)profiles.splice(index,1);}
      }
      send(res,{result:req.url === '/api/sms/send' ? 3 : 0});
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const heart = setInterval(()=>{for(const stream of streams)stream.write(': heartbeat\n\n');},1000);
  const browser = await chromium.launch({headless:true,channel:'chrome'}), context = await browser.newContext({viewport:{width:1440,height:1100}}), page = await context.newPage();
  const errors=[];page.on('pageerror',error=>errors.push(error.message));
  const go = async key=>{await page.evaluate(key=>OpenU60Navigation.go(key),key);await page.waitForFunction(key=>document.documentElement.dataset.route === key,key);};
  const feedback = id=>page.locator(`#${id} .action-status`);
  const saved = id=>page.waitForFunction(id=>document.querySelector(`#${id} .action-status`).textContent.includes('Saved and confirmed'),id);
  const save = async(id,confirm=true)=>{await page.locator(`#${id} button[type=submit]`).click();if(confirm)await page.locator('.confirm-dialog button').filter({hasText:/^(Save changes|Apply change)$/}).click();};
  const count = url=>requests.filter(item=>item.url===url).length;
  const last = url=>requests.filter(item=>item.url===url && item.body !== null).at(-1);
  await page.goto(`http://127.0.0.1:${server.address().port}/#network/wifi`);
  await page.waitForFunction(()=>document.querySelector('#stream-state').textContent==='Live');
  await page.locator('#wifi-config-ssid_2g').waitFor({state:'visible'});
  await page.waitForFunction(()=>!document.querySelector('#wifi-config fieldset').disabled);
  assert.equal(await page.locator('#wifi-config-key_2g').inputValue(),'','stored passwords stay out of input fields');
  assert.equal(count('/api/wifi/status'),1);
  await page.waitForFunction(()=>document.querySelector('#guest-config .action-status').textContent.includes('unavailable'));
  assert.equal(await page.locator('#guest-config fieldset').evaluate(fieldset=>fieldset.disabled),true,'failed reads keep controls disabled');
  await page.locator('#guest-config').getByRole('button',{name:'Read current settings'}).click();await page.waitForFunction(()=>!document.querySelector('#guest-config fieldset').disabled);
  await page.locator('#wifi-config-ssid_2g').fill('New home'); wifi.ssid_2g='Changed elsewhere';push();
  await page.waitForFunction(()=>document.querySelector('#wifi-config .action-status').textContent.includes('draft is kept'));
  assert.equal(await page.locator('#wifi-config-ssid_2g').inputValue(),'New home');
  await save('wifi-config',false);await page.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(count('/api/wifi/settings'),0);
  await save('wifi-config');await saved('wifi-config');assert.deepEqual(last('/api/wifi/settings').body,{ssid_2g:'New home'});
  assert.equal(await page.locator('#wifi-config-key_2g').inputValue(),'');
  await page.locator('#wifi-config-key_2g').fill('bad$password');await save('wifi-config',false);assert.match(await feedback('wifi-config').textContent(),/cannot store/);assert.equal(count('/api/wifi/settings'),1);
  await page.locator('#wifi-config button').filter({hasText:'Discard edits'}).click();
  await page.locator('#wifi-config-txpower_2g').fill('');await save('wifi-config',false);assert.match(await feedback('wifi-config').textContent(),/whole number/);await page.locator('#wifi-config button').filter({hasText:'Discard edits'}).click();
  failNext='/api/wifi/settings';await page.locator('#wifi-config-ssid_2g').fill('Rejected change');await save('wifi-config');await page.waitForFunction(()=>document.querySelector('#wifi-config .action-status').textContent.includes('fixture rejected'));assert.equal(await page.locator('#wifi-config-ssid_2g').inputValue(),'Rejected change');await page.locator('#wifi-config button').filter({hasText:'Discard edits'}).click();
  mismatch=true;await page.locator('#wifi-config-ssid_2g').fill('Unconfirmed');await save('wifi-config');
  await page.waitForFunction(()=>document.querySelector('#wifi-config .action-status').textContent.includes('not confirmed'));
  assert.equal(await page.locator('#wifi-config-ssid_2g').inputValue(),'Unconfirmed');mismatch=false;await page.locator('#wifi-config button').filter({hasText:'Discard edits'}).click();
  await page.locator('#guest-config-guest_disabled_2g').selectOption('0');await page.locator('#guest-config-guest_key').fill('newguestpass');await page.locator('#guest-config-guest_active_time').selectOption('60');await save('guest-config');await saved('guest-config');assert.deepEqual(last('/api/wifi/guest').body,{guest_disabled_2g:'0',guest_key:'newguestpass',guest_active_time:'60'});
  // Existing forms are never polled by SSE or route changes.
  const priorReads=count('/api/wifi/status');push();await go('network/clients');await go('network/wifi');assert.equal(count('/api/wifi/status'),priorReads);
  wifi.ssid_2g='Live clean update';push();await page.waitForFunction(()=>document.querySelector('#wifi-config-ssid_2g').value==='Live clean update');
  await go('network/mobile');await page.waitForFunction(()=>!document.querySelector('#mobile-config fieldset').disabled);
  await page.locator('#mobile-config-enable').selectOption('0');await save('mobile-config');await saved('mobile-config');assert.deepEqual(last('/api/modem/data').body,{cid:1,enable:0,roam_enable:0,connect_mode:1,connect_status:'disconnected'});
  await page.locator('#radio-config-operate_mode').selectOption('LPM');await save('radio-config');await saved('radio-config');assert.equal(last('/api/modem/airplane').body.operate_mode,'LPM');
  await page.locator('#network-mode-config-net_select').selectOption('Only_5G');await save('network-mode-config');await saved('network-mode-config');assert.deepEqual(last('/api/modem/network-mode').body,{net_select:'Only_5G'});
  await go('network/apn');await page.waitForFunction(()=>!document.querySelector('#apn-profiles fieldset').disabled);
  await page.locator('#apn-profile').selectOption('1');assert.equal(await page.locator('#apn-password').inputValue(),'');assert.equal(await page.getByRole('button',{name:'Delete profile',exact:true}).isDisabled(),true);
  await page.locator('#apn-profilename').fill('Carrier edited');await page.getByRole('button',{name:'Save profile',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Save profile',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#apn-profiles .action-status').textContent.includes('saved and confirmed'));
  assert.equal(requests.find(item=>item.url==='/api/router/apn/profiles'&&item.method==='PUT').body.password,'private');
  await page.locator('#apn-profile').selectOption('new');await page.locator('#apn-profilename').fill('Backup');await page.locator('#apn-wanapn').fill('backup.internet');await page.getByRole('button',{name:'Save profile',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Save profile',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('#apn-profile').value==='2');
  await page.getByRole('button',{name:'Activate profile',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Activate profile',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#apn-profiles .action-status').textContent.includes('Active profile confirmed'));
  await page.locator('#apn-profile').selectOption('1');await page.getByRole('button',{name:'Delete profile',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Delete profile',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#apn-profiles .action-status').textContent.includes('deletion confirmed'));
  await go('network/bands');await page.locator('#lte-band-bands').fill('b7, 3,3');await page.locator('#lte-band button[type=submit]').click();await page.locator('.confirm-dialog').getByRole('button',{name:'Apply LTE bands',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#lte-band .action-status').textContent.includes('accepted'));assert.deepEqual(last('/api/cell/band/lte').body,{is_lte_band:'1',lte_band_mask:'3,7',is_gw_band:'0',gw_band_mask:''});
  await page.locator('#nr-band-bands').fill('n78');await page.locator('#nr-band-mode').selectOption('sa');await page.locator('#nr-band button[type=submit]').click();await page.locator('.confirm-dialog').getByRole('button',{name:'Apply NR bands',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#nr-band .action-status').textContent.includes('accepted'));assert.deepEqual(last('/api/cell/band/nr').body,{nr5g_type:'sa',nr5g_band:'78'});
  await go('sms/inbox');await page.waitForFunction(()=>document.querySelectorAll('.message-card').length===2);
  const first=page.locator('.message-card').first();await first.getByRole('button',{name:'Mark read',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Apply',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#sms-action-status').textContent.includes('confirmed'));assert.deepEqual(last('/api/sms/read').body,{id:'2;',tag:0});
  await first.getByRole('button',{name:'Reply',exact:true}).click();await page.waitForFunction(()=>document.documentElement.dataset.route==='sms/compose');assert.equal(await page.locator('#sms-compose-number').inputValue(),'+639170001234');await page.locator('#sms-compose-message').fill('Hi 😀');
  await page.locator('#sms-compose button[type=submit]').click();assert.match(await page.locator('.confirm-copy').textContent(),/Hi 😀/);await page.locator('.confirm-dialog').getByRole('button',{name:'Send message',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#sms-compose .action-status').textContent.includes('Delivery is not confirmed'));assert.equal(last('/api/sms/send').body.message_body,'004800690020D83DDE00');assert.equal(last('/api/sms/send').body.encode_type,'UNICODE');assert.equal(await page.locator('#sms-compose-message').inputValue(),'');
  // An uncertain send is retained and never retried automatically.
  dropNext='/api/sms/send';await page.locator('#sms-compose-number').fill('+1234');await page.locator('#sms-compose-message').fill('Keep draft');await page.locator('#sms-compose button[type=submit]').click();await page.locator('.confirm-dialog').getByRole('button',{name:'Send message',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#sms-compose .action-status').textContent.includes('could not be confirmed'));assert.equal(await page.locator('#sms-compose-message').inputValue(),'Keep draft');assert.equal(count('/api/sms/send'),2);
  await go('sms/inbox');await page.getByRole('button',{name:'Select visible',exact:true}).click();push();await page.waitForFunction(()=>document.querySelectorAll('.message-select input:checked').length===2);
  await page.locator('.message-toolbar').getByRole('button',{name:'Delete',exact:true}).click();await page.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(count('/api/sms/delete'),0);
  await page.locator('.message-toolbar').getByRole('button',{name:'Delete',exact:true}).click();await page.locator('.confirm-dialog').getByRole('button',{name:'Delete messages',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#sms-action-status').textContent.includes('deletion confirmed'));assert.equal(last('/api/sms/delete').body.id,'2;1;');assert.equal(await page.locator('.message-card').count(),0);
  await go('settings/device');await page.waitForFunction(()=>!document.querySelector('#charge-config fieldset').disabled);
  await page.locator('#charge-config-charge_limit_enabled').selectOption('true');await page.locator('#charge-config-charge_limit').fill('85');await save('charge-config',false);await saved('charge-config');assert.deepEqual(last('/api/device/charge-control').body,{charge_limit_enabled:true,charge_limit:85});
  await page.locator('#manual-charge-charging_stopped').selectOption('true');await save('manual-charge');await saved('manual-charge');assert.deepEqual(last('/api/device/charge-control').body,{charging_stopped:true});
  await page.locator('#power-save-power_saver_mode').selectOption('1');await save('power-save',false);await saved('power-save');assert.deepEqual(requests.find(item=>item.url==='/api/device/power-save'&&item.method==='PUT').body,{deviceInfoList:{power_saver_mode:'1'}});
  await page.locator('#fast-boot-fast_boot').selectOption('1');await save('fast-boot',false);await saved('fast-boot');
  await page.locator('#auto-sleep-enabled').selectOption('true');await page.locator('#auto-sleep-timeout').fill('20');await save('auto-sleep');await saved('auto-sleep');assert.deepEqual(requests.find(item=>item.url==='/api/device/auto-sleep'&&item.method==='PUT').body,{enabled:true,timeout:'20'});
  await page.getByRole('button',{name:'Restart router',exact:true}).click();await page.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(count('/api/device/reboot'),0);
  await page.getByRole('button',{name:'Erase and reset',exact:true}).click();assert.equal(await page.locator('.confirm-dialog').getByRole('button',{name:'Erase and reset'}).isDisabled(),true);await page.locator('#confirm-reset').fill('reset');assert.equal(await page.locator('.confirm-dialog').getByRole('button',{name:'Erase and reset'}).isDisabled(),true);await page.locator('#confirm-reset').fill('RESET');await page.locator('.confirm-dialog').getByRole('button',{name:'Erase and reset'}).click();await page.waitForFunction(()=>document.querySelector('#device-reset .action-status').textContent.includes('requested'));assert.equal(count('/api/device/factory-reset'),1);
  // Every new route remains usable across the existing phone/tablet/desktop themes.
  for(const width of [320,390,768,820,1440]) for(const theme of ['light','dark']) {
    await page.setViewportSize({width,height:1000});await go('settings/appearance');await page.locator(`input[name=appearance][value=${theme}]`).check();
    for(const key of ['network/wifi','network/mobile','network/apn','network/bands','sms/compose','sms/inbox','settings/device']) {await go(key);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`${key} overflow at ${width} ${theme}`);}
    if([390,820,1440].includes(width)) {await go('network/wifi');await page.evaluate(()=>scrollTo({top:0,behavior:'instant'}));await page.screenshot({path:`/tmp/zte-controls-${theme}-${width}.png`});}
  }
  await go('network/wifi');await page.locator('#wifi-config-key_2g').fill('unsavedpassword');rejectAuth=true;await page.locator('#wifi-config-ssid_2g').fill('Auth expired');await save('wifi-config');await page.waitForFunction(()=>document.querySelector('#login-dialog').open);assert.equal(await page.locator('#wifi-config-key_2g').inputValue(),'');assert.equal(await page.locator('#sms-compose-message').inputValue(),'');assert.equal(await page.locator('#wifi-config fieldset').evaluate(fieldset=>fieldset.disabled),true);
  assert.deepEqual(errors,[]);
  await browser.close();clearInterval(heart);for(const stream of streams)stream.destroy();server.close();
  console.log('Passed: Wi-Fi/guest changed-field writes and password preservation, dirty and clean SSE updates, save readbacks and mismatch states, mobile/SA/NSA/airplane contracts, APN create/edit/activate/delete, LTE/NR restrictions, SMS Unicode send/reply/read/bulk delete/cancel and uncertain-send retention, charging/power/sleep controls, typed factory-reset confirmation, auth-expiry draft clearing, and seven routes in light/dark at five widths. All commands used a local fixture.');
})().catch(error=>{console.error(error);process.exit(1);});
