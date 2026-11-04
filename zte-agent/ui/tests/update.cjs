const assert=require('node:assert/strict'),http=require('node:http'),fs=require('node:fs'),path=require('node:path'),{chromium}=require('playwright');
(async()=>{
  const requests=[],streams=new Set(),errors=[];
  let status={current:{version:'2.4.0',commit:'abc123456789',target:'aarch64-unknown-linux-musl'},channel:'stable',settings:{channel:'stable',automatic_checks:true,automatic_downloads:false,automatic_install:false},phase:'idle',available:null,staged:null,staged_slot:null,last_check:null,last_error:null,progress:{received:0,total:0,percent:0},signing_configured:true,bootstrap_ready:true,rollback_available:false,active_slot:'a',automatic_install_supported:false,restart_scope:'agent_only',install_readiness:{ready:true,battery_percent:80,external_power:false,network_type:'LTE',signal_bars:4,wan_ready:true}};
  const reading=data=>({ok:true,data}),baseReadings={device:reading({hostname:'U60'}),wan:reading({up:true}),wan6:reading({up:true}),signal:reading({network_type:'5G'}),modem:reading({operate_mode:'ONLINE'}),speed:reading({}),battery:reading({capacity:80}),wifi:reading({}),clients:reading({hosts:{}}),sms_capacity:reading({})};
  const snapshot=res=>res.write(`event: snapshot\ndata: ${JSON.stringify({readings:{...baseReadings,ota_status:reading(status)}})}\n\n`);
  const patchStatus=()=>{for(const res of streams)res.write(`event: patch\ndata: ${JSON.stringify({readings:{ota_status:reading(status)}})}\n\n`);};
  const reply=(res,data=status)=>{res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({ok:true,data}));};
  const release={schema:1,channel:'beta',version:'2.5.0-beta.1',tag:'v2.5.0-beta.1',commit:'def123456789',target:'aarch64-unknown-linux-musl',size:7340032,sha256:'a'.repeat(64),binary_url:'https://github.com/jesther-ai/open-u60-pro/releases/download/v2.5.0-beta.1/zte-agent',notes_url:'https://github.com/jesther-ai/open-u60-pro/releases/tag/v2.5.0-beta.1',published_at:'2026-11-02T00:00:00Z',minimum_updater_version:'2.4.0-beta.1'};
  const server=http.createServer((req,res)=>{
    if(!req.url.startsWith('/api/')){const file=req.url==='/'?'index.html':/^\/(?:ui|console\/a)\/[\w.-]+$/.test(req.url)?req.url.split('/').pop():'',full=path.join(__dirname,'..',file);if(!file||!fs.existsSync(full)){res.writeHead(404);res.end();return;}res.setHeader('Content-Type',file.endsWith('.css')?'text/css':file.endsWith('.js')?'text/javascript':'text/html');res.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'");res.end(fs.readFileSync(full));return;}
    if(req.url.startsWith('/api/dashboard/events')){res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));snapshot(res);return;}
    let raw='';req.on('data',chunk=>raw+=chunk);req.on('end',()=>{const body=raw?JSON.parse(raw):null;requests.push({url:req.url,method:req.method,body});
      if(req.url==='/api/update/settings'&&req.method==='PUT'){status={...status,channel:body.channel,settings:{...body,automatic_install:false}};reply(res);patchStatus();return;}
      if(req.url==='/api/update/check'&&req.method==='POST'){status={...status,phase:'idle',available:release,last_check:Math.floor(Date.now()/1000)};reply(res);patchStatus();return;}
      if(req.url==='/api/update/download'&&req.method==='POST'){
        // The backend reports exact integer progress. A stale/rounded 100 from an
        // older agent must still not make the UI finish before every byte arrives.
        status={...status,phase:'downloading',progress:{received:release.size-1,total:release.size,percent:100}};patchStatus();
        setTimeout(()=>{status={...status,progress:{received:release.size,total:release.size,percent:100}};patchStatus();},250);
        setTimeout(()=>{status={...status,phase:'staged',staged:release,staged_slot:'b',progress:{received:release.size,total:release.size,percent:100}};reply(res);patchStatus();},500);
        return;
      }
      if(req.url==='/api/update/install'&&req.method==='POST'){reply(res);return;}
      if(req.url==='/api/update/rollback'&&req.method==='POST'){reply(res);return;}
      reply(res,{});
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const heartbeat=setInterval(()=>{for(const res of streams)res.write(': heartbeat\n\n');},1000);
  const browser=await chromium.launch({headless:true,channel:'chrome'}),page=await browser.newPage({viewport:{width:1440,height:1000}});page.on('pageerror',error=>errors.push(error.message));
  const confirm=label=>page.locator('.confirm-dialog').getByRole('button',{name:label,exact:true}).click();
  try{
    await page.goto(`http://127.0.0.1:${server.address().port}/#settings/update`);await page.waitForFunction(()=>document.querySelector('#update-current-version')?.textContent==='v2.4.0');
    assert.match(await page.locator('.update-panel').textContent(),/A\/B recovery ready/);assert.equal(await page.locator('#update-install').isDisabled(),true);
    await page.locator('#update-channel').selectOption('beta');await page.locator('#update-auto-download').check();await page.locator('#update-save').click();await confirm('Use Beta');await page.waitForFunction(()=>document.querySelector('#update-feedback').textContent.includes('Preferences saved'));
    assert.deepEqual(requests.find(request=>request.url==='/api/update/settings').body,{channel:'beta',automatic_checks:true,automatic_downloads:true});
    await page.locator('#update-check').click();await page.waitForFunction(()=>document.querySelector('#update-available-version')?.textContent.includes('v2.5.0-beta.1'));assert.match(await page.locator('#update-release-copy').textContent(),/Signed Beta/);
    await page.locator('#update-download').click();
    await page.waitForFunction(()=>document.querySelector('#update-phase').textContent==='Downloading'&&document.querySelector('#update-percent').textContent==='99');
    assert.equal(await page.locator('#update-progress-bar').getAttribute('style'),'width: 99%;');
    await page.waitForFunction(()=>document.querySelector('#update-bytes').textContent.includes('verifying signed binary'));
    assert.equal(await page.locator('#update-percent').textContent(),'99','verification never presents a premature 100%');
    await page.waitForFunction(()=>document.querySelector('#update-feedback').textContent.includes('staged'));assert.equal(await page.locator('#update-install').isDisabled(),false);assert.equal(await page.locator('#update-rollback').isDisabled(),true);assert.equal(await page.locator('#update-percent').textContent(),'100');
    await page.locator('#update-install').click();await confirm('Install agent');await page.waitForFunction(()=>document.querySelector('#update-feedback').textContent.includes('reconnect automatically'));assert.deepEqual(requests.find(request=>request.url==='/api/update/install').body,{confirm:true});
    const before=requests.length;await page.waitForTimeout(1200);assert.equal(requests.length,before,'the update page receives live SSE and never polls while idle');
    for(const width of [320,390,820,1440]){await page.setViewportSize({width,height:1000});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`update page overflow at ${width}px`);}
    assert.deepEqual(errors,[]);console.log('Passed: signed update channel settings, staging, confirmation, SSE-only live status and responsive layout.');
  }finally{await browser.close();clearInterval(heartbeat);for(const stream of streams)stream.destroy();server.closeAllConnections();server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
