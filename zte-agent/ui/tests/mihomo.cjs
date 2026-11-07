const assert=require('node:assert/strict'),{chromium}=require('playwright'),{start}=require('./fixture.cjs');
(async()=>{
  let status={installed:false,configured:false,running:false,version:'1.19.33',dashboard_version:'3.29.1',dashboard:null,scope:'lan_tcp_udp_dual_stack_tun',experimental:true,error:null,autostart:false,recovery_required:false};
  let library={selected:null,profiles:[],merge_enabled:false,script_enabled:false,merge_saved:false,script_saved:false};
  const editors={},extensions={merge_enabled:false,script_enabled:false,merge:'',script:''};
  let switchFails=null,nextId=1,dashboard=()=>({url:'/ui/mihomo/dashboard/#/proxies'}),dashboardFails=null;
  const sync=()=>{status={...status,configured:!!library.selected};};
  const fixture=await start(({method,url,body},reply,fail)=>{
    if(!url.startsWith('/api/mihomo/'))return false;
    if(method==='GET'){
      if(url==='/api/mihomo/status'){sync();reply(status);return true;}
      if(url==='/api/mihomo/profiles'){reply(library);return true;}
      if(url==='/api/mihomo/extensions'){reply(extensions);return true;}
      const edit=url.match(/^\/api\/mihomo\/profiles\/([0-9a-f]+)$/);if(edit){editors[edit[1]]?reply(editors[edit[1]]):fail('Unknown profile',404);return true;}
      return false;
    }
    if(url==='/api/mihomo/dashboard'){if(dashboardFails){fail(...dashboardFails);return true;}reply(dashboard());return true;}
    if(url==='/api/mihomo/install'){status={...status,installed:true};reply(status);return true;}
    if(url==='/api/mihomo/switch'){if(switchFails){fail(switchFails);return true;}status={...status,running:body.enabled,autostart:body.enabled,recovery_required:false};sync();reply(status);return true;}
    if(url==='/api/mihomo/profiles'){
      const id=body.id||`a${String(nextId).padStart(3,'0')}`;
      if(body.action==='add'||body.action==='edit'){
        const remote=body.source_type==='remote',profile={id:body.action==='add'?`a${String(nextId++).padStart(3,'0')}`:body.id,name:body.name,remote,interval_hours:body.interval_hours,updated:Math.floor(Date.now()/1000)-5,attempted:0,error:null};
        editors[profile.id]={id:profile.id,name:body.name,url:remote?body.url:null,source_type:remote?'remote':'local',interval_hours:body.interval_hours};
        library={...library,profiles:body.action==='add'?[...library.profiles,profile]:library.profiles.map(p=>p.id===profile.id?profile:p),selected:library.selected||profile.id};
      }else if(body.action==='select'){library={...library,selected:id};}
      else if(body.action==='refresh'){library={...library,profiles:library.profiles.map(p=>p.id===id?{...p,updated:Math.floor(Date.now()/1000)}:p)};}
      else if(body.action==='delete'){library={...library,profiles:library.profiles.filter(p=>p.id!==id),selected:library.selected===id?null:library.selected};}
      else if(body.action==='extensions'){Object.assign(extensions,body.extensions);library={...library,merge_enabled:body.extensions.merge_enabled,script_enabled:body.extensions.script_enabled,merge_saved:!!body.extensions.merge,script_saved:!!body.extensions.script};}
      reply(library);return true;
    }
    return false;
  });
  const browser=await chromium.launch({headless:true,channel:'chrome'}),page=await browser.newPage({viewport:{width:1440,height:1500}}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  const text=sel=>page.locator(sel).textContent(),posts=()=>fixture.requests.filter(r=>r.method==='POST'),last=()=>posts().at(-1);
  const said=(sel,words)=>page.waitForFunction(([sel,words])=>document.querySelector(sel)?.textContent.includes(words),[sel,words]);
  const confirm=label=>page.locator('.confirm-dialog').getByRole('button',{name:label,exact:true}).click();
  const button=name=>page.getByRole('button',{name,exact:true});
  const url='https://sub.example.com/clash?token=very-private-token';
  try{
    // Nothing is installed: the page says what comes first and starts no work.
    await page.goto(`${fixture.url}/#network/mihomo`);await said('#mihomo-status .mihomo-state','OFF');
    assert.match(await text('#mihomo-status h3'),/Clash is off/);assert.match(await text('#mihomo-status'),/Install the pinned Mihomo core first/);
    assert.equal(await button('Start Clash').isDisabled(),true);assert.match(await text('#mihomo-status'),/Install the core first\./);
    assert.equal(await button('Open Zashboard').isHidden(),true,'no dashboard while Clash is off');
    assert.equal(await button('Add configuration').isDisabled(),true);assert.match(await text('#mihomo-profiles'),/Install the Mihomo core to add configuration profiles/);
    assert.match(await text('#mihomo-status .mihomo-facts'),/Not installed/);assert.equal(posts().length,0,'opening the page must not start work');
    const idle=fixture.requests.length;await page.waitForTimeout(1200);assert.equal(fixture.requests.length,idle,'an idle Clash page does not poll');

    // Install is an explicit, confirmed download.
    await button('Install Mihomo core').click();assert.match(await text('.confirm-dialog'),/22 MB/);assert.match(await text('.confirm-dialog'),/cellular data/);await confirm('Install');
    await said('#mihomo-status .action-status','Core installed');assert.equal(last().url,'/api/mihomo/install');
    assert.match(await text('#mihomo-status .mihomo-facts'),/Installed · 1\.19\.33/);assert.equal(await button('Reinstall pinned core').isVisible(),true);
    assert.equal(await button('Start Clash').isDisabled(),true);assert.match(await text('#mihomo-status'),/Add a configuration profile first/);

    // The profile form validates before anything is sent.
    await button('Add configuration').click();assert.equal(await page.locator('#mihomo-profile-url').isVisible(),true);assert.equal(await page.locator('#mihomo-profile-yaml').isHidden(),true);
    await button('Validate & add').click();await said('#mihomo-profiles .action-status','profile name');
    await page.locator('#mihomo-profile-name').fill('Home');await page.locator('#mihomo-profile-url').fill('http://sub.example.com/x');await button('Validate & add').click();await said('#mihomo-profiles .action-status','HTTPS');
    await page.locator('#mihomo-profile-url').fill(url);await page.locator('#mihomo-profile-interval').selectOption('24');
    assert.equal(posts().filter(r=>r.url.endsWith('/profiles')).length,0);
    await button('Validate & add').click();assert.match(await text('.confirm-dialog'),/never turned on/);await confirm('Add profile');
    await said('#mihomo-profiles .action-status','Profile saved');
    assert.deepEqual(last().body,{action:'add',name:'Home',source_type:'remote',url,yaml:'',interval_hours:24});
    assert.equal(await page.locator('.mihomo-editor').first().isHidden(),true,'the form closes after saving');
    const card=page.locator('.mihomo-profile:has-text("Home")');assert.match(await card.textContent(),/Subscription · Every day/);assert.match(await card.textContent(),/Selected/);
    assert.doesNotMatch(await page.content(),/very-private-token/,'the subscription URL is not in the page until the editor is opened');
    assert.equal(await button('Start Clash').isDisabled(),false,'a selected profile makes Clash startable');

    // A pasted or imported YAML profile; oversized files are refused in the browser.
    await button('Add configuration').click();await page.locator('#mihomo-profile-name').fill('Pasted');await page.locator('#mihomo-profile-type').selectOption('local');
    assert.equal(await page.locator('#mihomo-profile-url').isHidden(),true);
    await page.locator('#mihomo-profile-file').setInputFiles({name:'huge.yaml',mimeType:'text/yaml',buffer:Buffer.alloc(512*1024+1,97)});await said('#mihomo-profiles .action-status','larger than 512 KiB');
    await page.locator('#mihomo-profile-file').setInputFiles({name:'ok.yaml',mimeType:'text/yaml',buffer:Buffer.from('rules:\n  - MATCH,DIRECT\n')});
    await page.waitForFunction(()=>document.querySelector('#mihomo-profile-yaml').value.includes('MATCH,DIRECT'));
    await button('Validate & add').click();await confirm('Add profile');await said('#mihomo-profiles .action-status','Profile saved');
    assert.deepEqual(last().body,{action:'add',name:'Pasted',source_type:'local',url:'',yaml:'rules:\n  - MATCH,DIRECT\n',interval_hours:0});

    // Editing reads the private editor on demand and keeps a local profile’s saved YAML unless replaced.
    await page.locator('.mihomo-profile:has-text("Home")').getByRole('button',{name:'Edit'}).click();await page.waitForFunction(()=>document.querySelector('#mihomo-profile-url').value.includes('very-private-token'));
    assert.equal(fixture.requests.at(-1).url,'/api/mihomo/profiles/a001');assert.equal(await page.locator('#mihomo-profile-interval').inputValue(),'24');
    await page.locator('#mihomo-profile-name').fill('Home 2');await button('Validate & save changes').click();await confirm('Save profile');await said('#mihomo-profiles .action-status','Profile saved');
    assert.deepEqual(last().body,{action:'edit',id:'a001',name:'Home 2',source_type:'remote',url,yaml:'',interval_hours:24});
    await page.locator('.mihomo-profile:has-text("Pasted")').getByRole('button',{name:'Edit'}).click();await page.waitForFunction(()=>document.querySelector('#mihomo-profile-name').value==='Pasted');
    assert.match(await page.locator('#mihomo-profile-yaml').getAttribute('placeholder'),/keep the saved YAML/);
    await button('Validate & save changes').click();await confirm('Save profile');await said('#mihomo-profiles .action-status','Profile saved');assert.equal(last().body.yaml,'');

    // Start: confirmed, then every profile action warns about the reconnect.
    await button('Start Clash').click();assert.match(await text('.confirm-dialog'),/not a kill switch/);await confirm('Start Clash');
    await said('#mihomo-status .mihomo-state','RUNNING');assert.deepEqual(last().body,{enabled:true});
    assert.match(await text('#mihomo-status h3'),/Clash is running/);assert.equal(await page.getByRole('button',{name:/Install|Reinstall/}).count(),0,'no install while running');
    assert.match(await text('#mihomo-status .mihomo-facts'),/Automatic start enabled/);

    // Zashboard opens in its own tab and the console stays where it was.
    const popupFor=async()=>{const [popup]=await Promise.all([page.waitForEvent('popup'),button('Open Zashboard').click()]);return popup;};
    const pane=await popupFor();await pane.waitForURL(`${fixture.url}/ui/mihomo/dashboard/#/proxies`);
    await said('#mihomo-status .action-status','Zashboard is open in a new tab');assert.equal(last().url,'/api/mihomo/dashboard');assert.deepEqual(last().body,{});
    assert.equal(await pane.evaluate(()=>window.opener),null,'the new tab has no handle on the console');await pane.close();assert.equal(page.url(),`${fixture.url}/#network/mihomo`);
    dashboard=()=>({url:'https://evil.example/ui/mihomo/dashboard/'});
    const blankClosed=async()=>{const gone=new Promise(resolve=>page.once('popup',popup=>popup.isClosed()?resolve():popup.once('close',resolve)));await button('Open Zashboard').click();await gone;};
    await blankClosed();
    await said('#mihomo-status .action-status','unexpected dashboard address');assert.equal(await page.locator('#mihomo-status .action-status').getAttribute('data-error'),'true');
    dashboard=()=>({url:'/ui/mihomo/dashboard/#/proxies'});dashboardFails=['Clash is off',409];
    await blankClosed();
    await said('#mihomo-status .action-status','Clash is off. Start it first.');dashboardFails=null;
    await page.evaluate(()=>{window.__open=window.open;window.open=()=>null;});
    await button('Open Zashboard').click();await said('#mihomo-status .action-status','blocked the new tab');
    const link=page.locator('#mihomo-status .action-status a');assert.deepEqual(await link.evaluate(a=>[a.getAttribute('href'),a.target,a.rel]),['/ui/mihomo/dashboard/#/proxies','_blank','noopener']);
    await page.evaluate(()=>{window.open=window.__open;});
    await page.locator('.mihomo-profile:has-text("Pasted")').getByRole('button',{name:'Use profile'}).click();assert.match(await text('.confirm-dialog'),/briefly reconnects/);await confirm('Use profile');
    await said('#mihomo-profiles .action-status','Profile selected');assert.deepEqual(last().body,{action:'select',id:'a002'});
    await page.locator('.mihomo-profile:has-text("Home 2")').getByRole('button',{name:'Update now'}).click();await confirm('Update now');await said('#mihomo-profiles .action-status','Profile updated');assert.deepEqual(last().body,{action:'refresh',id:'a001'});
    await page.locator('.mihomo-profile:has-text("Pasted")').getByRole('button',{name:'Delete'}).click();assert.match(await text('.confirm-dialog'),/saved source and subscription URL are removed/);await confirm('Delete profile');
    await said('#mihomo-profiles .action-status','Profile deleted');assert.equal(await page.locator('.mihomo-profile').count(),1);

    assert.equal(await button('Open Zashboard').isVisible(),true);
    // A refusal from the router is shown as it is.
    switchFails='Mihomo reserved firewall chains are occupied';await button('Stop Clash').click();await confirm('Stop Clash');
    await said('#mihomo-status .action-status','chains are occupied');assert.equal(await page.locator('#mihomo-status .action-status').getAttribute('data-error'),'true');switchFails=null;
    await button('Stop Clash').click();await confirm('Stop Clash');await said('#mihomo-status .mihomo-state','OFF');

    // Global Merge & Script load on demand and validate every profile on save.
    assert.equal(fixture.requests.filter(r=>r.url==='/api/mihomo/extensions').length,0,'private extension content is not fetched until requested');
    await button('Edit Merge & Script').click();await page.locator('#mihomo-merge').waitFor({state:'visible'});
    await page.locator('#mihomo-merge').fill('dns:\n  enable: true');await page.getByLabel('Apply Merge to every profile').check();
    await button('Validate all & save').click();assert.match(await text('.confirm-dialog'),/re-validated/);await confirm('Validate all & save');await said('#mihomo-extensions .action-status','saved and applied');
    assert.deepEqual(last().body,{action:'extensions',extensions:{merge_enabled:true,script_enabled:false,merge:'dns:\n  enable: true',script:''}});assert.equal(await page.locator('#mihomo-merge').isHidden(),true);

    // A leftover restart choice and a failed cleanup have their own recovery actions.
    status={...status,running:false,autostart:true,recovery_required:false};await page.getByRole('button',{name:'Read current status'}).click();
    await button('Stop automatic start & keep off').waitFor({state:'visible'});await button('Stop automatic start & keep off').click();await confirm('Keep off');await said('#mihomo-status .action-status','automatic start cleared');
    status={...status,running:false,autostart:true,recovery_required:true,error:'Network cleanup paused: busy'};await page.getByRole('button',{name:'Read current status'}).click();
    await said('#mihomo-status .mihomo-state','RECOVERY REQUIRED');assert.equal(await button('Retry proxy cleanup').isVisible(),true);assert.equal(await page.getByRole('button',{name:/Start Clash/}).isHidden(),true);
    assert.match(await text('#mihomo-status .relay-problem'),/cleanup paused/);assert.equal(await button('Add configuration').isDisabled(),true);

    for(const theme of ['light','dark'])for(const width of [320,390,820,1440]){
      await page.setViewportSize({width,height:1400});await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);
      assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=document.documentElement.clientWidth),true,`${theme} ${width}px has no horizontal overflow`);
    }
    assert.deepEqual(errors,[]);
    console.log('Passed: Clash install and start, profile validation/add/edit/select/update/delete, private editors on demand, extensions, Zashboard in its own tab, recovery actions, no idle polling and responsive themes.');
  }finally{await browser.close();await fixture.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
