// Generate fixtures with U60_WEB_QA_DIR=<dir> cargo test -p zte-agent web::tests::network.
// Run with U60_WEB_QA_DIR=<dir> node zte-agent/tests/network-ui.cjs.
// Requires Playwright (U60_PLAYWRIGHT may name its module) and Chromium.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const { chromium } = require(process.env.U60_PLAYWRIGHT || 'playwright');
const root = process.env.U60_WEB_QA_DIR;
assert(root, 'Set U60_WEB_QA_DIR to the generated fixtures directory');

let fixture = 'mihomo-profiles';
let redirect = false;
let htmlError = false;
let extensionsResult = null;
let onPost = null;
const posts = [];
const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  if (req.method === 'POST') {
    const chunks = [];
    req.on('data', chunk => chunks.push(chunk));
    req.on('end', () => {
      posts.push({ path: url.pathname, headers: req.headers, body: Buffer.concat(chunks).toString() });
      const respond = () => {
        if (extensionsResult) {
          const reply = extensionsResult;
          res.writeHead(reply.ok ? 200 : 400, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
          res.end(JSON.stringify(reply));
        } else if (htmlError) {
          res.writeHead(503, { 'Content-Type': 'text/html' });
          res.end(`<p class="msg err" role="alert">${typeof htmlError === 'string' ? htmlError : 'RA cleanup is busy; retry cleanup.'}</p>`);
        } else if (redirect) {
          res.writeHead(303, { Location: url.pathname.startsWith('/ui/relay/') ? '/ui/relay' : '/ui/router' });
          res.end();
        } else {
          res.writeHead(400, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
          res.end(JSON.stringify({ ok: false, error: 'Download failed safely', once: 'fresh-once-token' }));
        }
      };
      if (onPost) {
        const receive = onPost;
        onPost = null;
        receive(respond);
      } else setTimeout(respond, 800);
    });
    return;
  }
  if (url.pathname.startsWith('/ui/live/')) {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' }).end();
    return;
  }
  if (url.pathname.startsWith('/ui/a/')) {
    const file = path.join(root, url.pathname);
    if (!fs.existsSync(file)) { res.writeHead(404).end(); return; }
    res.setHeader('Content-Type', file.endsWith('.css') ? 'text/css' : file.endsWith('.js') ? 'text/javascript' : 'image/png');
    res.end(fs.readFileSync(file));
    return;
  }
  const reply = JSON.parse(fs.readFileSync(path.join(root, fixture + '.json')));
  for (const [key, value] of reply.headers) if (key !== 'content-length') res.setHeader(key, value);
  res.writeHead(reply.status).end(reply.html);
});

async function checkExtensions(browser, origin, width) {
  const context = await browser.newContext({ viewport: { width, height: 1000 } });
  const page = await context.newPage();
  try {
    fixture = 'mihomo-extensions'; redirect = false; htmlError = false;
    await page.goto(origin + '/ui/router/mihomo', { waitUntil: 'networkidle' });
    const form = page.locator('.net-extensions');
    const script = form.getByLabel('JavaScript', { exact: true });
    assert.equal(await script.inputValue(), 'function main(config) { return config; }');
    assert(await page.locator('#global-extensions').evaluate(details => details.open));
    const button = form.getByRole('button', { name: 'Validate all & apply extensions' });
    const changed = 'function main(config) { config.ipv6 = true; return config; }';
    await script.fill(changed);
    extensionsResult = { ok: true, once: 'saved-once', message: 'Global Merge & Script saved. All profiles validated and the changes were applied.', extensions: { merge: '', script: changed, merge_enabled: false, script_enabled: true } };
    const submitted = new Promise(resolve => { onPost = resolve; });
    await button.click();
    const respond = await submitted;
    try {
      const spinner = form.locator('.net-spin');
      await spinner.waitFor({ state: 'visible' });
      const size = await spinner.evaluate(node => { const css = getComputedStyle(node); return { width: parseFloat(css.width), height: parseFloat(css.height), grow: css.flexGrow }; });
      assert(Math.abs(size.width - size.height) <= 1 && size.grow === '0', `Spinner stretched at ${width}px: ${JSON.stringify(size)}`);
      assert(await script.isDisabled());
      await page.screenshot({ path: path.join(root, `extensions-saving-${width}.png`), fullPage: true });
    } finally { respond(); }
    await form.locator('.net-result.ok').waitFor();
    assert.equal(page.url(), origin + '/ui/router/mihomo');
    assert.equal(await script.inputValue(), changed);
    assert(await page.locator('#global-extensions').evaluate(details => details.open));
    assert.equal(await form.locator('.net-spin').count(), 0);
    assert(await script.isEnabled());
    assert.equal(await form.locator('[name="once"]').inputValue(), 'saved-once');
    assert(await form.locator('.net-result').getAttribute('role') === 'status');
    await page.screenshot({ path: path.join(root, `extensions-saved-${width}.png`), fullPage: true });

    const invalid = 'function main(config) { throw Error("private-error"); }';
    await script.fill(invalid);
    assert.equal(await form.locator('.net-result.ok').count(), 0);
    extensionsResult = { ok: false, once: 'retry-once', error: 'Global script validation failed; the previous configuration is unchanged.' };
    await button.click();
    await form.locator('.net-result.err').waitFor();
    assert.equal(await script.inputValue(), invalid);
    assert(await page.locator('#global-extensions').evaluate(details => details.open));
    assert(await form.locator('.net-result').textContent().then(text => text.includes('previous configuration is unchanged')));
    assert(await script.isEnabled());
    assert.equal(await form.locator('[name="once"]').inputValue(), 'retry-once');
    await page.screenshot({ path: path.join(root, `extensions-error-${width}.png`), fullPage: true });
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1));
    console.log(`PASS ${width}px: round spinner, saved text, expanded editor, inline success/error, repeat-save token`);
  } finally { extensionsResult = null; await context.close(); }
}

async function checkErrorFeedback(browser, origin, width) {
  const context = await browser.newContext({ viewport: { width, height: 900 } });
  const page = await context.newPage();
  const failures = [];
  page.on('pageerror', error => failures.push(error.message));
  const check = (ok, message) => { if (!ok) failures.push(message); };
  const checkSpacing = async (form, label) => {
    const gap = await form.evaluate(node => {
      const error = node.querySelector('.net-result').getBoundingClientRect();
      const buttons = [...node.querySelectorAll('button[type="submit"], .actions a')].map(button => button.getBoundingClientRect());
      const top = Math.min(...buttons.map(button => button.top));
      const bottom = Math.max(...buttons.map(button => button.bottom));
      return error.top >= bottom ? error.top - bottom : top - error.bottom;
    });
    check(gap >= 15, `${label}: error/button gap is ${gap}px at ${width}px`);
  };
  const submit = async (form, button, whilePending) => {
    const received = new Promise(resolve => { onPost = resolve; });
    await button.click();
    const respond = await received;
    try {
      await form.locator('.net-spin').waitFor({ state: 'visible' });
      await whilePending();
    } finally { respond(); }
    await form.locator('.net-spin').waitFor({ state: 'detached' });
  };
  try {
    fixture = 'mihomo-add'; redirect = false; htmlError = false;
    await page.goto(origin + '/ui/router/mihomo?add=1', { waitUntil: 'networkidle' });
    const form = page.locator('#profile-new form');
    await form.getByLabel('Name', { exact: true }).fill('Retry QA');
    await form.getByLabel('Subscription URL').fill('https://example.com/qa');
    const save = form.getByRole('button', { name: 'Save configuration' });
    await submit(form, save, async () => {});
    await checkSpacing(form, 'New download failure');
    await page.screenshot({ path: path.join(root, `error-feedback-${width}.png`), fullPage: true });
    await submit(form, save, async () => {
      check(await form.locator('.net-result:visible').count() === 0, `Retry leaves an old red error box at ${width}px`);
      check(await form.locator('[role="status"]').isVisible(), 'Retry must retain its progress indicator');
    });
    check(await form.locator('.net-result:visible').count() === 1, 'A new failure must show one fresh error');
    check((await form.locator('.net-result').textContent()).includes('Download failed safely'), 'New failure text must remain visible');

    fixture = 'mihomo-edit-error';
    await page.goto(origin + '/ui/router/mihomo?edit=one', { waitUntil: 'networkidle' });
    const edit = page.locator('#profile-edit form');
    await checkSpacing(edit, 'Server-rendered failure');
    const before = posts.length;
    await edit.getByLabel('Name', { exact: true }).fill('');
    await edit.getByRole('button', { name: 'Save changes' }).click();
    check(posts.length === before && await edit.locator('.net-result').isVisible(), 'Invalid input must not submit or discard the previous error');
    await edit.getByLabel('Name', { exact: true }).fill('Retry edited profile');
    await submit(edit, edit.getByRole('button', { name: 'Save changes' }), async () => {
      check(await edit.locator('.net-result:visible').count() === 0, `Server-rendered error remains during retry at ${width}px`);
    });
    await checkSpacing(edit, 'Retried editor failure');

    fixture = 'mihomo-errors';
    await page.goto(origin + '/ui/router/mihomo', { waitUntil: 'networkidle' });
    const first = page.locator('form:has(input[name="id"][value="one"])');
    const other = page.locator('form:has(input[name="id"][value="two"])');
    await submit(first, first.getByRole('button', { name: 'Update now' }), async () => {});
    await submit(other, other.getByRole('button', { name: 'Update now' }), async () => {});
    await checkSpacing(first, 'Profile update failure');
    await submit(first, first.getByRole('button', { name: 'Update now' }), async () => {
      check(await first.locator('.net-result:visible').count() === 0, 'Profile retry must clear its own previous result');
      check(await other.locator('.net-result').isVisible(), 'Profile retry must retain unrelated failures');
      check(await page.getByText('Core failed <safely>', { exact: true }).isVisible(), 'Profile retry must not hide core status');
    });
    fixture = 'mihomo-profiles'; redirect = true;
    await submit(first, first.getByRole('button', { name: 'Update now' }), async () => {
      check(await first.locator('.net-result:visible').count() === 0, 'Successful retry must also clear the previous result while pending');
    });
    await page.waitForURL(origin + '/ui/router');
    check(await page.locator('.net-result, .net-spin, .net-progress').count() === 0, 'Success must not retain stale feedback');
    redirect = false;
    assert.deepEqual(failures, []);
    console.log(`PASS ${width}px: error spacing, cleared retry feedback, fresh failure, validation preserves feedback`);
  } finally {
    await context.close();
  }
}

async function run() {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({ headless: true, executablePath: process.env.U60_CHROME || undefined });
  try {
    for (const width of [320, 390, 1440]) await checkExtensions(browser, origin, width);
    if (process.env.U60_EXTENSIONS_QA_ONLY) return;
    for (const width of [320, 390, 1440]) {
      const context = await browser.newContext({ viewport: { width, height: 900 } });
      const page = await context.newPage();
      fixture = 'relay-empty'; redirect = false;
      htmlError = 'LAN RA configuration differs after reboot; inspect the retained snapshot for a vendor commit before recovering';
      await page.goto(origin + '/ui/relay', { waitUntil: 'networkidle' });
      const scan = page.locator('form[action="/ui/relay/scan"]');
      await scan.locator('button').click();
      await scan.locator('.net-result').waitFor({ state: 'visible' });
      const layout = await scan.evaluate(form => {
        const error = form.querySelector('.net-result').getBoundingClientRect();
        const head = document.querySelector('#networks-heading').closest('.relay-head').getBoundingClientRect();
        const box = form.getBoundingClientRect();
        return { gap: error.top - head.bottom, width: error.width, formWidth: box.width };
      });
      await page.screenshot({ path: path.join(root, `relay-scan-error-${width}.png`), fullPage: true });
      assert(layout.gap >= 15 && layout.width >= layout.formWidth - 2,
        `Scan error must sit below the complete header, not squeeze it at ${width}px: ${JSON.stringify(layout)}`);
      const retry = new Promise(resolve => { onPost = resolve; });
      await scan.locator('button').click();
      const respond = await retry;
      try { assert.equal(await scan.locator('.net-result:visible').count(), 0); }
      finally { respond(); }
      await scan.locator('.net-spin').waitFor({ state: 'detached' });
      await context.close();
      console.log(`PASS ${width}px: full-width scan error below header and cleared on retry`);
    }
    htmlError = false;
    if (process.env.U60_RELAY_QA_ONLY) return;
    for (const width of [320, 390, 1440]) await checkErrorFeedback(browser, origin, width);
    if (process.env.U60_ERROR_QA_ONLY) return;
    for (const width of [320, 390, 1440]) {
      const context = await browser.newContext({ viewport: { width, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      fixture = 'mihomo-profiles'; redirect = false;
      await page.goto(origin + '/ui/router/mihomo', { waitUntil: 'networkidle' });
      const profileSpacing = await page.locator('a[href*="?edit="]').evaluateAll(links => links.map(link => {
        const card = link.closest('li').getBoundingClientRect();
        const buttons = [...link.closest('form').querySelectorAll('.btn')].map(button => button.getBoundingClientRect());
        return {
          bottom: card.bottom - Math.max(...buttons.map(button => button.bottom)),
          left: Math.min(...buttons.map(button => button.left)) - card.left,
          right: card.right - Math.max(...buttons.map(button => button.right)),
        };
      }));
      await page.screenshot({ path: path.join(root, `profile-list-${width}.png`), fullPage: true });
      assert(profileSpacing.length >= 2);
      for (const spacing of profileSpacing) {
        assert(spacing.bottom >= 15 && spacing.left >= 15 && spacing.right >= 15,
          `Profile actions need inset spacing at ${width}px: ${JSON.stringify(spacing)}`);
      }
      await page.getByRole('link', { name: 'Add configuration', exact: true }).click();
      const dialog = page.locator('#profile-new');
      assert(await dialog.evaluate(node => node.matches(':modal')));
      await dialog.getByLabel('Name', { exact: true }).fill('Browser QA');
      await dialog.getByLabel('Subscription URL').fill('https://example.com/?token=qa');
      const save = dialog.getByRole('button', { name: 'Save configuration' });
      const count = posts.length;
      await save.click();
      await dialog.locator('.net-spin').waitFor({ state: 'visible' });
      assert(await save.isDisabled());
      assert(await dialog.getByLabel('Name', { exact: true }).isDisabled());
      await dialog.locator('form').evaluate(form => form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true })));
      await dialog.getByRole('link', { name: 'Cancel' }).click();
      assert(await dialog.isVisible(), 'Cancel must not hide in-progress work');
      await dialog.locator('.net-result').waitFor({ state: 'visible' });
      await page.waitForFunction(() => !document.querySelector('#profile-new form').hasAttribute('aria-busy'));
      assert.equal(posts.length, count + 1);
      assert(posts.at(-1).body.includes('name="action"\r\n\r\nadd'));
      assert(posts.at(-1).body.includes('name="csrf"'));
      assert.equal(posts.at(-1).headers['x-u60-form'], '1');
      assert.equal(await dialog.getByLabel('Name', { exact: true }).inputValue(), 'Browser QA');
      assert.equal(await dialog.locator('[name="once"]').inputValue(), 'fresh-once-token');
      await dialog.getByLabel('Type', { exact: true }).selectOption('local');
      assert(await dialog.getByLabel('Subscription URL').isDisabled());
      await dialog.getByLabel('Paste YAML').fill("rules: ['MATCH,DIRECT']");
      await save.click();
      await dialog.locator('.net-spin').waitFor({ state: 'visible' });
      await page.waitForFunction(() => !document.querySelector('#profile-new form').hasAttribute('aria-busy'));
      assert(!posts.at(-1).body.includes('name="url"'));
      assert(posts.at(-1).body.includes("rules: ['MATCH,DIRECT']"));
      await dialog.getByRole('link', { name: 'Cancel' }).click();
      assert(!(await dialog.isVisible()));

      fixture = 'mihomo-edit';
      await page.goto(origin + '/ui/router/mihomo?edit=one', { waitUntil: 'networkidle' });
      const edit = page.locator('#profile-edit');
      assert(await edit.evaluate(node => node.matches(':modal')));
      assert.equal(await edit.getByLabel('Subscription URL').inputValue(), 'https://example.com/?token=private-url');
      assert.equal(await edit.getByLabel('Automatic update').inputValue(), '6');
      for (const element of [page.locator('html'), edit]) {
        const dimensions = await element.evaluate(node => ({ scroll: node.scrollWidth, width: node.clientWidth }));
        assert(dimensions.scroll <= dimensions.width + 1, 'No horizontal overflow');
      }
      for (const select of await edit.locator('select').all()) {
        assert(await select.evaluate(node => node.clientWidth >= node.parentElement.clientWidth - 3), 'Select must span its row');
      }
      await page.screenshot({ path: path.join(root, `modal-edit-${width}.png`), fullPage: true });
      fixture = 'mihomo-delete';
      await page.goto(origin + '/ui/router/mihomo?delete=one', { waitUntil: 'networkidle' });
      assert(await page.locator('#profile-delete').isVisible());

      fixture = 'relay-ipv6'; redirect = true;
      await page.goto(origin + '/ui/relay', { waitUntil: 'networkidle' });
      const disconnect = page.locator('form[action="/ui/relay/disconnect"]');
      const disconnectCount = posts.length;
      await disconnect.locator('button').click();
      await disconnect.locator('.net-spin').waitFor({ state: 'visible' });
      assert(await disconnect.locator('button').isDisabled());
      await disconnect.evaluate(form => form.dispatchEvent(new SubmitEvent('submit', { bubbles: true, cancelable: true })));
      await page.waitForTimeout(950);
      assert.equal(posts.length, disconnectCount + 1, 'Disconnect submits only once');
      htmlError = true;
      await page.goto(origin + '/ui/relay', { waitUntil: 'networkidle' });
      await disconnect.locator('button').click();
      await disconnect.locator('.net-spin').waitFor({ state: 'visible' });
      await page.waitForFunction(() => document.querySelector('.net-result')?.textContent.includes('RA cleanup is busy'));
      assert(!(await disconnect.locator('button').isDisabled()));
      htmlError = false;

      fixture = 'relay-scan'; redirect = true;
      await page.goto(origin + '/ui/relay', { waitUntil: 'networkidle' });
      const scan = page.locator('form[action="/ui/relay/scan"]');
      await scan.locator('button').click();
      await scan.locator('.net-spin').waitFor({ state: 'visible' });
      await page.waitForTimeout(950);
      const connect = page.locator('form[action="/ui/relay/connect"]');
      for (const field of ['remember', 'ipv6', 'confirmed']) {
        assert(await connect.locator(`[name="${field}"]`).isChecked(), `${field} defaults on`);
      }
      await connect.locator('[name="network"]').first().check();
      for (const field of await connect.locator('input[type="checkbox"][required]').all()) await field.check();
      await connect.locator('button[type="submit"]').click();
      await connect.locator('.net-spin').waitFor({ state: 'visible' });
      await page.waitForTimeout(950);
      fixture = 'router-off';
      await page.goto(origin + '/ui/router', { waitUntil: 'networkidle' });
      const toggle = page.locator('form[action="/ui/router/mihomo/switch"]');
      const toggleReceived = new Promise(resolve => { onPost = resolve; });
      await toggle.locator('button').click();
      const toggleRespond = await toggleReceived;
      try {
        await toggle.locator('.net-spin').waitFor({ state: 'visible' });
        assert.equal(await toggle.locator('[data-net-state]').textContent(), 'Starting Clash...');
        assert.equal(await toggle.locator('button .net-spin').count(), 0, 'The switch knob must never contain a spinner');
        assert.equal(await toggle.locator('button').getAttribute('aria-checked'), 'false', 'Do not claim Running before success');
        assert(await toggle.locator('button').isDisabled());
        const circle = await toggle.locator('.net-spin').evaluate(node => { const css = getComputedStyle(node); return css.width === css.height && css.flexGrow === '0'; });
        assert(circle, 'Switch spinner must stay round');
      } finally { toggleRespond(); }
      await toggle.locator('.net-spin').waitFor({ state: 'detached' });
      assert.equal(await toggle.locator('[data-net-state]').textContent(), 'Off');
      assert(await toggle.locator('button').isEnabled());
      assert.deepEqual(errors, []);
      await context.close();
      console.log(`PASS ${width}px: modal forms, duplicate prevention, retained input, private editor, busy indicators, CSP`);
    }
    const context = await browser.newContext({ javaScriptEnabled: false });
    const page = await context.newPage();
    for (const [name, query, id] of [['mihomo-add', 'add=1', 'profile-new'], ['mihomo-edit', 'edit=one', 'profile-edit'], ['mihomo-edit-error', 'edit=one', 'profile-edit']]) {
      fixture = name;
      await page.goto(origin + '/ui/router/mihomo?' + query);
      assert(await page.locator('#' + id).isVisible());
      assert.equal(await page.locator('#' + id + ' form').getAttribute('method'), 'post');
      if (name === 'mihomo-edit-error') assert(await page.locator('#' + id + ' .net-result').isVisible());
    }
    await context.close();
    console.log('PASS no-JavaScript editor fallback');
  } finally {
    await browser.close();
  }
}

run().catch(error => { console.error(error); process.exitCode = 1; }).finally(() => server.close());
