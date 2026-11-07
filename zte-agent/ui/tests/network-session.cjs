// Real console modules, with delayed local responses crossing a Lock boundary.
const assert = require('node:assert/strict');
const {chromium} = require('playwright');
const {start} = require('./fixture.cjs');
(async () => {
  let pending;
  const fixture = await start(({url}, reply) => {
    if (url === '/api/mihomo/status') reply({installed:true, configured:true, running:true});
    else if (url === '/api/mihomo/profiles') reply({selected:'abc', profiles:[{id:'abc', name:'Private profile', remote:true}]});
    else if (['/api/mihomo/profiles/abc', '/api/mihomo/extensions', '/api/mihomo/dashboard'].includes(url)) pending = reply;
    else return false;
    return true;
  });
  const browser = await chromium.launch({headless:true, channel:'chrome'});
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => sessionStorage.setItem('openu60.session', 'fixture-session'));
  const ready = async () => {
    pending = null;
    if (page.url().startsWith(fixture.url)) await page.reload();
    else await page.goto(`${fixture.url}/#network/mihomo`);
    await page.locator('.mihomo-profile').waitFor();
  };
  const waitPending = async () => {
    for (let i=0; !pending && i<100; i++) await new Promise(resolve => setTimeout(resolve,10));
    assert.ok(pending, 'the private read is in flight');
  };
  const lock = async () => {
    await page.locator('#signout').click();
    await page.waitForFunction(() => OpenU60Console.locked);
  };
  try {
    await ready();
    await page.locator('.mihomo-profile').getByRole('button',{name:'Edit',exact:true}).click();
    await waitPending(); await lock();
    pending({name:'Private profile', source_type:'remote', url:'https://example.invalid/?token=private-fixture'});
    await page.waitForTimeout(150);
    assert.equal(await page.locator('#mihomo-profile-url').inputValue(),'');
    assert.equal(await page.locator('#mihomo-profile-url').isHidden(),true);

    await ready();
    await page.getByRole('button',{name:'Edit Merge & Script',exact:true}).click();
    await waitPending(); await lock();
    pending({merge:'private-fixture-merge', script:'private-fixture-script'});
    await page.waitForTimeout(150);
    assert.equal(await page.locator('#mihomo-merge').inputValue(),'');
    assert.equal(await page.locator('#mihomo-script').inputValue(),'');
    assert.equal(await page.locator('#mihomo-merge').isHidden(),true);

    await ready();
    const [tab] = await Promise.all([page.waitForEvent('popup'), page.getByRole('button',{name:'Open Zashboard',exact:true}).click()]);
    await waitPending(); await lock();
    const closed = tab.waitForEvent('close');
    pending({url:'/ui/mihomo/dashboard/#/proxies'});
    await closed;
    assert.ok(fixture.requests.filter(r=>r.url==='/api/auth/logout').length >= 3);
    assert.deepEqual(errors,[]);
    console.log('Passed: delayed private editors and dashboard grants cannot reopen after Lock; Lock sends server logout.');
  } finally { await browser.close(); await fixture.close(); }
})().catch(error=>{console.error(error);process.exitCode=1;});
