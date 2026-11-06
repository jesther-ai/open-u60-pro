// Exercises real digit animation on the streamed UI; no router is contacted.
const {chromium} = require('playwright');
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
(async()=>{
  const streams = new Set(), requests = [], errors = [];
  const data = {
    device:{hostname:'OpenU60',uptime_secs:1234,kernel:'test',load_avg:[0,0,0]},
    speed:{rx_speed:12500000,tx_speed:1250000,rx_bytes:999000000,tx_bytes:1200000000},
    battery:{capacity:80,status:'Charging',temperature:320},wan:{up:true},modem:{operate_mode:'ONLINE'},
    signal:{network_type:'LTE-NSA',lte_pci:'123',wan_active_band:'3',wan_active_channel:'1800',lte_rsrp:'-90',lte_rsrq:'-10',lte_snr:'20',nr5g_action_band:'78',nr5g_action_channel:'640000',nr5g_pci:'321',nr5g_rsrp:'-85',nr5g_rsrq:'-9',nr5g_snr:'25'},
    clients:{hosts:{'aa:bb:cc:dd:ee:ff':{name:'Laptop',ipaddrs:['192.168.0.2']}},dhcp_leases:[]},
  };
  const resources={cpu:{ok:true,data:{overall:10,cores:[10,20]}},memory:{ok:true,data:{usage_pct:40,available_kb:102400}},thermal:{ok:true,data:{sensors:[{id:'thermal0',name:'CPU',temperature_c:40}]}},processes:{ok:true,data:{total_count:1,processes:[{pid:1,name:'agent',cpu_pct:10,rss_kb:10240,state:'S'}]}},lan:{ok:true,data:{}},wan6:{ok:true,data:{}}};
  const frame=(res,event,values)=>res.write(`event: ${event}\ndata: ${JSON.stringify({readings:values})}\n\n`);
  function push(names) { for(const res of streams){const values=Object.fromEntries(names.map(name=>[name,{ok:true,data:name==='resources'?resources:data[name]}]));frame(res,'patch',values);} }
  const server=http.createServer((req,res)=>{
    if(req.url.startsWith('/api/')) requests.push(req.url);
    if(req.url.startsWith('/api/dashboard/events')){res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));const initial=Object.fromEntries(Object.entries(data).map(([name,data])=>[name,{ok:true,data}]));if(req.url.includes('topics=health'))initial.resources={ok:true,data:resources};frame(res,'snapshot',initial);return;}
    const file=req.url==='/'?'index.html':/^\/(?:ui|console\/a)\/[\w.-]+$/.test(req.url)?req.url.split('/').pop():'',full=path.join(__dirname,'..',file);
    if(!file||!fs.existsSync(full)){res.writeHead(404);res.end();return;}
    res.setHeader('Content-Type',file.endsWith('.css')?'text/css':file.endsWith('.js')?'text/javascript':'text/html');res.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'");res.end(fs.readFileSync(full));
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const heartbeat=setInterval(()=>{for(const stream of streams)stream.write(': heartbeat\n\n');},1000);
  const browser=await chromium.launch({headless:true,channel:'chrome'}),page=await browser.newPage({viewport:{width:1440,height:1100},reducedMotion:'no-preference'});
  page.on('pageerror',error=>errors.push(error.message));
  await page.addInitScript(()=>{
    window.__numberAnimations=0;
    const original=Element.prototype.animate;
    Element.prototype.animate=function(...args){if(this.classList.contains('number-current'))window.__numberAnimations++;return original.apply(this,args);};
  });
  const go=async key=>{await page.evaluate(key=>OpenU60Navigation.go(key),key);await page.waitForFunction(key=>document.documentElement.dataset.route===key,key);await page.waitForFunction(()=>document.querySelector('#stream-state').textContent==='Live');};
  const settled=()=>page.waitForFunction(()=>!document.querySelector('[data-number-animating]'));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);await page.waitForFunction(()=>document.querySelector('#download').textContent==='100.00');
  assert.equal(await page.evaluate(()=>window.__numberAnimations),0,'initial readings never count from invented zeroes');
  const height=await page.locator('#download').evaluate(node=>node.getBoundingClientRect().height);
  data.speed.rx_speed=15000000;push(['speed']);
  await page.waitForFunction(()=>document.querySelector('#download').textContent==='120.00');
  assert.equal(await page.locator('#download').getAttribute('aria-label'),'120.00');
  assert.equal(await page.locator('#download').getAttribute('data-number-animating'),'true');
  assert.ok(await page.locator('#download .number-ghost:not([hidden])').count()>0,'changed digits have an outgoing visual layer');
  assert.equal(await page.locator('#download .number-ghost').first().getAttribute('aria-hidden'),'true');
  assert.equal(await page.locator('#download .number-ghost').first().textContent(),'','old visual digits never duplicate the current text');
  assert.equal(await page.locator('#download').evaluate(node=>node.getBoundingClientRect().height),height,'digit rolls must not change the line height');
  await page.locator('#download').evaluate(node=>node.getAnimations({subtree:true}).forEach(animation=>{animation.pause();animation.currentTime=140;}));
  await page.screenshot({path:'/tmp/zte-number-transition.png'});
  await page.locator('#download').evaluate(node=>node.getAnimations({subtree:true}).forEach(animation=>animation.play()));
  const starts=await page.evaluate(()=>window.__numberAnimations);push(['speed']);await page.waitForTimeout(50);assert.equal(await page.evaluate(()=>window.__numberAnimations),starts,'identical SSE values do not restart motion');
  data.speed.rx_speed=16000000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').textContent==='128.00');
  data.speed.rx_speed=17000000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').textContent==='136.00');await settled();assert.equal(await page.locator('#download').textContent(),'136.00','rapid patches keep the newest reading');
  // Growing and shrinking the integer must preserve place values without padding zeroes.
  data.battery.capacity=99;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='99');await settled();
  await page.evaluate(()=>window.__onesDigit=[...document.querySelectorAll('#battery .number-slot')].at(-1));
  data.battery.capacity=100;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='100');assert.equal(await page.evaluate(()=>window.__onesDigit===[...document.querySelectorAll('#battery .number-slot')].at(-1)),true);await settled();
  data.battery.capacity=0;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='0');assert.equal(await page.locator('#battery').getAttribute('data-number-animating'),'true');await settled();
  data.battery.capacity=null;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='—');assert.equal(await page.locator('#battery').getAttribute('data-number-animating'),null);
  data.battery.capacity=80;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='80');assert.equal(await page.locator('#battery').getAttribute('data-number-animating'),null);
  // Formatting changes animate as one update and preserve the exact new unit.
  data.speed.rx_bytes=1000000000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#received').textContent==='1.00 GB');await settled();assert.equal(await page.locator('#received').getAttribute('aria-label'),'1.00 GB');
  data.speed.rx_bytes=1010000000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#received').textContent==='1.01 GB');await settled();
  await go('network/signal');await page.locator('#lte-carriers .carrier-metrics').scrollIntoViewIfNeeded();await page.evaluate(()=>{window.__carrier=document.querySelector('#lte-carriers .carrier-card');});
  data.signal.lte_rsrp='-94';push(['signal']);await page.waitForFunction(()=>document.querySelector('#lte-carriers .carrier-metrics dd').textContent==='-94 dBm');
  assert.equal(await page.evaluate(()=>window.__carrier===document.querySelector('#lte-carriers .carrier-card')),true,'carrier measurements keep stable DOM nodes');
  assert.equal(await page.locator('#lte-carriers .carrier-metrics dd').first().getAttribute('data-number-animating'),'true');
  assert.equal(await page.locator('#pci .number-display').count(),0,'cell identifiers never roll through fake values');await settled();
  // Diagnostic metrics use the same renderer, with stable sensor and process identities.
  await go('tools/diagnostics');await page.waitForFunction(()=>document.querySelector('#diagnostic-health dd').textContent==='10.0 %');await page.locator('#diagnostic-health').scrollIntoViewIfNeeded();await page.evaluate(()=>window.__sensor=document.querySelector('.sensor-card'));
  resources.cpu.data.overall=25;resources.thermal.data.sensors[0].temperature_c=42;push(['resources']);await page.waitForFunction(()=>document.querySelector('#diagnostic-health dd').textContent==='25.0 %');assert.equal(await page.locator('#diagnostic-health dd').first().getAttribute('data-number-animating'),'true');assert.equal(await page.evaluate(()=>window.__sensor===document.querySelector('.sensor-card')),true);await settled();
  await page.locator('#diagnostic-processes').scrollIntoViewIfNeeded();await page.locator('#process-search').fill('agent');await page.evaluate(()=>window.__process=document.querySelector('#diagnostic-processes tbody tr'));
  resources.processes.data.processes[0].cpu_pct=15;push(['resources']);await page.waitForFunction(()=>document.querySelector('#diagnostic-processes tbody tr td:nth-child(3)').textContent==='15.0 %');assert.equal(await page.evaluate(()=>window.__process===document.querySelector('#diagnostic-processes tbody tr')),true);assert.equal(await page.locator('#process-search').inputValue(),'agent');assert.equal(await page.locator('#diagnostic-processes tbody tr td:nth-child(2) .number-display').count(),0);await settled();
  await go('dashboard/overview');await page.evaluate(()=>scrollTo({top:0,behavior:'instant'}));
  data.battery.capacity=79;push(['battery']);await page.waitForFunction(()=>document.querySelector('#battery').textContent==='79');await page.emulateMedia({reducedMotion:'reduce'});await settled();const reducedStarts=await page.evaluate(()=>window.__numberAnimations);
  data.speed.rx_speed=18000000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').textContent==='144.00');assert.equal(await page.evaluate(()=>window.__numberAnimations),reducedStarts,'reduced motion updates instantly');
  await page.emulateMedia({reducedMotion:'no-preference'});
  for(const width of [390,820,1440])for(const theme of ['light','dark']){
    await page.setViewportSize({width,height:1100});await go('settings/appearance');await page.locator(`input[name=appearance][value=${theme}]`).check();await go('dashboard/overview');await page.evaluate(()=>scrollTo({top:0,behavior:'instant'}));data.speed.rx_speed+=125000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').hasAttribute('data-number-animating'));assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`${theme} animated number overflow at ${width}`);await settled();
  }
  // Hiding the tab settles active visuals and never starts background animations.
  data.speed.rx_speed+=125000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').hasAttribute('data-number-animating'));
  await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>true});document.dispatchEvent(new Event('visibilitychange'));});await settled();
  await page.evaluate(()=>{Object.defineProperty(document,'hidden',{configurable:true,get:()=>false});document.dispatchEvent(new Event('visibilitychange'));});await page.waitForFunction(()=>document.querySelector('#stream-state').textContent==='Live');
  data.speed.rx_speed+=125000;push(['speed']);await page.waitForFunction(()=>document.querySelector('#download').hasAttribute('data-number-animating'));for(const stream of streams)stream.write('event: auth\ndata: {}\n\n');await page.waitForFunction(()=>document.querySelector('#login-dialog').open);await settled();await page.waitForTimeout(450);assert.equal(await page.locator('#download').textContent(),'—','no animation can restore values after locking');assert.equal(await page.locator('#diagnostic-health dd').first().textContent(),'—');
  assert.ok(requests.every(url=>url.startsWith('/api/dashboard/events')),'animations add no requests or polling');assert.deepEqual(errors,[]);
  await browser.close();clearInterval(heartbeat);for(const stream of streams)stream.destroy();server.close();
  console.log('Passed: real-value digit rolls, immediate current text and accessible labels, stable line height, unchanged and interrupted SSE updates, unit changes, stable carrier/sensor/process nodes, IDs and inputs excluded, reduced-motion changes, hidden tabs, lock cancellation, no added requests, and both themes across phone/tablet/desktop.');
})().catch(error=>{console.error(error);process.exit(1);});
