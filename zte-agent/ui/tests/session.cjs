// Authentication regressions use only an isolated local fixture.
const {chromium}=require('playwright'),assert=require('node:assert/strict'),http=require('node:http'),fs=require('node:fs'),path=require('node:path');
(async()=>{
  let loginCount=0,currentToken='',lateResponse;const streams=new Set(),errors=[],requests=[];
  const send=(res,data,status=200)=>{res.writeHead(status,{'Content-Type':'application/json'});res.end(JSON.stringify(status===200 ? {ok:true,data} : {ok:false,error:'unauthorized'}));};
  const server=http.createServer((req,res)=>{
    if(!req.url.startsWith('/api/')){const file=req.url==='/'?'index.html':(req.url.startsWith('/ui/')||req.url.startsWith('/console/a/'))?req.url.split('/').pop():'',full=path.join(__dirname,'..',file);if(!file||!fs.existsSync(full)){res.writeHead(404);res.end();return;}res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(full));return;}
    requests.push({url:req.url,auth:req.headers.authorization});
    if(req.url==='/api/auth/login'){let raw='';req.on('data',chunk=>raw+=chunk);req.on('end',()=>{if(JSON.parse(raw).password!=='test-password'){send(res,{},401);return;}currentToken=`fixture-token-${++loginCount}`;send(res,{token:currentToken});});return;}
    if(req.headers.authorization!==`Bearer ${currentToken}` || !currentToken){send(res,{},401);return;}
    if(req.url==='/api/test/late'){lateResponse=res;return;}
    if(req.url.startsWith('/api/dashboard/events')){res.writeHead(200,{'Content-Type':'text/event-stream'});streams.add(res);res.on('close',()=>streams.delete(res));res.write(`event: snapshot\ndata: ${JSON.stringify({readings:{wan:{ok:true,data:{up:true,'ipv4-address':[{address:'10.0.0.1'}]}},wan6:{ok:true,data:{up:true,'ipv6-address':[{address:'2001:db8::1'}]}}}})}\n\n`);return;}
    send(res,{});
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));const heartbeat=setInterval(()=>{for(const res of streams)res.write(': heartbeat\n\n');},1000);
  const browser=await chromium.launch({headless:true,channel:'chrome'}),context=await browser.newContext(),page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  const live=()=>page.waitForFunction(()=>OpenU60Console.connected),login=async()=>{await page.waitForFunction(()=>document.querySelector('#login-dialog').open);await page.locator('#password').fill('test-password');await page.locator('#login-form button[type=submit]').click();await live();};
  const base=`http://127.0.0.1:${server.address().port}`;await page.goto(base);await login();assert.equal(loginCount,1);
  assert.equal(await page.evaluate(()=>sessionStorage.getItem('openu60.session')),currentToken);assert.equal(await page.locator('#password').inputValue(),'');
  for(const key of ['network/signal','settings/about','dashboard/overview','settings/appearance']){await page.evaluate(key=>OpenU60Navigation.go(key),key);await live();assert.equal(await page.locator('#login-dialog').evaluate(node=>node.open),false);}
  await page.reload();await live();assert.equal(loginCount,1,'reload resumes the tab session');
  await page.locator('.brand').click();await live();assert.equal(loginCount,1,'the brand link keeps the session');
  assert.equal(await page.evaluate(()=>Object.values(localStorage).some(value=>value.includes('fixture-token')||value.includes('test-password'))),false);
  await page.evaluate(()=>{window.pendingOldRequest=OpenU60Console.request('/api/test/late').catch(()=>{});});
  await page.waitForTimeout(50);assert.ok(lateResponse);await page.locator('#signout').click();assert.equal(await page.evaluate(()=>sessionStorage.getItem('openu60.session')),null);
  await login();assert.equal(loginCount,2);send(lateResponse,{},401);await page.evaluate(()=>window.pendingOldRequest);await live();assert.equal(await page.locator('#login-dialog').evaluate(node=>node.open),false,'a late old-session 401 cannot lock the new session');
  for(const res of streams)res.write('event: auth\ndata: {}\n\n');await page.waitForFunction(()=>document.querySelector('#login-dialog').open);
  assert.equal(await page.evaluate(()=>sessionStorage.getItem('openu60.session')),null);assert.equal(await page.locator('#wan-ip').textContent(),'—');assert.equal(await page.locator('#wan-ipv6').textContent(),'—');
  await page.reload();await page.waitForFunction(()=>document.querySelector('#login-dialog').open);assert.equal(loginCount,2,'an expired/locked token is not restored');
  const privateContext=await browser.newContext();await privateContext.addInitScript(()=>Object.defineProperty(window,'sessionStorage',{get(){throw new DOMException('Unavailable','SecurityError');}}));const privatePage=await privateContext.newPage();privatePage.on('pageerror',error=>errors.push(error.message));await privatePage.goto(base);await privatePage.waitForFunction(()=>document.querySelector('#login-dialog').open);await privatePage.locator('#password').fill('test-password');await privatePage.locator('#login-form button[type=submit]').click();await privatePage.waitForFunction(()=>OpenU60Console.connected);await privatePage.evaluate(()=>OpenU60Navigation.go('settings/about'));assert.equal(await privatePage.locator('#login-dialog').evaluate(node=>node.open),false,'blocked storage falls back to memory');
  assert.ok(requests.filter(req=>req.url.startsWith('/api/dashboard/events')).every(req=>!req.url.includes('token=')));assert.deepEqual(errors,[]);
  await browser.close();clearInterval(heartbeat);for(const res of streams)res.destroy();server.close();console.log('Passed: one login across navigation and reload, hash-only brand link, tab-only token storage, lock/expiry cleanup, stale 401 isolation, no token URLs and storage-denied fallback.');
})().catch(error=>{console.error(error);process.exit(1);});
