const assert=require('node:assert/strict'),M=require('../mihomo-model.js');

// Lifecycle wording and which actions exist in each state.
let v=M.view({running:true,installed:true,configured:true,autostart:true});
assert.deepEqual([v.key,v.chip,v.title,v.toggleLabel,v.canStart,v.showInstall,v.showClearAutostart],['running','RUNNING','Clash is running','Stop Clash',true,false,false]);
v=M.view({running:false,installed:true,configured:true});
assert.deepEqual([v.key,v.chip,v.toggleLabel,v.canStart,v.showInstall,v.installLabel],['off','OFF','Start Clash',true,true,'Reinstall pinned core']);
v=M.view({running:false,installed:false,configured:false});
assert.deepEqual([v.canStart,v.toggleHint,v.installLabel],[false,'Install the core first.','Install Mihomo core']);
assert.equal(M.view({installed:true,configured:false}).toggleHint,'Add a configuration profile first.');
v=M.view({running:false,installed:true,configured:true,recovery_required:true,autostart:true});
assert.deepEqual([v.key,v.chip,v.canStart,v.showInstall,v.showClearAutostart,v.toggleHint],['recovery','RECOVERY REQUIRED',false,false,false,'Retry cleanup first.'],'recovery blocks start, install and the separate clear button');
v=M.view({running:false,installed:true,configured:true,autostart:true});
assert.equal(v.showClearAutostart,true,'an off core with a saved restart choice offers to clear it');
assert.equal(M.view({running:true,recovery_required:true}).key,'running');assert.equal(M.view(null).key,'off');
assert.deepEqual([true,false,undefined].map(autostart=>M.autostartLabel({autostart})),['Automatic start enabled','Stays off','Unknown']);
assert.deepEqual(M.facts({installed:true,version:'1.19.33',configured:true,autostart:false}).map(f=>f[1]),['Installed · 1.19.33','Saved privately','Stays off','LAN TCP/UDP · IPv4/IPv6']);
assert.equal(M.facts({}).find(f=>f[0]==='Core')[1],'Not installed');

// Profile freshness and schedule labels.
const now=1_800_000_000;
assert.equal(M.updatedLabel(0,now),'Never updated');assert.equal(M.updatedLabel(now-5,now),'Updated just now');assert.equal(M.updatedLabel(now-600,now),'Updated 10 min ago');
assert.equal(M.updatedLabel(now-7200,now),'Updated 2 h ago');assert.equal(M.updatedLabel(now-3*86400,now),'Updated 3 d ago');assert.equal(M.updatedLabel(now+500,now),'Updated just now','a future stamp from a corrected clock is not negative');
assert.equal(M.updatedLabel('x',now),'Never updated');assert.equal(M.updatedLabel(1e15,now),'Never updated');
assert.equal(M.intervalLabel(24),'Every day');assert.equal(M.intervalLabel(5),'Manual only');

// Subscription URLs follow the router’s rules.
for(const [url,ok] of [['https://example.com/sub?token=abc',true],['  https://example.com/a  ',true],['http://example.com/a',false],['file:///etc/passwd',false],['https://user:pw@example.com/',false],['https://example.com/a#frag',false],['',false],['not a url',false],['https://'+'a'.repeat(5000)+'.example',false]])
  assert.equal(M.subscriptionProblem(url)==='',ok,url.slice(0,60));

// Profile forms: names, sources and the saved-copy exception for editing.
const base={mode:'add',type:'remote',name:'Home',url:'https://example.com/sub',yaml:'',hasSavedYaml:false};
assert.equal(M.profileProblem(base),'');
assert.match(M.profileProblem({...base,name:'  '}),/profile name/);assert.match(M.profileProblem({...base,name:'x'.repeat(129)}),/profile name/);assert.match(M.profileProblem({...base,name:'bad\u0007name'}),/profile name/);
assert.equal(M.profileProblem({...base,name:'香'.repeat(42)}),'','42 three-byte characters fit in 128 bytes');assert.match(M.profileProblem({...base,name:'香'.repeat(43)}),/profile name/,'the limit counts bytes');
assert.match(M.profileProblem({...base,url:'http://x'}),/HTTPS/);
assert.match(M.profileProblem({...base,type:'local'}),/Paste or choose/);
assert.equal(M.profileProblem({...base,type:'local',yaml:'rules: []'}),'');
assert.equal(M.profileProblem({...base,type:'local',mode:'edit',hasSavedYaml:true}),'','editing a local profile may keep its saved YAML');
assert.match(M.profileProblem({...base,type:'local',mode:'edit',hasSavedYaml:false}),/Paste or choose/);
assert.match(M.profileProblem({...base,type:'local',yaml:'a'.repeat(M.MAX_CONFIG+1)}),/512 KiB/);assert.equal(M.profileProblem({...base,type:'local',yaml:'a'.repeat(M.MAX_CONFIG)}),'');
assert.match(M.profileProblem({...base,type:'both'}),/Choose/);

// Request bodies carry only the fields of the chosen source.
assert.deepEqual(M.profileRequest({...base,url:' https://example.com/sub ',interval:'24',name:' Home '}),{action:'add',name:'Home',source_type:'remote',url:'https://example.com/sub',yaml:'',interval_hours:24});
assert.deepEqual(M.profileRequest({mode:'edit',id:'0a1b',type:'local',name:'Pasted',url:'https://ignored',yaml:'rules: []',interval:'168'}),{action:'edit',id:'0a1b',name:'Pasted',source_type:'local',url:'',yaml:'rules: []',interval_hours:0});
assert.deepEqual(M.profileAction('select','0a1b'),{action:'select',id:'0a1b'});assert.deepEqual(M.scheduleRequest('0a1b','6'),{action:'schedule',id:'0a1b',interval_hours:6});assert.equal(M.scheduleRequest('x','nope').interval_hours,0);

// Confirmations mention the reconnect only when Clash is running.
assert.match(M.confirmation('select',{name:'Home'},true)[1],/briefly reconnects/);assert.doesNotMatch(M.confirmation('select',{name:'Home'},false)[1],/briefly reconnects/);
assert.equal(M.confirmation('delete',{name:'Home'},false)[0],'Delete Home?');assert.match(M.confirmation('add',{name:'Home'},false)[1],/cellular data is never turned on/);
for(const action of ['select','refresh','edit','add','delete'])assert.equal(M.confirmation(action,null,false).length,3);

// Global Merge and Script.
assert.equal(M.extensionsProblem({merge:'a'.repeat(M.MAX_EXTENSION),script:''}),'');assert.match(M.extensionsProblem({merge:'',script:'a'.repeat(M.MAX_EXTENSION+1)}),/128 KiB/);
assert.deepEqual(M.extensionsRequest({merge_enabled:1,script_enabled:true,merge:'dns: {}',script:undefined}),{action:'extensions',extensions:{merge_enabled:false,script_enabled:true,merge:'dns: {}',script:''}},'only a real true enables');

// Proxy groups.
assert.deepEqual([42,0,null,undefined].map(M.delayLabel),['42 ms','Timeout','Untested','Untested']);assert.deepEqual([40,200,900,0,null].map(M.delayLevel),['good','fair','slow','slow','none']);
assert.equal(M.modeLabel('global'),'Global');assert.equal(M.modeLabel('weird'),'Rule');assert.deepEqual(M.modes.map(m=>m[0]),['rule','global','direct']);
const groups=[{name:'Auto',now:'HK 01',all:['HK 01','US 02']},{name:'Proxy',now:'Auto',all:['Auto','DIRECT']},{name:'Loop',now:'Loop',all:['Loop']},{name:'Empty',now:null,all:[]}];
assert.equal(M.resolveNode(groups,'Proxy'),'HK 01','nested groups resolve to the node in use');assert.equal(M.resolveNode(groups,'DIRECT'),'DIRECT');assert.equal(M.resolveNode(groups,'Loop'),'','a cycle gives up');assert.equal(M.resolveNode(groups,'Empty'),'');
assert.deepEqual(M.memberOptions(groups[0],{'HK 01':{delay:88},'US 02':{delay:0}}),[{name:'HK 01',label:'HK 01 · 88 ms'},{name:'US 02',label:'US 02 · Timeout'}]);
assert.deepEqual(M.memberOptions({all:['A']},{A:{delay:null}}),[{name:'A',label:'A · Untested'}]);
assert.deepEqual(M.memberOptions(groups[1],{}),[{name:'Auto',label:'Auto'},{name:'DIRECT',label:'DIRECT'}]);
assert.deepEqual(M.filterGroups(groups,'  us ').map(g=>g.name),['Auto'],'a member match keeps its group');assert.equal(M.filterGroups(groups,'').length,4);assert.deepEqual(M.filterGroups(groups,'zzz'),[]);

// Delay tests are bounded and keep their order.
(async()=>{
  let running=0,peak=0;
  const done=await M.limited([1,2,3,4,5,6,7],3,async n=>{running++;peak=Math.max(peak,running);await new Promise(r=>setTimeout(r,5));running--;return n*2;});
  assert.deepEqual(done,[2,4,6,8,10,12,14]);assert.equal(peak,3);
  assert.deepEqual(await M.limited([],3,async n=>n),[]);
  let release, settled=false; const started=[];
  const held=new Promise(resolve=>{release=resolve;});
  const failed=M.limited([0,1,2,3,4],3,async n=>{started.push(n);if(n===0)throw new Error('first failed');await held;});
  const observed=failed.then(()=>{throw new Error('expected rejection');},error=>{settled=true;assert.equal(error.message,'first failed');});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(settled,false,'the batch must retain its lock until in-flight workers finish');
  release();await observed;assert.deepEqual(started,[0,1,2],'a failure stops new work');
  assert.equal(M.dashboardTarget({url:'/ui/mihomo/dashboard/#/proxies'}),'/ui/mihomo/dashboard/#/proxies');
  assert.equal(M.dashboardTarget({url:'/ui/mihomo/dashboard/'}),'/ui/mihomo/dashboard/');
  for(const bad of [null,{},{url:7},{url:''},{url:'https://evil.example/ui/mihomo/dashboard/'},{url:'//evil.example/ui/mihomo/dashboard/'},{url:'/ui/mihomo/api/'},{url:'/ui/mihomo/dashboard/\\evil'},{url:'/ui/mihomo/dashboard/ x'},{url:'javascript:alert(1)'}])assert.equal(M.dashboardTarget(bad),'',JSON.stringify(bad));
  console.log('Passed: Clash lifecycle wording, profile validation and requests, extensions, proxy-group resolution, bounded delay tests and Zashboard address checks.');
})().catch(error=>{console.error(error);process.exitCode=1;});
