const assert=require('node:assert/strict'),{chromium}=require('playwright'),{start}=require('./fixture.cjs');
(async()=>{
  const off={connected:false,network_id:null,saved_network_id:null,width_mode:null,ssid:null,frequency:null,width:null,width_state:null,address:null,gateway:null,networks:[],internet:'not_verified',ipv6_relay:false,ipv6:{requested:false,state:'off',upstream:false,nat66:false,internet:'not_verified'},cellular_fallback:true,restoring:null,experimental:true,error:null,clock_error:null,recovery_required:false,autostart:false,saved_networks:[]};
  let status={...off};
  const scan=[
    {id:'hotel5',ssid:'Hotel Guest',frequency:5180,signal:-48,security:'wpa2',supported:true,remembered:true,width_mode:'auto'},
    {id:'cafe24',ssid:'Cafe <b>Open</b>',frequency:2437,signal:-71,security:'open',supported:true,remembered:true,width_mode:'off'},
    {id:'home5',ssid:'Home',frequency:5745,signal:-60,security:'wpa2',supported:true,remembered:false,width_mode:'off'},
    {id:'wpa3',ssid:'Modern',frequency:5220,signal:-50,security:'unsupported',supported:false,remembered:false,width_mode:'off'}
  ];
  const saved=[{id:'k-hotel',ssid:'Hotel Guest',security:'wpa2',width_mode:'auto',remembered:true},{id:'k-cafe',ssid:'Cafe <b>Open</b>',security:'open',width_mode:'off',remembered:true}];
  let refuse=null,hang=false,restoringPolls=0;
  const fixture=await start(({method,url,body,res},reply,fail)=>{
    if(!url.startsWith('/api/wifi-relay/'))return false;
    if(method==='GET'&&url==='/api/wifi-relay/status'){if(status.restoring)restoringPolls++;reply(status);return true;}
    if(method!=='POST')return false;
    if(hang&&url==='/api/wifi-relay/connect'){res.writeHead(200,{'Content-Type':'application/json'});res.end('{"ok":tr');hang=false;status={...status,connected:true,ssid:'Hotel Guest',address:'192.168.8.23',gateway:'192.168.8.1',frequency:5180,width:80,width_state:'stable',width_mode:'auto',saved_network_id:'k-hotel',autostart:true,networks:scan,saved_networks:saved,ipv6:{...off.ipv6,state:'configured',upstream:true,address:'2001:db8::2',lan_prefix:'fd00:1::/64'}};return true;}
    if(url==='/api/wifi-relay/scan'){status={...status,networks:scan,saved_networks:saved};reply(status);return true;}
    if(url==='/api/wifi-relay/connect'){
      if(refuse){fail(refuse,503);return true;}
      const net=scan.find(n=>n.id===body.network);
      status={...status,connected:true,ssid:net.ssid,address:'192.168.8.23',gateway:'192.168.8.1',frequency:net.frequency,width:80,width_state:'stable',width_mode:body.width_mode||net.width_mode,saved_network_id:'k-hotel',autostart:true,networks:scan,saved_networks:saved,clock_error:net.id==='home5'?'The router clock could not be verified over this uplink.':null,ipv6:{...off.ipv6,state:body.ipv6?'configured':'off',upstream:!!body.ipv6,address:'2001:db8::2',lan_prefix:'fd00:1::/64'}};
      reply(status);return true;
    }
    if(url==='/api/wifi-relay/disconnect'){status={...off,networks:scan,saved_networks:saved};reply(status);return true;}
    if(url==='/api/wifi-relay/clock'){status={...status,clock_error:null};reply(status);return true;}
    if(url==='/api/wifi-relay/tuning'){saved.find(s=>s.id===body.id).width_mode=body.width_mode;status={...status,saved_networks:saved};reply(status);return true;}
    if(url==='/api/wifi-relay/forget'){const i=saved.findIndex(s=>s.id===body.id);if(i>=0)saved.splice(i,1);status={...status,saved_networks:saved};reply(status);return true;}
    return false;
  });
  const browser=await chromium.launch({headless:true,channel:'chrome'}),page=await browser.newPage({viewport:{width:1440,height:1400}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  const text=sel=>page.locator(sel).textContent(),posts=()=>fixture.requests.filter(r=>r.method==='POST'),last=()=>posts().at(-1);
  const said=(sel,words)=>page.waitForFunction(([sel,words])=>document.querySelector(sel)?.textContent.includes(words),[sel,words]);
  const confirm=label=>page.locator('.confirm-dialog').getByRole('button',{name:label,exact:true}).click();
  const pick=name=>page.locator('.relay-network',{hasText:name}).click();
  try{
    // Opening the page reads state and never starts work.
    await page.goto(`${fixture.url}/#network/relay`);await said('#relay-status .relay-state','RELAY OFF');
    assert.match(await text('#relay-status h3'),/Share a Wi-Fi connection/);assert.equal(posts().length,0,'opening the page must not start work');
    assert.equal(await page.locator('#relay-status .relay-facts').isHidden(),true);assert.equal(await page.getByRole('button',{name:'Disconnect relay'}).isHidden(),true);
    assert.match(await text('#relay-saved'),/No saved Wi-Fi networks/);
    const before=fixture.requests.length;await page.waitForTimeout(1200);assert.equal(fixture.requests.length,before,'an idle relay page does not poll');

    // Scan lists networks; text is inert and unsupported networks cannot be chosen.
    await page.getByRole('button',{name:'Scan',exact:true}).click();await said('#relay-networks .action-status','Found 4 networks');
    assert.equal(await page.locator('.relay-network').count(),4);assert.equal(await page.locator('#relay-networks strong b').count(),0,'SSIDs are never parsed as HTML');
    assert.match(await text('.relay-network:has-text("Cafe")'),/Cafe <b>Open<\/b>/);
    assert.equal(await page.locator('.relay-network:has-text("Modern") input').isDisabled(),true);assert.match(await text('.relay-network:has-text("Modern")'),/Unsupported/);
    assert.match(await text('.relay-network:has-text("Hotel")'),/5 GHz · -48 dBm · WPA2/);assert.match(await text('.relay-network:has-text("Hotel")'),/Saved/);assert.equal(await page.getByRole('button',{name:'Rescan'}).count(),1);
    assert.equal(await page.locator('#relay-password').getAttribute('type'),'password');assert.equal(await page.locator('#relay-password').getAttribute('autocomplete'),'new-password');
    assert.equal(await page.getByRole('button',{name:'Connect to Wi-Fi'}).isDisabled(),true,'nothing selected yet');

    // A password the router would refuse is caught before the hotspot is touched.
    await pick('Home');await page.locator('#relay-password').fill('');await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();
    await said('#relay-networks .action-status','Enter the Wi-Fi password');
    await page.locator('#relay-password').fill('short');await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();await said('#relay-networks .action-status','8–63 printable ASCII');
    assert.equal(posts().filter(r=>r.url.endsWith('/connect')).length,0);

    // A definite refusal shows the router’s reason and re-reads the state.
    refuse='Wi-Fi association failed; check password and signal';
    await page.locator('#relay-password').fill('correct horse');await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();await confirm('Connect');
    await said('#relay-networks .action-status','Wi-Fi association failed');assert.equal(await page.locator('#relay-networks .action-status').getAttribute('data-error'),'true');
    assert.deepEqual(last().body,{network:'home5',password:'correct horse',confirmed:true,ipv6:true,remember:true});refuse=null;

    // A remembered network connects with an empty password; the saved bandwidth is expressed by omission.
    await pick('Hotel');assert.equal(await page.locator('#relay-password').inputValue(),'correct horse','the typed draft survives a different choice until it is cleared');await page.locator('#relay-password').fill('');
    await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();
    assert.match(await text('.confirm-dialog'),/Connect to Hotel Guest\?/);assert.match(await text('.confirm-dialog'),/briefly reconnect/);await confirm('Connect');
    await said('#relay-status .relay-state','WI-FI LINKED');
    assert.deepEqual(last().body,{network:'hotel5',password:'',confirmed:true,ipv6:true,remember:true});
    assert.match(await text('#relay-status h3'),/Hotel Guest/);const facts=await text('#relay-status .relay-facts');
    for(const word of ['192.168.8.23','192.168.8.1','5 GHz · 80 MHz · stable','Auto tuning','Configured \\(internet not verified\\)','2001:db8::2','fd00:1::/64','Automatic reconnect enabled'])assert.match(facts,new RegExp(word));
    assert.equal(await page.getByRole('button',{name:'Disconnect relay'}).isVisible(),true);assert.match(await text('#relay-networks .action-status'),/Internet is not verified/);
    assert.match(await text('#relay-saved .relay-saved-item:has-text("Hotel Guest")'),/Connected/);

    // Disconnect is confirmed; the page returns to the off state.
    await page.getByRole('button',{name:'Disconnect relay'}).click();assert.match(await text('.confirm-dialog'),/restored/);await confirm('Disconnect relay');
    await said('#relay-status .relay-state','RELAY OFF');assert.equal(last().url,'/api/wifi-relay/disconnect');

    // Open networks never send a password; options map onto the request.
    await pick('Cafe');assert.equal(await page.locator('#relay-password').isDisabled(),true);assert.equal(await page.locator('input[name=remember]').isDisabled(),true);
    await page.locator('#relay-width').selectOption('off');await page.locator('input[name=ipv6]').uncheck();
    await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();await confirm('Connect');await said('#relay-status .relay-state','WI-FI LINKED');
    assert.deepEqual(last().body,{network:'cafe24',password:'',confirmed:true,ipv6:false,remember:false,width_mode:'off'});
    assert.match(await text('#relay-status .relay-facts'),/Off/);

    // Saved networks: bandwidth and forgetting are explicit, confirmed changes.
    const item=page.locator('#relay-saved .relay-saved-item:has-text("Hotel Guest")');
    assert.equal(await item.getByRole('button',{name:'Save bandwidth'}).isDisabled(),true,'unchanged');
    await item.locator('select').selectOption('off');await item.getByRole('button',{name:'Save bandwidth'}).click();await confirm('Save bandwidth');
    await said('#relay-saved .action-status','Bandwidth choice saved');assert.deepEqual(last().body,{id:'k-hotel',width_mode:'off',confirmed:true});
    await page.locator('#relay-saved .relay-saved-item:has-text("Cafe")').getByRole('button',{name:'Forget'}).click();assert.match(await text('.confirm-dialog'),/active relay stays connected/);await confirm('Forget network');
    await said('#relay-saved .action-status','Network forgotten');assert.deepEqual(last().body,{id:'k-cafe'});assert.equal(await page.locator('.relay-saved-item').count(),1);

    // Time-not-verified banner and its explicit recheck.
    await pick('Home');await page.locator('#relay-password').fill('correct horse');await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();await confirm('Connect');
    await page.locator('.relay-clock').waitFor({state:'visible'});assert.match(await text('.relay-clock'),/time not verified/);
    await page.getByRole('button',{name:'Recheck time'}).click();await said('#relay-status .action-status','Time verified');await page.locator('.relay-clock').waitFor({state:'hidden'});
    assert.equal(await page.locator('#relay-password').inputValue(),'','the password is cleared after a successful connect');

    // A restore in progress is the only thing that polls.
    status={...off,restoring:'Hotel Guest',autostart:true,networks:scan,saved_networks:saved};restoringPolls=0;
    await page.getByRole('button',{name:'Read current status'}).click();await said('#relay-status .relay-state','RECONNECTING');
    assert.match(await text('#relay-status h3'),/Hotel Guest/);assert.equal(await page.getByRole('button',{name:'Stop reconnecting'}).isVisible(),true);
    await page.waitForFunction(()=>true);await page.waitForTimeout(5600);assert.ok(restoringPolls>=2,`a pending restore refreshes itself (${restoringPolls})`);
    status={...off,networks:scan,saved_networks:saved};await page.waitForTimeout(5300);await said('#relay-status .relay-state','RELAY OFF');
    const quiet=fixture.requests.length;await page.waitForTimeout(1200);assert.equal(fixture.requests.length,quiet,'polling stops once the restore ends');

    // A request that never answers is reported as uncertain, then the state is re-read.
    status={...off,networks:scan,saved_networks:saved};await page.getByRole('button',{name:'Read current status'}).click();await said('#relay-status .relay-state','RELAY OFF');
    await pick('Hotel');hang=true;await page.getByRole('button',{name:'Connect to Wi-Fi'}).click();await confirm('Connect');
    await said('#relay-networks .action-status','did not answer');assert.equal(await page.locator('#relay-networks .action-status').getAttribute('data-error'),'true');
    await said('#relay-status .action-status','re-read');await said('#relay-status .relay-state','WI-FI LINKED');

    for(const theme of ['light','dark'])for(const width of [320,390,820,1440]){
      await page.setViewportSize({width,height:1200});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=document.documentElement.clientWidth),true,`${theme} ${width}px has no horizontal overflow`);
    }
    assert.deepEqual(errors,[]);
    console.log('Passed: relay states, scan list, password checks, connect/disconnect, saved networks, restore polling, uncertain requests and responsive themes.');
  }finally{await browser.close();await fixture.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
