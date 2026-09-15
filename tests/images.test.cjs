const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src-tauri/src/lib.rs'), 'utf8');
const template = source.match(/const INIT_JS_TPL: &str = r#"([\s\S]*?)"#;/)[1];
const origin = 'https://os.labstreamsas.com';

function shell({ frame = false, host = origin, images = [] } = {}) {
  class Image {
    constructor(src = '') { this.attrs = { src }; }
    get src() { return this.attrs.src; }
    set src(value) { this.attrs.src = value; }
    getAttribute(name) { return this.attrs[name]; }
    setAttribute(name, value) { this.attrs[name] = value; }
  }
  const setter = Object.getOwnPropertyDescriptor(Image.prototype, 'src').set;
  const setAttribute = Image.prototype.setAttribute;
  const emitted = [];
  const opened = [];
  const listeners = {};
  const nodes = images.map((src) => new Image(src));
  const root = { classList: { contains: () => false } };
  const win = {
    __TAURI__: { event: { emit: (...args) => emitted.push(args) }, core: { invoke: (...args) => opened.push(args) } },
    open: () => 'original',
  };
  win.self = win;
  win.top = frame ? {} : win;
  const sandbox = {
    window: win, URL, HTMLImageElement: Image,
    location: new URL(host + '/proyectos/ejemplo'),
    document: {
      readyState: 'complete', title: 'Entregables', documentElement: root, body: root,
      querySelector: () => null, querySelectorAll: () => nodes,
      addEventListener: (name, callback) => { listeners[name] = callback; },
    },
    history: {}, navigator: { platform: 'MacIntel' },
    getComputedStyle: () => ({ backgroundColor: 'rgb(255, 255, 255)' }),
    addEventListener: (name, callback) => { listeners[name] = callback; },
    setTimeout: () => 0,
  };
  vm.runInNewContext(template.replaceAll('__ORIGIN__', origin).replaceAll('__LABEL__', 'tab-1'), sandbox);
  return { Image, nodes, setter, setAttribute, win, emitted, opened, listeners };
}

test('does not replace native image setters or rewrite server-rendered covers', () => {
  const urls = ['/api/files-asset/cover?thumb=1', '/api/files-asset/cover?t=123.sig&thumb=1'];
  const state = shell({ images: urls });
  assert.equal(Object.getOwnPropertyDescriptor(state.Image.prototype, 'src').set, state.setter);
  assert.equal(state.Image.prototype.setAttribute, state.setAttribute);
  assert.deepEqual(state.nodes.map((image) => image.src), urls);
});

for (const url of [
  '/api/files-asset/cover?thumb=1',
  '/api/files-asset/cover?t=expired.sig&thumb=1&v=abc',
  '/api/files-asset/cover?thumb=xl',
  '/api/files-asset/video?poster=1',
  '/api/files-asset/original',
  'https://other.example/image.webp',
]) {
  test(`keeps authenticated browser transport for ${url}`, () => {
    const { Image } = shell();
    const image = new Image();
    image.src = url;
    assert.equal(image.src, url);
    image.setAttribute('src', url + '&r=1');
    assert.equal(image.src, url + '&r=1');
  });
}

test('tab opening and title reports still work', () => {
  const { win, emitted } = shell();
  win.open('/revisiones/123', '_blank');
  assert.equal(emitted[0][0], 'ls-title');
  assert.equal(emitted[0][1].title, 'Entregables');
  assert.equal(emitted[1][0], 'ls-new-tab');
  assert.equal(emitted[1][1].url, origin + '/revisiones/123');
});

test('does not interfere with embedded editors or unrelated origins', () => {
  for (const options of [{ frame: true }, { host: 'https://docs.example.com' }, { host: 'https://docs.google.com' }]) {
    const { win, emitted } = shell(options);
    assert.equal(win.__lsShell, undefined);
    assert.equal(win.open(), 'original');
    assert.equal(emitted.length, 0);
  }
});

test('Google editors and login use the system browser, never an embedded tab', () => {
  for (const host of ['docs', 'sheets', 'drive', 'accounts']) {
    const { win, emitted, opened } = shell();
    const url = `https://${host}.google.com/example`;
    win.open(url, '_blank');
    assert.equal(emitted.filter(([name]) => name === 'ls-new-tab').length, 0);
    assert.equal(opened[0][0], 'plugin:opener|open_url');
    assert.equal(opened[0][1].url, url);
  }
});
