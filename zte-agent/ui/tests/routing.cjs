const assert=require('node:assert/strict');
const http=require('node:http');
const fs=require('node:fs');
const path=require('node:path');
const {chromium}=require('playwright');
(async()=>{
  const streams=new Set(),requests=[];
  const lan={lan_ipaddr:'192.168.0.1',lan_netmask:'255.255.255.0',dhcp_enable:'1',dhcp_start:'100',dhcp_end:'192.168.0.149',dhcp_lease_time:'12h'};
  const firewall={firewall_switch:'1',firewall_level:'medium',nat_switch:'1',dmz_enabled:'0',dmz_ip:'',port_forward_switch:'1'};
  const upnp={upnp_switch:'0'},qos={qos_switch:'0'};
  let rules=[{name:'Home server',protocol:'tcp',wan_port:'8443',lan_ip:'192.168.0.10',lan_port:'443',enabled:'1'},{name:'Console',protocol:'udp',wan_port:'3074',lan_ip:'192.168.0.11',lan_port:'3074',enabled:'1'}];
  let clients={hosts:{},dhcp_leases:[{hostname:'Home server',ipaddr:'192.168.0.10',macaddr:'aa:bb:cc:dd:ee:01'}]};
  let failRules=false,failLAN=false,failQoS=false,ignoreWrite=false,loseAddReply=false,authExpired=false,postGuardRace=false;
  const envelope=data=>({ok:true,data});
  const readings=topic=>({modem:envelope({operate_mode:'ONLINE'}),signal:envelope({network_type:'LTE',rsrp:'-88',sinr:'12'}),clients:envelope(clients),...(topic==='routing'?{
    lan_config:failLAN?{ok:false,error:'LAN unavailable'}:envelope(lan),firewall_config:envelope(firewall),upnp_config:envelope(upnp),port_rules:failRules?{ok:false,error:'List unavailable'}:envelope({rule_list:rules}),qos_config:failQoS?{ok:false,error:'QoS unsupported'}:envelope(qos)
  }:{})});
  const frame=(res,event)=>res.write(`event: ${event}\ndata: ${JSON.stringify({readings:readings(res.topic)})}\n\n`);
  const push=()=>{for(const res of streams)frame(res,'patch');};
  const send=(res,data,status=200)=>{res.writeHead(status,{'Content-Type':'application/json'});res.end(JSON.stringify(status<300?{ok:true,data}:{ok:false,error:data}));};
  const server=http.createServer((req,res)=>{
    if(!req.url.startsWith('/api/')){
      const file=req.url==='/'?'index.html':/^\/(?:ui|console\/a)\/[\w.-]+$/.test(req.url)?req.url.split('/').pop():'',full=path.join(__dirname,'..',file);
      if(!file||!fs.existsSync(full)){res.writeHead(404);res.end();return;}
      res.setHeader('Content-Type',file.endsWith('.css')?'text/css':file.endsWith('.js')?'text/javascript':'text/html');res.setHeader('Content-Security-Policy',"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'");res.end(fs.readFileSync(full));return;
    }
    if(authExpired){send(res,'unauthorized',401);return;}
    if(req.url.startsWith('/api/dashboard/events')){requests.push({url:req.url,method:'GET'});res.topic=new URL(req.url,'http://localhost').searchParams.get('topics');res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));frame(res,'snapshot');return;}
    let raw='';req.on('data',chunk=>raw+=chunk);req.on('end',()=>{
      const body=raw?JSON.parse(raw):null;requests.push({url:req.url,method:req.method,body});
      if(req.method==='GET'){
        if(req.url==='/api/router/lan'){send(res,failLAN?'LAN unavailable':lan,failLAN?503:200);return;}
        if(req.url==='/api/router/firewall'){send(res,firewall);return;}
        if(req.url==='/api/router/firewall/upnp'){send(res,upnp);return;}
        if(req.url==='/api/router/qos'){send(res,failQoS?'QoS unsupported':qos,failQoS?503:200);return;}
        if(req.url==='/api/router/firewall/port-forward'){send(res,failRules?'List unavailable':{rule_list:rules},failRules?503:200);return;}
      }
      if(req.url==='/api/router/firewall/port-forward'){
        if(body.action==='add'){
          if(!ignoreWrite){const {action,...rule}=body;rules.push(rule);}
          if(loseAddReply){res.writeHead(200,{'Content-Type':'application/json'});res.end('{"ok":');setTimeout(push,5);return;}
        }else{
          if(postGuardRace)rules.unshift({name:'Arrived later',protocol:'tcp',wan_port:'9000',lan_ip:'192.168.0.12',lan_port:'9000',enabled:'1'});
          if(JSON.stringify(body.expected_rules)!==JSON.stringify(rules)){send(res,'Port-forward rules changed. Review the current list before deleting.',409);setTimeout(push,5);return;}
          if(!ignoreWrite)rules=rules.filter((rule,index)=>String(rule.id??index)!==body.id);
        }
      }else if(!ignoreWrite){
        if(req.url==='/api/router/lan')Object.assign(lan,body);
        else if(req.url==='/api/router/qos')Object.assign(qos,body);
        else if(req.url==='/api/router/firewall/upnp')Object.assign(upnp,body);
        else Object.assign(firewall,body);
      }
      send(res,{result:0});setTimeout(push,5);
    });
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const heartbeat=setInterval(()=>{for(const res of streams)res.write(': heartbeat\n\n');},1000);
  const browser=await chromium.launch({headless:true,channel:'chrome'}),context=await browser.newContext({viewport:{width:1440,height:1100}}),page=await context.newPage(),errors=[];
  page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});page.on('dialog',dialog=>dialog.dismiss());
  const go=async key=>{await page.evaluate(key=>OpenU60Navigation.go(key),key);await page.waitForFunction(key=>document.documentElement.dataset.route===key,key);await page.waitForFunction(()=>document.querySelector('#stream-state').textContent==='Live');};
  const confirm=async label=>page.locator('.confirm-dialog').getByRole('button',{name:label,exact:true}).click();
  const status=async(id,text)=>{try{await page.waitForFunction(([id,text])=>document.querySelector(`#${id} > .action-status`).textContent.includes(text),[id,text]);}catch(error){console.error({id,text,actual:await page.locator(`#${id} > .action-status`).textContent(),errors,recent:requests.slice(-5)});await page.screenshot({path:'/tmp/zte-routing-failure.png'});throw error;}};
  const submit=id=>page.locator(`#${id} button[type=submit]`).click();
  const count=(url,method)=>requests.filter(item=>item.url===url&&(!method||item.method===method)).length;
  const last=(url,method)=>requests.filter(item=>item.url===url&&(!method||item.method===method)).at(-1);
  const writes=()=>requests.filter(item=>['POST','PUT','DELETE'].includes(item.method)).length;
  try{
    await page.goto(`http://127.0.0.1:${server.address().port}/#network/lan`);await status('lan-config','Current settings loaded');
    assert.equal(await page.locator('#lan-config-dhcp_start').inputValue(),'192.168.0.100');assert.equal(writes(),0);
    assert.match(await page.locator('#lan-overview').textContent(),/Reported leases/);
    await page.locator('#lan-config-dhcp_start').fill('192.168.0.1');await submit('lan-config');await status('lan-config','different from the router');assert.equal(writes(),0);
    await page.locator('#lan-config-dhcp_start').fill('192.168.0.110');await page.locator('#lan-config-dhcp_lease_time').fill('1d');
    clients.dhcp_leases.push({hostname:'<img src=x>',ipaddr:'192.168.0.20',macaddr:'aa:bb:cc:dd:ee:02'});push();await page.waitForFunction(()=>document.querySelectorAll('#lan-leases tbody tr').length===2);assert.equal(await page.locator('#lan-leases img').count(),0);assert.equal(await page.locator('#lan-config-dhcp_start').inputValue(),'192.168.0.110');
    await submit('lan-config');await confirm('Cancel');assert.equal(writes(),0);
    await submit('lan-config');await confirm('Save changes');await status('lan-config','Saved and confirmed');assert.deepEqual(last('/api/router/lan','PUT').body,{...lan});
    const streamsBefore=count('/api/dashboard/events?topics=routing');await go('network/firewall');await status('firewall-switch','Current settings loaded');await status('dmz-config','Current settings loaded');assert.equal(count('/api/dashboard/events?topics=routing'),streamsBefore,'routing pages share one subscription');
    for(const [id,key,value,url]of [['firewall-switch','firewall_switch','0','switch'],['firewall-level','firewall_level','high','level'],['nat-config','nat_switch','0','nat'],['upnp-config','upnp_switch','1','upnp']]){
      await page.locator(`#${id}-${key}`).selectOption(value);await submit(id);await confirm('Save changes');await status(id,'Saved and confirmed');assert.deepEqual(last(`/api/router/firewall/${url}`,'PUT').body,{[key]:value});
    }
    await page.locator('#dmz-config-dmz_enabled').selectOption('1');await page.locator('#dmz-config-dmz_ip').fill('10.0.0.2');await submit('dmz-config');await status('dmz-config','inside the LAN subnet');assert.equal(count('/api/router/firewall/dmz','PUT'),0);
    await page.locator('#dmz-config-dmz_ip').fill('192.168.0.10');await submit('dmz-config');await confirm('Save changes');await status('dmz-config','Saved and confirmed');assert.deepEqual(last('/api/router/firewall/dmz','PUT').body,{dmz_enabled:'1',dmz_ip:'192.168.0.10'});
    await page.locator('#upnp-config-upnp_switch').selectOption('0');upnp.upnp_switch='0';push();await status('upnp-config','unsaved draft is kept');const upnpWrites=count('/api/router/firewall/upnp','PUT');await submit('upnp-config');await confirm('Save changes');await status('upnp-config','changed elsewhere');assert.equal(count('/api/router/firewall/upnp','PUT'),upnpWrites);await page.locator('#upnp-config').getByRole('button',{name:'Discard edits',exact:true}).click();await page.locator('#upnp-config').getByRole('button',{name:'Read current settings',exact:true}).click();await status('upnp-config','Current settings loaded');
    await go('network/ports');await page.waitForFunction(()=>document.querySelectorAll('#port-rules .rule-card').length===2);await status('ports-switch','Current settings loaded');
    await page.locator('#ports-switch-port_forward_switch').selectOption('0');await submit('ports-switch');await confirm('Save changes');await status('ports-switch','Saved and confirmed');assert.deepEqual(last('/api/router/firewall/port-forward/switch','PUT').body,{port_forward_switch:'0'});
    const fillRule=async(values={})=>{for(const [key,value]of Object.entries({name:'SSH',protocol:'tcp',wan_port:'2222',lan_ip:'192.168.0.20',lan_port:'22',enabled:'1',...values})){const input=page.locator(`#port-rule-${key}`);if(['protocol','enabled'].includes(key))await input.selectOption(value);else await input.fill(value);}};
    await fillRule({wan_port:'8443'});await submit('port-rules');await status('port-rules','already belong');
    await fillRule();await go('network/qos');await status('qos-config','Current settings loaded');await go('network/ports');assert.equal(await page.locator('#port-rule-name').inputValue(),'SSH');
    await submit('port-rules');await confirm('Add rule');await status('port-rules','added and confirmed');assert.deepEqual(last('/api/router/firewall/port-forward','POST').body,{action:'add',name:'SSH',protocol:'tcp',wan_port:'2222',lan_ip:'192.168.0.20',lan_port:'22',enabled:'1'});assert.equal(await page.locator('#port-rule-name').inputValue(),'');
    await page.locator('#port-rules').getByRole('button',{name:'Delete Home server',exact:true}).click();rules.reverse();push();await confirm('Delete rule');await status('port-rules','rule list changed');assert.equal(requests.filter(item=>item.body?.action==='delete').length,0);
    await page.locator('#port-rules').getByRole('button',{name:'Delete Home server',exact:true}).click();await confirm('Delete rule');await status('port-rules','removed and confirmed');assert.equal(last('/api/router/firewall/port-forward','POST').body.expected_rules.length,3);assert.equal(rules.some(rule=>rule.name==='Home server'),false);
    postGuardRace=true;await page.locator('#port-rules').getByRole('button',{name:'Delete Console',exact:true}).click();await confirm('Delete rule');await status('port-rules','rules changed');assert.equal(rules.some(rule=>rule.name==='Console'),true);postGuardRace=false;push();await page.locator('#port-rules').getByRole('button',{name:'Read current rules',exact:true}).click();await status('port-rules','Current rules loaded');
    loseAddReply=true;await fillRule({name:'Uncertain response',wan_port:'2223'});const beforeLost=count('/api/router/firewall/port-forward','POST');await submit('port-rules');await confirm('Add rule');await status('port-rules','could not be confirmed');await page.waitForTimeout(400);assert.equal(count('/api/router/firewall/port-forward','POST'),beforeLost+1);assert.equal(await page.locator('#port-rule-name').inputValue(),'Uncertain response');loseAddReply=false;
    await page.locator('#port-rules').getByRole('button',{name:'Discard draft',exact:true}).click();
    failRules=true;push();await page.waitForFunction(()=>document.querySelector('#port-rules fieldset').disabled);assert.ok(await page.locator('#port-rules .rule-card').count()>0,'failed reads retain last rules');failRules=false;push();await page.waitForFunction(()=>!document.querySelector('#port-rules fieldset').disabled);
    await go('network/qos');await page.locator('#qos-config-qos_switch').selectOption('1');await submit('qos-config');await confirm('Save changes');await status('qos-config','Saved and confirmed');assert.deepEqual(last('/api/router/qos','PUT').body,{qos_switch:'1'});
    ignoreWrite=true;await page.locator('#qos-config-qos_switch').selectOption('0');await submit('qos-config');await confirm('Save changes');await status('qos-config','not confirmed');assert.equal(await page.locator('#qos-config-qos_switch').inputValue(),'0');ignoreWrite=false;await page.locator('#qos-config').getByRole('button',{name:'Discard edits',exact:true}).click();
    await go('network/lan');await page.locator('#lan-config-lan_ipaddr').fill('192.168.5.1');await page.locator('#lan-config-dhcp_start').fill('192.168.5.100');await page.locator('#lan-config-dhcp_end').fill('192.168.5.149');await submit('lan-config');await confirm('Save changes');await status('lan-config','Saved and confirmed');assert.equal(await page.locator('.routing-reconnect a').getAttribute('href'),`http://192.168.5.1:${server.address().port}/#network/lan`);
    const readsBeforeIdle=requests.length;await page.waitForTimeout(1200);assert.equal(requests.length,readsBeforeIdle,'SSE causes no browser polling');
    for(const theme of ['light','dark'])for(const width of [320,390,820,1440]){
      await page.setViewportSize({width,height:1100});await go('settings/appearance');await page.locator(`input[name=appearance][value=${theme}]`).check();
      for(const route of ['network/lan','network/firewall','network/ports','network/qos']){
        await go(route);assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false,`${route} overflows ${theme} ${width}`);
        if(width===390 && route==='network/lan' && theme==='dark')await page.screenshot({path:'/tmp/zte-routing-lan-phone-dark.png',fullPage:true});
        if(width===1440 && route==='network/ports' && theme==='light')await page.screenshot({path:'/tmp/zte-routing-ports-desktop-light.png',fullPage:true});
      }
    }
    await go('network/ports');await page.evaluate(()=>scrollTo({top:0,behavior:'instant'}));rules.push({name:'Live change',protocol:'udp',wan_port:'40000',lan_ip:'192.168.5.25',lan_port:'40000',enabled:'1'});push();await page.waitForFunction(()=>document.querySelector('#ports-overview dd.animated-number')?.hasAttribute('data-number-animating'));assert.match(await page.locator('#ports-overview').textContent(),/Reported rules/);
    await go('network/lan');failLAN=true;push();await page.waitForFunction(()=>document.querySelector('#lan-overview .reading-caption').textContent.includes('unavailable'));assert.match(await page.locator('#lan-overview').textContent(),/192.168.5.1/);failLAN=false;
    await go('network/ports');for(const stream of streams)stream.destroy();streams.clear();await page.waitForFunction(()=>!OpenU60Console.connected && document.querySelector('#port-rules fieldset').disabled);await page.waitForFunction(()=>OpenU60Console.connected);assert.ok(await page.locator('#port-rules .rule-card').count()>0);
    failQoS=true;const unsupported=await context.newPage();await unsupported.goto(`http://127.0.0.1:${server.address().port}/#network/qos`);await unsupported.waitForFunction(()=>document.querySelector('#qos-config .action-status').textContent.includes('Settings unavailable'));assert.equal(await unsupported.locator('#qos-config fieldset').evaluate(node=>node.disabled),true);await unsupported.close();failQoS=false;push();
    authExpired=true;await go('network/qos');await page.locator('#qos-config').getByRole('button',{name:'Read current settings',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#login-dialog').open);assert.equal(await page.locator('#port-rule-name').inputValue(),'');assert.equal(await page.locator('.routing-reconnect a').getAttribute('href'),null);
    assert.deepEqual(errors,[]);
    console.log('Passed: all seven router controls, exact write contracts, validations, confirmation cancellation, LAN reconnect links, dirty/live conflicts, stale/guarded deletes, unconfirmed writes, no retry/polling, retained failures, auth clearing, digit motion and 32 responsive/theme views.');
  }finally{await browser.close();clearInterval(heartbeat);for(const stream of streams)stream.destroy();server.closeAllConnections();server.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
