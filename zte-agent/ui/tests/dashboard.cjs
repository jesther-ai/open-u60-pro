// Run with Playwright available in NODE_PATH; uses a real streaming HTTP fixture.
const { chromium } = require('playwright');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
(async () => {
  let mode = 'live', requiresAuth = false, delayedResponse;
  const streams = new Set(), requests = [];
  const fixtures = {
    device: {hostname:'OpenU60',uptime_secs:183960,kernel:'5.4.260',load_avg:[0.12,0.18,0.15]},
    wan: {up:true,'ipv4-address':[{address:'10.154.28.6'}]},
    wan6: {up:true,'ipv6-address':[{address:'2001:db8:4870:7d0c:1cc7:b83e:60e4:a9c8'}]},
    signal: {network_provider:'Example Mobile',network_type:'LTE-NSA',nr5g_rsrp:'-84',nr5g_snr:'22.4',nr5g_rsrq:'-10',nr5g_rssi:'-59',nr5g_action_band:'78',nr5g_pci:'231',nr5g_action_channel:'633984',nr5g_cell_id:'0x3456701',
      nrca:'231,78,0,633984,100;231,78,1,641280,100;',nrcasig:'-91,-11,17,-64;',
      lte_rsrp:'-90',lte_snr:'18',lte_rsrq:'-11',lte_rssi:'-62',lte_pci:'265',wan_active_band:'3',wan_active_channel:'1775',cell_id:'0x12AB01',
      lteca:'265,3,0,1775,15;265,1,1,150,20;19,7,2,2850,20;',ltecasig:'-94,-12,16,-65;-99,-14,10,-70;'},
    modem: {operate_mode:'ONLINE'},
    sms: {messages:[{id:2,number:'+639170001234',content:'00480065006C006C006F0020005500360030',date:'26,10,25,12,30,00,+32',tag:'1',mem_store:'nv'},{id:1,number:'Carrier',content:'003C0069006D00670020007300720063003D0078003E',date:'26,10,25,11,00,00,+32',tag:'0',mem_store:'sim'}]},
    sms_capacity: {sms_nv_total:500,sms_nvused_total:2,sms_sim_total:30,sms_simused_total:1,sms_dev_unread_num:1},
    speed: {rx_speed:15400000,tx_speed:1640000,rx_bytes:8400000000,tx_bytes:1200000000},
    battery: {capacity:86,status:'Charging',temperature:327},
    wifi: {wifi_onoff:'1',radio2_disabled:'0',radio5_disabled:'0',ssid_2g:'OpenU60',ssid_5g:'OpenU60 · 5G',channel_2g:'6',channel_5g:'149'},
    clients: {hosts:{'aa:bb:cc:dd:ee:01':{name:'MacBook Pro',ipaddrs:['192.168.0.102']},'aa:bb:cc:dd:ee:02':{name:'<img src=x onerror=alert(1)>',ipaddrs:['192.168.0.104']}},dhcp_leases:[{hostname:'Living room TV',ipaddr:'192.168.0.108',macaddr:'aa:bb:cc:dd:ee:03'}]}
  };
  const envelope = names => Object.fromEntries(names.map(name => [name,{ok:true,data:fixtures[name]}]));
  const send = (response,event,readings) => response.write(`event: ${event}\r\ndata: ${JSON.stringify({readings})}\r\n\r\n`);
  const push = names => { for (const response of streams) send(response,'patch',envelope(names.filter(name => !name.startsWith('sms') || response.includeSms))); };
  const disconnect = () => { for (const response of streams) response.destroy(); };
  const server = http.createServer((request, response) => {
    if (request.url.startsWith('/api/')) requests.push({url:request.url,authorization:request.headers.authorization});
    if (request.url.split('?')[0] === '/api/dashboard/events') {
      if (requiresAuth && request.headers.authorization !== 'Bearer test-token') {
        response.writeHead(401,{'Content-Type':'application/json'}); response.end('{"ok":false}'); return;
      }
      if (mode === 'offline') { response.destroy(); return; }
      response.writeHead(200,{'Content-Type':'text/event-stream','Cache-Control':'no-store'});
      response.flushHeaders();
      response.includeSms = request.url.includes('sms=1');
      streams.add(response); response.on('close', () => streams.delete(response));
      if (mode === 'delayed') { delayedResponse = response; return; }
      send(response,'snapshot',envelope(Object.keys(fixtures).filter(name => !name.startsWith('sms') || response.includeSms)));
      return;
    }
    if (request.url === '/api/auth/login') {
      let body = ''; request.on('data',chunk => body += chunk);
      request.on('end', () => {
        const valid = JSON.parse(body).password === 'test-password';
        response.writeHead(valid?200:401,{'Content-Type':'application/json'});
        response.end(JSON.stringify(valid?{ok:true,data:{token:'test-token'}}:{ok:false}));
      }); return;
    }
    if (request.url === '/api/wifi/status' || request.url === '/api/wifi/guest') { response.setHeader('Content-Type','application/json'); response.end(JSON.stringify({ok:true,data:request.url.endsWith('status') ? fixtures.wifi : {ssid:'Guest',encryption:'psk2+ccmp',disabled_2g:'1',disabled_5g:'1',hidden:'0',isolate:'1'}})); return; }
    const file = {'/':'index.html','/ui/dashboard.css':'dashboard.css','/ui/routing-model.js':'routing-model.js','/ui/routing.js':'routing.js','/ui/clients-model.js':'clients-model.js','/ui/clients.js':'clients.js','/ui/dashboard.js':'dashboard.js','/ui/cellular.js':'cellular.js','/ui/esim-model.js':'esim-model.js','/ui/esim.js':'esim.js','/ui/addresses.js':'addresses.js','/ui/numbers.js':'numbers.js','/ui/motion.js':'motion.js','/ui/navigation.js':'navigation.js','/ui/appearance.js':'appearance.js','/ui/actions.js':'actions.js','/ui/controls.js':'controls.js','/ui/advanced.js':'advanced.js','/ui/operators.js':'operators.js','/ui/forwarding.js':'forwarding.js','/ui/diagnostics.js':'diagnostics.js','/ui/https.js':'https.js','/ui/battery-model.js':'battery-model.js','/ui/battery.js':'battery.js'}[request.url.split('#')[0].replace('/console/a/','/ui/')];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.setHeader('Content-Type',file.endsWith('.css')?'text/css':file.endsWith('.js')?'text/javascript':'text/html');
    response.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'");
    response.end(fs.readFileSync(path.join(__dirname,'..',file)));
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  let fragmentedFrame = false;
  const heartbeat = setInterval(() => { if (!fragmentedFrame) for (const response of streams) response.write(': heartbeat\n\n'); },1000);
  const browser = await chromium.launch({headless:true,channel:process.env.PLAYWRIGHT_CHANNEL || 'chrome'});
  const context = await browser.newContext({viewport:{width:1440,height:1100}});
  const page = await context.newPage();
  const errors = []; page.on('pageerror',error => errors.push(error.message));
  await page.goto(process.env.UI_BASE_URL || `http://127.0.0.1:${server.address().port}`);
  const navigate = async (group,child) => {
    const summary = page.locator(`.nav-group[data-group="${group}"] > summary`);
    if (await page.evaluate(() => innerWidth < 768)) {
      await summary.click();
      await page.locator(`#section-tabs a[href="#${group}/${child}"]`).click();
    } else {
      const open = await page.locator(`.nav-group[data-group="${group}"]`).getAttribute('open');
      if (open === null) await summary.click();
      await page.locator(`.nav-children a[href="#${group}/${child}"]`).click();
    }
    await page.waitForFunction(key => document.documentElement.dataset.route === key,`${group}/${child}`);
  };
  const live = () => page.waitForFunction(() => document.querySelector('#stream-state').textContent === 'Live');
  await live();
  assert.equal(await page.locator('#operator').textContent(),'Example Mobile');
  assert.equal(await page.locator('#network-type').textContent(),'5G NSA');
  const connectionType = await page.locator('#network-type').evaluate(node=>parseFloat(getComputedStyle(node).fontSize));
  const connectionStatus = await page.locator('#connection-title').evaluate(node=>parseFloat(getComputedStyle(node).fontSize));
  const operatorSize = await page.locator('#operator').evaluate(node=>parseFloat(getComputedStyle(node).fontSize));
  assert.ok(operatorSize > connectionType && connectionType > connectionStatus,'operator, generation and status must have a clear descending type hierarchy');
  assert.equal(await page.locator('#download').textContent(),'123.20');
  assert.equal(await page.locator('#device-count').textContent(),'3');
  assert.equal(await page.locator('#wan-ip').textContent(),'10.154.28.6');
  assert.equal(await page.locator('#wan-ipv6').textContent(),'2001:db8:4870:7d0c:1cc7:b83e:60e4:a9c8');
  fixtures.wan6['ipv6-address'].push({address:'2001:db8::2'});push(['wan6']);
  await page.waitForFunction(()=>document.querySelector('#wan-ipv6').textContent.includes('2001:db8::2'));
  assert.equal(await page.locator('#wan-ip').textContent(),'10.154.28.6','an IPv6-only patch preserves IPv4');
  for(const response of streams)send(response,'patch',{wan6:{ok:false,error:'unavailable'}});
  await page.waitForFunction(()=>document.querySelector('#wan-ipv6-status').textContent.includes('Last reported'));
  assert.match(await page.locator('#wan-ipv6').textContent(),/2001:db8::2/);
  fixtures.wan6['ipv6-address']=[];push(['wan6']);
  await page.waitForFunction(()=>document.querySelector('#wan-ipv6').textContent==='Not reported');
  fixtures.wan6['ipv6-address']=[{address:'2001:db8:4870:7d0c:1cc7:b83e:60e4:a9c8'}];push(['wan6']);
  assert.equal(await page.locator('#client-list img').count(),0,'host names remain text');
  assert.equal(await page.locator('#refresh').count(),0,'the dashboard requires no refresh control');
  // A patch changes only its dependent panel and leaves typed search text alone.
  await page.locator('#device-search').fill('MacBook');
  assert.equal(await page.locator('#client-list tr').count(),1);
  fixtures.battery.capacity = 73; push(['battery']);
  await page.waitForFunction(() => document.querySelector('#battery').textContent === '73');
  assert.equal(await page.locator('#download').textContent(),'123.20');
  assert.equal(await page.locator('#device-search').inputValue(),'MacBook');
  await page.locator('#device-search').fill('nothing matches');
  assert.match(await page.locator('#client-list').textContent(),/No devices match/);
  await page.locator('#device-search').fill('');
  fixtures.clients.hosts['aa:bb:cc:dd:ee:02'].name = 'iPhone'; push(['clients']);
  await page.waitForFunction(() => document.querySelector('#client-list').textContent.includes('iPhone'));
  await page.locator('#device-search').blur();
  assert.equal(await page.locator('html').getAttribute('data-appearance'),'system');
  await page.emulateMedia({colorScheme:'dark'});
  await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
  await navigate('settings','appearance');
  const streamCount = () => requests.filter(request => request.url.startsWith('/api/dashboard/events')).length;
  const beforeTheme = streamCount();
  await page.locator('input[name="appearance"][value="light"]').check();
  assert.equal(await page.locator('html').getAttribute('data-theme'),'light');
  assert.equal(await page.evaluate(() => localStorage.getItem('openu60.appearance')),'light');
  await page.emulateMedia({colorScheme:'light'}); await page.emulateMedia({colorScheme:'dark'});
  assert.equal(await page.locator('html').getAttribute('data-theme'),'light','manual light overrides the system');
  await page.locator('input[name="appearance"][value="dark"]').check();
  assert.equal(await page.locator('html').getAttribute('data-theme'),'dark');
  assert.equal(streamCount(),beforeTheme,'theme changes must not restart SSE');
  await page.reload(); await live();
  assert.equal(await page.locator('html').getAttribute('data-theme'),'dark','saved appearance survives reload');
  assert.equal(await page.locator('input[value="dark"]').isChecked(),true);
  assert.equal(await page.locator('meta[name="theme-color"]').getAttribute('content'),'#111821');
  await page.locator('input[name="appearance"][value="system"]').check();
  await page.emulateMedia({colorScheme:'light'});
  await page.waitForFunction(() => document.documentElement.dataset.theme === 'light');
  await page.emulateMedia({colorScheme:'dark'});
  await page.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
  await page.locator('input[name="appearance"][value="light"]').check();
  for (const width of [390,820,1440]) {
    await page.setViewportSize({width,height:1000});
    await navigate('settings','appearance');
    for (const theme of ['light','dark']) {
      await page.locator(`input[name="appearance"][value="${theme}"]`).check();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false,`${theme} appearance overflow at ${width}px`);
      await page.screenshot({path:`/tmp/zte-appearance-${theme}-${width}.png`});
      await navigate('network','signal');
      assert.ok(await page.locator('#cellular').isVisible());
      await page.screenshot({path:`/tmp/zte-theme-${theme}-${width}.png`});
      await navigate('settings','appearance');
    }
  }
  // Text must remain legible on the actual themed surfaces, including small labels.
  const contrast = async selector => page.locator(selector).first().evaluate(element => {
    const rgb = text => (text.match(/[\d.]+/g) || []).map(Number);
    const luminance = channels => {
      const values = channels.slice(0,3).map(value => { value /= 255; return value <= .04045 ? value/12.92 : ((value+.055)/1.055)**2.4; });
      return values[0]*.2126+values[1]*.7152+values[2]*.0722;
    };
    let node = element, background;
    while (node) {
      const color = rgb(getComputedStyle(node).backgroundColor);
      if (color.length === 3 || color[3] === 1) { background = color; break; }
      node = node.parentElement;
    }
    const values = [luminance(rgb(getComputedStyle(element).color)),luminance(background || [255,255,255])].sort((a,b)=>a-b);
    return (values[1]+.05)/(values[0]+.05);
  });
  for (const theme of ['light','dark']) {
    await page.locator(`input[name="appearance"][value="${theme}"]`).check();
    assert.ok(await contrast('.theme-choice:has(input:checked) small') >= 4.5,`${theme} helper text contrast`);
    await navigate('network','signal');
    for (const selector of ['#page-title','#page-description','.carrier-facts dt','.carrier-metrics dd']) {
      assert.ok(await contrast(selector+':first-of-type').catch(() => 0) >= 4.5,`${theme} text contrast for ${selector}`);
    }
    await navigate('settings','appearance');
  }
  await page.locator('input[name="appearance"][value="dark"]').focus();
  await page.keyboard.press('ArrowLeft');
  assert.equal(await page.locator('html').getAttribute('data-theme'),'light','appearance choices support native keyboard navigation');
  const second = await page.context().newPage();
  second.on('pageerror',error => errors.push(error.message));
  await second.goto(`${new URL(page.url()).origin}/#settings/appearance`);
  await second.locator('input[name="appearance"][value="dark"]').check();
  await page.waitForFunction(() => document.documentElement.dataset.appearance === 'dark');
  await second.close();
  const limited = await page.context().newPage();
  limited.on('pageerror',error => errors.push(error.message));
  await limited.addInitScript(() => {
    Storage.prototype.getItem = () => { throw new DOMException('Storage blocked','SecurityError'); };
    Storage.prototype.setItem = () => { throw new DOMException('Storage blocked','SecurityError'); };
  });
  await limited.emulateMedia({colorScheme:'dark'});
  await limited.goto(`${new URL(page.url()).origin}/#settings/appearance`);
  await limited.waitForFunction(() => document.documentElement.dataset.theme === 'dark');
  await limited.locator('input[name="appearance"][value="light"]').check();
  assert.equal(await limited.locator('html').getAttribute('data-theme'),'light');
  assert.match(await limited.locator('#appearance-status').textContent(),/Applied in this tab/);
  await limited.close();
  await page.locator('input[name="appearance"][value="light"]').check();
  await navigate('dashboard','overview');
  const viewports = [
    {width:320,height:740,layout:'mobile'},
    {width:390,height:844,layout:'mobile',preview:true},
    {width:600,height:900,layout:'mobile'},
    {width:767,height:1024,layout:'mobile'},
    {width:768,height:1024,layout:'tablet'},
    {width:820,height:1180,layout:'tablet',preview:true},
    {width:1024,height:768,layout:'tablet'},
    {width:1199,height:900,layout:'tablet'},
    {width:1200,height:900,layout:'desktop'},
    {width:1440,height:1100,layout:'desktop',preview:true},
    {width:1920,height:1080,layout:'desktop'}
  ];
  for (const {width,height,layout,preview} of viewports) {
    await page.setViewportSize({width,height});
    await navigate('dashboard','overview');
    await page.evaluate(() => { document.documentElement.style.scrollBehavior = 'auto'; scrollTo(0,0); });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const geometry = await page.evaluate(() => {
      const rail = document.querySelector('.sidebar').getBoundingClientRect();
      const nav = document.querySelector('#primary-navigation').getBoundingClientRect();
      const linkSizes = [...document.querySelectorAll('#primary-navigation summary')].map(link => {
        const rect = link.getBoundingClientRect(); return {width:rect.width,height:rect.height};
      });
      return {
        overflow: document.documentElement.scrollWidth > innerWidth,
        railWidth: rail.width, navBottom:nav.bottom,
        linkSizes, rowLayout: getComputedStyle(document.querySelector('#client-list tr')).display
      };
    });
    assert.equal(geometry.overflow, false, `page overflow at ${width}px`);
    assert.ok(geometry.linkSizes.every(size => size.width >= 44 && size.height >= 44), `navigation touch targets at ${width}px`);
    if (layout === 'mobile') {
      assert.equal(geometry.rowLayout, 'grid', 'mobile devices use cards');
      assert.ok(Math.abs(geometry.navBottom - height) <= 1, 'mobile navigation remains at the bottom');
    } else {
      assert.equal(geometry.rowLayout, 'table-row', 'larger screens use a device table');
      assert.equal(geometry.railWidth, layout === 'tablet' ? 224 : 252);
    }
    await page.mouse.move(width - 5, 5);
    if (preview) {
      await page.screenshot({path:`/tmp/zte-dashboard-${layout}.png`});
      await navigate('network','signal');
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false,`carrier inventory overflow at ${width}px`);
      await page.screenshot({path:`/tmp/zte-cellular-${layout}.png`});
    }
    await navigate('network','clients');
    await page.waitForFunction(() => location.hash === '#network/clients');
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.ok(await page.locator('#device-search').isVisible(), `device search reachable at ${width}px`);
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth),false,`content overflow at ${width}px`);
    if (layout === 'mobile' && preview) await page.screenshot({path:'/tmp/zte-dashboard-mobile-devices.png'});
  }
  // Long router-provided names must wrap without making the page wider.
  fixtures.wifi.ssid_2g = 'A'.repeat(64);
  fixtures.clients.hosts['aa:bb:cc:dd:ee:01'].name = 'A'.repeat(100);
  push(['wifi','clients']);
  await page.waitForFunction(() => document.querySelector('#ssid-2g').textContent.length === 64);
  for (const width of [320,768,1440]) {
    await page.setViewportSize({width,height:900});
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, `long-name overflow at ${width}px`);
  }
  fixtures.wifi.ssid_2g = 'OpenU60';
  fixtures.clients.hosts['aa:bb:cc:dd:ee:01'].name = 'MacBook Pro';
  await page.setViewportSize({width:390,height:844});
  await page.evaluate(() => scrollTo(0, document.documentElement.scrollHeight));
  const footerBottom = await page.locator('main footer').evaluate(element => element.getBoundingClientRect().bottom);
  const navigationTop = await page.locator('#primary-navigation').evaluate(element => element.getBoundingClientRect().top);
  assert.ok(footerBottom <= navigationTop, 'bottom navigation must not cover the last content');
  await navigate('dashboard','overview');

  const expectedRoots = ['Dashboard','Network','SMS','Tools','Settings'];
  assert.deepEqual(await page.locator('#primary-navigation .nav-text').allTextContents(),expectedRoots);
  const beforeNavigation = requests.filter(request => request.url.startsWith('/api/dashboard/events')).length;
  await navigate('network','wifi'); assert.ok(await page.locator('#wireless').isVisible());
  assert.equal(await page.locator('#cellular').isVisible(),false);
  await navigate('network','clients'); await page.locator('#device-search').fill('iPhone');
  await navigate('tools','device'); assert.ok(await page.locator('#system').isVisible());
  await navigate('network','clients'); assert.equal(await page.locator('#device-search').inputValue(),'iPhone');
  await page.locator('#device-search').fill('');
  await navigate('settings','about'); assert.ok(await page.locator('.about-panel').isVisible());
  await live();
  assert.ok(requests.filter(request=>request.url.startsWith('/api/dashboard/events')).slice(beforeNavigation).some(request=>request.url.includes('topics=clients')),'Connected Devices opts into its detailed shared subscription');
  await navigate('settings','connection');
  await page.goBack();
  await page.waitForFunction(() => document.documentElement.dataset.route === 'settings/about');
  await navigate('sms','inbox'); await live();
  await page.waitForFunction(() => document.querySelector('#sms-count').textContent === '2');
  assert.match(await page.locator('#message-list').textContent(),/Hello U60/);
  assert.equal(await page.locator('#message-list img').count(),0,'SMS is rendered as text');
  await page.locator('#sms-search').fill('Carrier');
  assert.equal(await page.locator('.message-card').count(),1);
  await navigate('sms','storage');
  assert.equal(await page.locator('#sms-internal').textContent(),'2 / 500 messages');
  await navigate('sms','inbox'); assert.equal(await page.locator('#sms-search').inputValue(),'Carrier');
  await page.locator('#sms-search').fill('');
  fixtures.sms.messages.unshift({id:3,number:'New sender',content:'004E00650077',date:'26,10,25,12,40,00,+32',tag:'1'}); push(['sms']);
  await page.waitForFunction(() => document.querySelector('#sms-count').textContent === '3');
  await page.screenshot({path:'/tmp/zte-navigation-sms.png'});
  await navigate('network','signal'); await live();
  assert.equal(await page.locator('.message-card').count(),0,'message content is cleared when leaving SMS');
  assert.ok(requests.some(request => request.url === '/api/dashboard/events?sms=1'));
  await navigate('dashboard','history');
  assert.ok(await page.locator('#history-empty').isVisible());
  await navigate('dashboard','overview');

  for (const response of streams) send(response,'patch',{signal:{ok:false,error:'unavailable'}});
  await page.waitForFunction(() => document.querySelector('#signal-quality').textContent === 'Last known');
  assert.equal(await page.locator('#rsrp').textContent(),'-84');
  assert.match(await page.locator('#cellular-freshness').textContent(),/not live/);
  push(['signal']); await page.waitForFunction(() => document.querySelector('#rsrp').textContent === '-84');
  assert.equal(await page.locator('#lte-carriers .carrier-card').count(),3);
  assert.equal(await page.locator('#nr-carriers .carrier-card').count(),2);
  assert.equal(await page.locator('#access-mode').textContent(),'5G NSA');
  assert.match(await page.locator('#nr-carriers').textContent(),/641280/);
  assert.match(await page.locator('#lte-carriers').textContent(),/-94 dBm/);
  fixtures.signal.network_type = 'NR-SA'; push(['signal']);
  await page.waitForFunction(() => document.querySelector('#access-mode').textContent === '5G SA');
  assert.match(await page.locator('#lte-status').textContent(),/not used in SA/);
  fixtures.signal.network_type = 'LTE-NSA'; push(['signal']);
  await page.waitForFunction(() => document.querySelector('#access-mode').textContent === '5G NSA');
  const servingSignal = structuredClone(fixtures.signal);
  fixtures.signal = {network_provider:'Example Mobile',network_type:'NO SERVICE',lte_rsrp:0,nr5g_rsrp:0};
  fixtures.wan.up = false; push(['signal','wan']);
  await page.waitForFunction(() => document.querySelector('#connection-title').textContent === 'Mobile data offline');
  assert.equal(await page.locator('#rsrp').textContent(),'—','no service must not claim live signal');
  assert.equal(await page.locator('#nr-carriers .carrier-card').count(),2,'last serving carriers survive service loss');
  assert.match(await page.locator('#nr-status').textContent(),/Last known/);
  assert.match(await page.locator('#cellular-freshness').textContent(),/No current serving signal/);
  assert.equal(await page.locator('#download').textContent(),'—');
  assert.equal(await page.locator('#received').textContent(),'8.40 GB');
  await page.locator('#availability').scrollIntoViewIfNeeded();
  await page.screenshot({path:'/tmp/zte-dashboard-offline-mobile.png'});
  await page.setViewportSize({width:1440,height:1100});
  await page.locator('#availability').scrollIntoViewIfNeeded();
  await page.screenshot({path:'/tmp/zte-dashboard-offline.png'});
  await navigate('settings','appearance');
  await page.locator('input[name="appearance"][value="dark"]').check();
  await navigate('dashboard','overview');
  await page.locator('#availability').scrollIntoViewIfNeeded();
  assert.ok(await contrast('.availability-panel h2') >= 4.5,'dark offline heading contrast');
  assert.ok(await contrast('#cellular-freshness') >= 4.5,'dark stale-reading text contrast');
  await page.screenshot({path:'/tmp/zte-theme-dark-offline.png'});
  fixtures.signal = servingSignal; fixtures.wan.up = true; push(['signal','wan','speed']);
  await page.waitForFunction(() => document.querySelector('#connection-title').textContent === 'Cellular connection active');
  assert.equal(await page.locator('#availability-title').textContent(),'Connection restored');
  fixtures.modem.operate_mode = 'LPM'; push(['modem']);
  await page.waitForFunction(() => document.querySelector('#connection-title').textContent.includes('Radio offline'));
  assert.equal(await page.locator('#cellular').getAttribute('data-stale'),'true');
  fixtures.modem.operate_mode = 'ONLINE'; push(['modem']);
  await page.waitForFunction(() => document.querySelector('#connection-title').textContent === 'Cellular connection active');
  assert.equal(await page.locator('#signal-quality').textContent(),'Good','a modem-only recovery patch restores dependent signal status');
  push(['speed']);
  // Exercise multiline data and a CRLF delimiter split across HTTP chunks.
  fragmentedFrame = true;
  const splitPayload = JSON.stringify({readings:{battery:{ok:true,data:{...fixtures.battery,capacity:72}}}});
  for (const response of streams) {
    response.write(`event: patch\r\ndata: ${splitPayload.slice(0,12)}\r\ndata: ${splitPayload.slice(12)}\r`);
    setTimeout(() => { response.write('\n\r\n'); fragmentedFrame = false; },20);
  }
  await page.waitForFunction(() => document.querySelector('#battery').textContent === '72');
  // An expired session is locked in-place; the next stream uses the bearer header.
  requiresAuth = true;
  for (const response of streams) response.write('event: auth\ndata: {}\n\n');
  await page.waitForFunction(() => document.querySelector('#login-dialog').open);
  assert.equal(await page.locator('#download').textContent(),'—');
  assert.ok(await contrast('dialog input') >= 4.5,'dark login input contrast');
  await page.screenshot({path:'/tmp/zte-theme-dark-login.png'});
  await page.locator('#password').fill('wrong'); await page.locator('#login-form button[type=submit]').click();
  await page.waitForFunction(() => document.querySelector('#login-error').textContent.includes('Incorrect'));
  await page.locator('#password').fill('test-password'); await page.locator('#login-form button[type=submit]').click();
  await live(); assert.equal(await page.locator('#password').inputValue(),'');
  assert.ok(requests.some(request => request.url === '/api/dashboard/events' && request.authorization === 'Bearer test-token'));
  for (const width of [390,820,1440]) {
    await page.setViewportSize({width,height:900});
    const rect = await page.locator('#signout').boundingBox();
    assert.ok(rect && rect.width >= 44 && rect.height >= 44,`lock control at ${width}px`);
    await page.locator('#signout').click();
    await page.waitForFunction(() => document.querySelector('#login-dialog').open);
    await page.locator('#password').fill('test-password'); await page.locator('#login-form button[type=submit]').click();
    await live();
  }
  mode = 'offline'; disconnect();
  await page.waitForFunction(() => document.querySelector('#stream-state').textContent === 'Reconnecting');
  assert.equal(await page.locator('#download').textContent(),'—');
  assert.equal(await page.locator('#nr-carriers .carrier-card').count(),2,'router loss retains the last carrier inventory');
  assert.match(await page.locator('#cellular-freshness').textContent(),/not live/);
  assert.equal(await page.locator('#received').textContent(),'8.40 GB');
  mode = 'live'; await live();
  assert.equal(await page.locator('#download').textContent(),'123.20','reconnect receives a complete snapshot');
  // Background tabs close their stream and get a fresh snapshot on return.
  await page.evaluate(() => {
    Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await page.waitForFunction(() => document.querySelector('#stream-state').textContent === 'Paused');
  await page.evaluate(() => {
    Object.defineProperty(document,'hidden',{configurable:true,get:()=>false});
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await live();
  // A late initial snapshot must not repopulate a locked console.
  mode = 'delayed'; disconnect();
  await page.waitForFunction(() => document.querySelector('#stream-state').textContent === 'Connecting');
  while (!delayedResponse) await new Promise(resolve => setTimeout(resolve,10));
  await page.locator('#signout').click();
  if (!delayedResponse.destroyed) send(delayedResponse,'snapshot',envelope(Object.keys(fixtures)));
  await page.waitForFunction(() => document.querySelector('#login-dialog').open);
  assert.equal(await page.locator('#hostname').textContent(),'ZTE U60 Pro');
  assert.equal(await page.locator('#client-list').textContent(),'Device data unavailable.');
  assert.ok(requests.every(request => ['/api/dashboard/events','/api/auth/login','/api/wifi/status','/api/wifi/guest'].includes(request.url.split('?')[0])),'no polling of individual API readings');
  assert.deepEqual(errors,[]);
  await browser.close(); clearInterval(heartbeat); disconnect(); server.close();
  console.log('Passed: saved Light/Dark/System themes, system appearance changes, themed phone/tablet/desktop and contrast, five primary tabs, expandable sidebars, routed pages and browser back, state preservation, optional live SMS, LTE and NR carrier aggregation, SA/NSA, no-service and LPM states, retained offline statistics, SSE snapshots and dependency patches, split frames, auth headers and expiry, automatic reconnection, tab visibility, no endpoint polling, 11 responsive viewports, search and session lock.');
})().catch(error => {console.error(error);process.exit(1);});
