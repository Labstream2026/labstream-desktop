// Run with Playwright available: node tests/images.browser.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const { chromium, webkit } = require('playwright');

const root = path.join(__dirname, '..');
const source = fs.readFileSync(path.join(root, 'src-tauri/src/lib.rs'), 'utf8');
const template = source.match(/const INIT_JS_TPL: &str = r#"([\s\S]*?)"#;/)[1];
const png = fs.readFileSync(path.join(root, 'app-icon.png'));
const urls = [
  '/api/files-asset/session?thumb=1',
  '/api/files-asset/signed?thumb=1&t=expired.sig',
  '/api/files-asset/poster?poster=1',
  '/api/files-asset/large?thumb=xl',
  '/api/files-asset/original',
];
const counts = { authenticated: 0, revalidated: 0, unauthorized: 0 };
const server = http.createServer((req, res) => {
  if (req.url.startsWith('/api/files-asset/')) {
    if (req.headers.cookie !== 'test-session=ok') {
      counts.unauthorized++;
      res.writeHead(401, { 'Cache-Control': 'no-store' }).end();
      return;
    }
    counts.authenticated++;
    const headers = { 'Content-Type': 'image/png', 'Cache-Control': 'private, no-cache', ETag: '"cover-1"' };
    if (req.headers['if-none-match'] === '"cover-1"') {
      counts.revalidated++;
      res.writeHead(304, headers).end();
    } else res.writeHead(200, headers).end(png);
    return;
  }
  res.writeHead(200, { 'Content-Type': 'text/html', 'Cache-Control': 'no-store' });
  res.end(`<html><title>Portadas: prueba local</title><body>
    ${urls.map((url, i) => `<img id="image-${i}" width="120" height="120" src="${url}">`).join('')}
    <canvas width="16" height="16"></canvas></body></html>`);
});

(async () => {
  let browser;
  try {
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    const origin = `http://127.0.0.1:${server.address().port}`;
    browser = await (process.env.TEST_BROWSER === 'webkit' ? webkit : chromium).launch({ headless: true });
    const context = await browser.newContext();
    await context.addCookies([{ name: 'test-session', value: 'ok', url: origin, httpOnly: true }]);
    await context.addInitScript(`window.__TAURI__ = { event: { emit() {} } };`);
    await context.addInitScript(template.replaceAll('__ORIGIN__', origin).replaceAll('__LABEL__', 'tab-test'));
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(origin);
    await page.waitForFunction(() => [...document.images].every((image) => image.complete && image.naturalWidth > 0));
    assert.deepEqual(await page.locator('img').evaluateAll((images) => images.map((image) => image.getAttribute('src'))), urls);
    // Cached images must still be readable by the annotation canvas.
    assert.equal(await page.evaluate(() => {
      const canvas = document.querySelector('canvas');
      canvas.getContext('2d').drawImage(document.images[0], 0, 0, 16, 16);
      return canvas.toDataURL().startsWith('data:image/png;');
    }), true);
    await page.reload();
    await page.waitForFunction(() => [...document.images].every((image) => image.complete && image.naturalWidth > 0));
    assert.ok(counts.revalidated >= urls.length, 'HTTP cache revalidation expected on reload');
    await page.evaluate(() => {
      const image = new Image();
      image.id = 'dynamic';
      image.setAttribute('src', '/api/files-asset/dynamic?thumb=1');
      document.body.appendChild(image);
    });
    await page.waitForFunction(() => document.getElementById('dynamic').naturalWidth > 0);
    // A native downloader without the webview session reproduces the 401.
    assert.equal((await fetch(origin + urls[0])).status, 401);
    await context.clearCookies();
    await page.reload();
    assert.equal(await page.locator('img').evaluateAll((images) => images.filter((image) => image.naturalWidth > 0).length), 0);
    assert.deepEqual(errors, []);
    console.log(JSON.stringify({ ok: true, covers: urls.length, dynamic: true, canvas: true, ...counts }));
  } finally {
    if (browser) await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
