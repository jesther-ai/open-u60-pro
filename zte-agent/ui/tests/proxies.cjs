const assert=require('node:assert/strict'),{chromium}=require('playwright'),{start}=require('./fixture.cjs');
(async()=>{
  let running=false,selectFails=null,modeFails=null,inflight=0,peak=0;
  const nodes={'HK 01':{type:'Trojan',delay:88},'US 02 <i>x</i>':{type:'Vmess',delay:0},'JP 03':{type:'Vless',delay:640},DIRECT:{type:'Direct',delay:null}};
  const delays={'HK 01':41,'US 02 <i>x</i>':0,'JP 03':390,DIRECT:12};
  const listing=()=>({mode:'rule',groups:[
    {name:'Auto',type:'URLTest',now:'HK 01',selectable:false,all:['HK 01','JP 03']},
    {name:'Proxy',type:'Selector',now:'Auto',selectable:true,all:['Auto','HK 01','US 02 <i>x</i>','JP 03','DIRECT']},
    {name:'GLOBAL',type:'Selector',now:'Proxy',selectable:true,all:['Proxy','Auto','DIRECT']}
  ],nodes:JSON.parse(JSON.stringify(nodes))});
  let state=listing();
  const fixture=await start(({method,url,body},reply,fail)=>{
    if(!url.startsWith('/api/mihomo/'))return false;
    if(method==='GET'&&url==='/api/mihomo/proxies'){running?reply(state):fail('Clash is off',409);return true;}
    if(method!=='POST')return false;
    if(!running){fail('Clash is off',409);return true;}
    if(url==='/api/mihomo/dashboard'){reply({url:'/ui/mihomo/dashboard/#/proxies'});return true;}
    if(url==='/api/mihomo/mode'){if(modeFails){fail(modeFails,503);return true;}state.mode=body.mode;reply({mode:body.mode});return true;}
    if(url==='/api/mihomo/proxies/select'){if(selectFails){fail(selectFails,503);return true;}state.groups.find(g=>g.name===body.group).now=body.name;reply({group:body.group,now:body.name});return true;}
    if(url==='/api/mihomo/proxies/delay'){inflight++;peak=Math.max(peak,inflight);setTimeout(()=>{inflight--;reply({name:body.name,delay:delays[body.name]});},60);return true;}
    return false;
  });
  const browser=await chromium.launch({headless:true,channel:'chrome'}),page=await browser.newPage({viewport:{width:1440,height:1500}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  const text=sel=>page.locator(sel).textContent(),posts=()=>fixture.requests.filter(r=>r.method==='POST'),last=()=>posts().at(-1);
  const said=(sel,words)=>page.waitForFunction(([sel,words])=>document.querySelector(sel)?.textContent.includes(words),[sel,words]);
  const confirm=async label=>{await page.locator('.confirm-dialog').getByRole('button',{name:label,exact:true}).click();await page.locator('.confirm-dialog').waitFor({state:'hidden'});};
  const group=name=>page.locator(`.proxy-group[data-group="${name}"]`);
  try{
    // Clash is off: say so, point to where it starts, and change nothing.
    await page.goto(`${fixture.url}/#network/proxies`);await said('#proxies-groups','Clash is off');
    assert.equal(await page.getByRole('button',{name:'Open Zashboard',exact:true}).isHidden(),true,'nothing to open while Clash is off');
    assert.equal(await page.getByRole('button',{name:'Open Clash / Mihomo'}).isVisible(),true);assert.equal(await page.locator('#proxies-mode-select').isDisabled(),true);assert.equal(posts().length,0);
    await page.getByRole('button',{name:'Open Clash / Mihomo'}).click();await page.waitForFunction(()=>location.hash==='#network/mihomo');
    await page.goto(`${fixture.url}/#network/proxies`);

    // Running: groups, the current node and the last measured delays.
    running=true;await page.getByRole('button',{name:'Read current state'}).click();await page.locator('.proxy-group').first().waitFor();
    assert.equal(await page.getByRole('button',{name:'Open Zashboard',exact:true}).isVisible(),true);
    const [pane]=await Promise.all([page.waitForEvent('popup'),page.getByRole('button',{name:'Open Zashboard',exact:true}).click()]);
    await pane.waitForURL(`${fixture.url}/ui/mihomo/dashboard/#/proxies`);await said('#proxies-mode .action-status','Zashboard is open in a new tab');await pane.close();
    assert.equal(await page.locator('.proxy-group').count(),3);assert.equal(await page.getByRole('button',{name:'Open Clash / Mihomo'}).isHidden(),true);
    assert.match(await group('Auto').textContent(),/Chosen automatically by Clash/);assert.equal(await group('Auto').locator('select').count(),0);
    assert.match(await group('Proxy').textContent(),/Now: Auto → HK 01/);assert.match(await group('Proxy').locator('.proxy-delay').textContent(),/88 ms/);
    assert.equal(await group('Proxy').locator('select').inputValue(),'Auto');
    assert.deepEqual(await group('Proxy').locator('option').allTextContents(),['Auto','HK 01 · 88 ms','US 02 <i>x</i> · Timeout','JP 03 · 640 ms','DIRECT · Untested']);
    assert.equal(await page.locator('.proxy-group i').count(),0,'node names are never parsed as HTML');
    assert.equal(await page.locator('#proxies-mode-select').inputValue(),'rule');
    const idle=fixture.requests.length;await page.waitForTimeout(1200);assert.equal(fixture.requests.length,idle,'the proxies page does not poll');

    const unchanged=posts().length;
    await page.locator('#proxies-mode-select').selectOption('direct');await confirm('Cancel');
    assert.equal(posts().length,unchanged,'cancelling mode confirmation sends nothing');
    assert.equal(await page.locator('#proxies-mode-select').inputValue(),'rule');

    // Choosing a node is a single explicit request; a refusal puts the choice back.
    await group('Proxy').locator('select').selectOption('JP 03');await confirm('Switch node');await said('#proxies-groups .action-status','Proxy now uses JP 03');
    assert.deepEqual(last().body,{group:'Proxy',name:'JP 03'});assert.match(await group('Proxy').textContent(),/Now: JP 03/);
    selectFails='Selector not found';await group('Proxy').locator('select').selectOption('HK 01');await confirm('Switch node');await said('#proxies-groups .action-status','Selector not found');
    assert.equal(await page.locator('#proxies-groups .action-status').getAttribute('data-error'),'true');assert.equal(await group('Proxy').locator('select').inputValue(),'JP 03','the select returns to the node in use');selectFails=null;

    // Delay tests: one node, then a group at limited parallelism; groups inside the list are skipped.
    await group('Proxy').getByRole('button',{name:'Test delay'}).click();await said('#proxies-groups .action-status','Tested 1 node');
    assert.deepEqual(last().body,{name:'JP 03'});assert.match(await group('Proxy').locator('.proxy-delay').textContent(),/390 ms/);
    const before=posts().length;peak=0;await group('Proxy').getByRole('button',{name:'Test group'}).click();await said('#proxies-groups .action-status','Tested 4 nodes');
    assert.equal(posts().length-before,4,'Auto is a group, so only the four nodes are tested');assert.ok(peak<=3,`at most three tests in flight (${peak})`);
    assert.deepEqual(posts().slice(before).map(r=>r.body.name).sort(),['DIRECT','HK 01','JP 03','US 02 <i>x</i>']);
    assert.deepEqual(await group('Proxy').locator('option').allTextContents(),['Auto','HK 01 · 41 ms','US 02 <i>x</i> · Timeout','JP 03 · 390 ms','DIRECT · 12 ms']);
    assert.match(await group('GLOBAL').locator('.proxy-delay').textContent(),/390 ms/,'groups sharing a node show the same fresh delay');

    // The routing mode, and a refusal that leaves it as it was.
    await page.locator('#proxies-mode-select').selectOption('global');await confirm('Change mode');await said('#proxies-mode .action-status','Mode is now Global');assert.deepEqual(last().body,{mode:'global'});
    modeFails='Mihomo refused the request (HTTP 400)';await page.locator('#proxies-mode-select').selectOption('direct');await confirm('Change mode');await said('#proxies-mode .action-status','refused the request');
    assert.equal(await page.locator('#proxies-mode-select').inputValue(),'global');modeFails=null;

    // Filtering is local and finds groups by their members.
    await page.locator('#proxies-search').fill('jp 03');assert.deepEqual(await page.locator('.proxy-group h3').allTextContents(),['Auto','Proxy'],'only the groups that list JP 03 remain; GLOBAL does not');
    await page.locator('#proxies-search').fill('direct');assert.equal(await page.locator('.proxy-group').count(),2);
    await page.locator('#proxies-search').fill('nothing like this');assert.match(await text('#proxies-groups'),/No group or node matches/);await page.locator('#proxies-search').fill('');

    // Clash turned off meanwhile: the page says so instead of failing quietly.
    running=false;await page.locator('#proxies-mode-select').selectOption('rule');await confirm('Change mode');await said('#proxies-groups','Clash is off');

    for(const theme of ['light','dark'])for(const width of [320,390,820,1440]){
      await page.setViewportSize({width,height:1400});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=document.documentElement.clientWidth),true,`${theme} ${width}px has no horizontal overflow`);
    }
    assert.deepEqual(errors,[]);
    console.log('Passed: proxy groups, node choice with rollback, bounded delay tests, routing mode, filtering, Clash-off handling and responsive themes.');
  }finally{await browser.close();await fixture.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
