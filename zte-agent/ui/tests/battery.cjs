const {chromium}=require('playwright'),assert=require('node:assert/strict'),http=require('node:http'),fs=require('node:fs'),path=require('node:path');
(async()=>{
const ok=data=>({ok:true,data}),streams=new Set(),requests=[],errors=[];
let packet={supplies:ok({battery:{status:'Charging',present:1,capacity:94,temp:340,voltage_now:4400000,current_now:1000000,power_now:55522725,charge_full:10522000,charge_full_design:10214000,cycle_count:6,health:'Good',time_to_full_now:1982},usb:{online:1,voltage_now:4750000,current_now:1400000,usb_type:'SDP [CDP] PD'}}),firmware:ok({battery_time_to_full:35,battery_time_to_empty:-1}),charger:ok({direct_power_supply_mode:'disable',otg_powerbank_state:0}),control:ok({charge_limit_enabled:true,charge_limit:85,hysteresis:5,charging_stopped:false})};
const core={device:ok({hostname:'Example router',uptime_secs:100}),wan:ok({up:true}),wan6:ok({up:false}),signal:ok({network_type:'LTE'}),modem:ok({operate_mode:'ONLINE'}),battery:ok({capacity:94,status:'Charging',temperature:340}),wifi:ok({}),clients:ok({hosts:{}}),speed:ok({rx_speed:0,tx_speed:0})};
const frame=(res,event,readings)=>res.write(`event: ${event}\ndata: ${JSON.stringify({readings})}\n\n`);
const server=http.createServer((req,res)=>{
 requests.push({url:req.url,method:req.method});
 if(req.url.startsWith('/api/dashboard/events')){res.power=new URL(req.url,'http://localhost').searchParams.get('topics')==='power';res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));frame(res,'snapshot',{...core,...(res.power?{battery_details:ok(packet)}:{})});return;}
 if(req.url.startsWith('/api/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify(ok({})));return;}
 const name=req.url==='/'?'index.html':/^\/console\/a\/[\w.-]+$/.test(req.url)?req.url.split('/').pop():null;
 if(!name){res.writeHead(404);res.end();return;}const file=path.join(__dirname,'..',name);if(!fs.existsSync(file)){res.writeHead(404);res.end();return;}
 res.setHeader('Content-Type',name.endsWith('.js')?'text/javascript':name.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const browser=await chromium.launch({headless:true,channel:'chrome'}),context=await browser.newContext(),page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
const text=id=>page.locator('#power-'+id).textContent(),push=()=>{for(const res of streams)if(res.power)frame(res,'patch',{battery_details:ok(packet)});};
try{
 await page.goto(`http://127.0.0.1:${server.address().port}/#tools/battery`);await page.waitForFunction(()=>OpenU60Console.connected&&document.querySelector('#power-power').textContent==='4.40 W');
 assert.equal(await text('usb-power'),'6.65 W');assert.equal(await text('until-full'),'34m');assert.equal(await text('until-empty'),'Not applicable');assert.equal(await text('resume'),'80 %');assert.match(await page.locator('#battery-overview .panel-note').textContent(),/inconsistent/);
 for(const theme of ['light','dark']){await page.evaluate(()=>OpenU60Navigation.go('settings/appearance'));await page.locator(`input[name=appearance][value=${theme}]`).check();await page.evaluate(()=>OpenU60Navigation.go('tools/battery'));await page.waitForFunction(()=>document.querySelector('#power-power').textContent==='4.40 W');for(const width of [390,820,1440]){await page.setViewportSize({width,height:1100});await page.waitForTimeout(120);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);}}
 await page.screenshot({path:'/tmp/openu60-battery-desktop.png',fullPage:true});
 packet.charger.data.otg_powerbank_state=1;packet.control.data.charge_limit_enabled=false;push();await page.waitForFunction(()=>document.querySelector('#power-usb-power').textContent==='Not applicable');assert.equal(await text('limit'),'85 %','configured thresholds remain visible when disabled');packet.charger.data.otg_powerbank_state=0;
 packet.supplies.data.battery.status='Discharging';packet.supplies.data.battery.current_now=-500000;packet.supplies.data.battery.time_to_empty_avg=7200;push();await page.waitForFunction(()=>document.querySelector('#power-power').textContent==='2.20 W');assert.equal(await text('until-full'),'Not applicable');assert.equal(await text('until-empty'),'2h');
 assert.ok(await page.locator('#power-power').evaluate(n=>n.children.length)>0,'live numbers use animated digit markup');
 packet.supplies.data.battery={status:'Not charging',cycle_count:0};packet.firmware.data={};push();await page.waitForFunction(()=>document.querySelector('#power-power').textContent==='Not reported');assert.equal(await text('temperature'),'Not reported');assert.equal(await text('cycles'),'0');assert.equal(await text('until-full'),'Not applicable');
 for(const res of streams)if(res.power)frame(res,'patch',{battery_details:{ok:false,error:'Read unavailable'}});await page.waitForFunction(()=>document.querySelector('#battery-overview .reading-caption').dataset.stale==='true');assert.equal(await text('cycles'),'0','failed readings retain explicitly stale measurements');
 const before=requests.filter(r=>r.url.startsWith('/api/dashboard/events')).length;await page.evaluate(()=>OpenU60Navigation.go('tools/device'));await page.waitForTimeout(200);assert.equal(await text('power'),'—');assert.equal(await page.evaluate(()=>OpenU60Console.data('battery_details')),undefined);
 assert.ok(requests.some(r=>r.url==='/api/dashboard/events?topics=power'));assert.ok(requests.filter(r=>r.url.startsWith('/api/dashboard/events')).length>before);assert.equal(requests.some(r=>r.url.startsWith('/api/')&&!r.url.startsWith('/api/dashboard/events')&&r.method!=='GET'),false);assert.deepEqual(errors,[]);
 console.log('Passed: streamed battery page, corrected wattage, distinct USB power, charge/runtime state changes, animated digits, missing values, both themes, responsive widths, topic cleanup and no setting changes.');
}finally{await browser.close();for(const res of streams)res.destroy();server.close();}
})().catch(e=>{console.error(e);process.exit(1);});
