const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/native-shim.js'), 'utf8');

function shell({ main = true, entries = [{ url: '/lobby', state: null }], position = 0, savedLast } = {}) {
  const listeners = new Map();
  const nativeListeners = new Map();
  const calls = [];
  const dispatched = [];
  const storage = new Map(savedLast === undefined ? [] : [['rh.navigationLast', savedLast]]);
  const location = new URL(entries[position].url, 'https://tauri.localhost');
  const history = {
    get state() { return entries[position].state; },
    get length() { return entries.length; },
    pushState(state, title, url) {
      const next = new URL(url, location);
      if (next.origin !== location.origin) { throw new Error('Cross-origin navigation'); }
      entries.splice(position + 1, Infinity, { url: next.href, state });
      position++;
      location.href = next.href;
    },
    replaceState(state, title, url) {
      entries[position] = { url, state };
      location.href = new URL(url, location).href;
    },
    back() {
      if (position > 0) { position--; location.href = new URL(entries[position].url, location).href; fire('popstate'); }
    },
    forward() {
      if (position + 1 < entries.length) { position++; location.href = new URL(entries[position].url, location).href; fire('popstate'); }
    },
  };
  function fire(type, event = {}) { for (const listener of listeners.get(type) || []) { listener(event); } }
  const window = {
    history,
    location,
    sessionStorage: { getItem: key => storage.get(key), setItem: (key, value) => storage.set(key, value) },
    addEventListener(type, listener) { listeners.set(type, [...(listeners.get(type) || []), listener]); },
    dispatchEvent(event) { dispatched.push(event.type); fire(event.type, event); },
    __TAURI_INTERNALS__: {
      transformCallback: callback => callback,
      invoke(command, args) {
        calls.push({ command, args });
        if (command === 'plugin:event|listen') { nativeListeners.set(args.event, args.handler); }
        return Promise.resolve(false);
      },
    },
  };
  vm.runInNewContext(source.replace('__RH_MAIN_WINDOW__', String(main)), {
    window,
    document: { documentElement: { classList: { toggle() {} } } },
    Event: class { constructor(type) { this.type = type; } },
    PopStateEvent: class { constructor(type, init) { this.type = type; Object.assign(this, init); } },
  });
  return {
    window, calls, entries, storage, dispatched,
    followHash(hash) {
      const next = new URL(location.href);
      next.hash = hash;
      // A native fragment link creates a null-state entry without invoking
      // history.pushState (the app's rel=external skip link does exactly this).
      entries.splice(position + 1, Infinity, { url: next.href, state: null });
      position++;
      location.href = next.href;
      fire('popstate', { state: null });
    },
    emit: (event, payload) => nativeListeners.get(event)?.({ payload }),
    available: () => calls.filter(call => call.command === 'navigation_state').at(-1)?.args,
  };
}

test('native navigation preserves router state, back/forward boundaries, and branching', () => {
  const app = shell();
  assert.equal(app.available().back, false);
  app.emit('rh://history', 'back');
  assert.equal(app.window.location.pathname, '/lobby');
  app.window.history.pushState({ scroll: 80 }, '', '/files');
  assert.equal(app.window.history.state.scroll, 80);
  app.emit('rh://navigate', '/radio');
  app.emit('rh://history', 'back');
  assert.equal(app.window.location.pathname, '/files');
  assert.equal(app.available().back, true);
  assert.equal(app.available().forward, true);
  app.emit('rh://history', 'forward');
  assert.equal(app.window.location.pathname, '/radio');
  app.emit('rh://history', 'back');
  app.emit('rh://navigate', '/boards');
  assert.equal(app.available().forward, false);
  assert.equal(app.window.history.length, 3);
  app.emit('rh://history', 'forward');
  assert.equal(app.window.location.pathname, '/boards');
});

test('revisiting the active route does not add history; appearance anchors work', () => {
  const app = shell();
  app.emit('rh://navigate', '/lobby');
  assert.equal(app.window.history.length, 1);
  app.emit('rh://navigate', '/settings#appearance');
  assert.equal(app.window.location.pathname, '/settings');
  assert.equal(app.window.location.hash, '#appearance');
  app.emit('rh://navigate', '/settings#appearance');
  assert.equal(app.window.history.length, 2);
});

test('native menu destinations cannot navigate to external sites', () => {
  const app = shell();
  for (const url of ['https://example.com', '//example.com', null, 4]) {
    app.emit('rh://navigate', url);
  }
  assert.equal(app.window.history.length, 1);
  assert.throws(() => app.window.history.pushState({}, '', 'https://example.com'));
  assert.equal(app.available().back, false);
});

test('replaceState preserves history position and reload restores forward availability', () => {
  const app = shell();
  app.emit('rh://navigate', '/files');
  app.emit('rh://navigate', '/radio');
  app.emit('rh://history', 'back');
  app.window.history.replaceState({ filter: 'audio' }, '', '/files?type=audio');
  assert.equal(app.window.history.state.filter, 'audio');
  const restored = shell({ entries: app.entries, position: 1, savedLast: app.storage.get('rh.navigationLast') });
  assert.equal(restored.available().back, true);
  assert.equal(restored.available().forward, true);
  restored.emit('rh://history', 'forward');
  assert.equal(restored.window.location.pathname, '/radio');
});

test('About only installs the bridge and never mutates the main navigation state', () => {
  const about = shell({ main: false, entries: [{ url: '/about', state: null }] });
  assert.equal(about.window.__RH_IS_NATIVE__, true);
  assert.equal(typeof about.window.__RH_NATIVE__.invoke, 'function');
  assert.equal(about.calls.length, 0);
  assert.equal(about.window.history.state, null);
});

test('native fragment links retain route history, branching, and reload availability', () => {
  const app = shell();
  app.emit('rh://navigate', '/files');
  app.followHash('rh-main');
  assert.equal(app.window.location.hash, '#rh-main');
  assert.equal(app.available().back, true);
  app.emit('rh://history', 'back');
  assert.equal(app.window.location.pathname, '/files');
  assert.equal(app.window.location.hash, '');
  app.emit('rh://history', 'back');
  assert.equal(app.window.location.pathname, '/lobby');
  assert.equal(app.available().back, false);
  app.emit('rh://history', 'forward');
  app.followHash('details');
  assert.equal(app.available().forward, false);
  assert.equal(app.window.history.length, 3);
  const restored = shell({ entries: app.entries, position: 2, savedLast: app.storage.get('rh.navigationLast') });
  assert.equal(restored.available().back, true);
  assert.equal(restored.available().forward, false);
  restored.emit('rh://history', 'back');
  assert.equal(restored.window.location.pathname, '/files');
  assert.equal(restored.window.location.hash, '');
  restored.emit('rh://history', 'forward');
  assert.equal(restored.window.location.hash, '#details');
});

test('native destination and history actions dismiss transient navigation UI', () => {
  const app = shell();
  let paletteOpen = true;
  app.window.addEventListener('rh-native-navigation', () => { paletteOpen = false; });
  // Same-destination selection still closes the overlay, without an extra entry.
  app.emit('rh://navigate', '/lobby');
  assert.equal(paletteOpen, false);
  assert.equal(app.window.history.length, 1);
  paletteOpen = true;
  app.emit('rh://navigate', '/files');
  assert.equal(paletteOpen, false);
  assert.deepEqual(app.dispatched.slice(-2), ['rh-native-navigation', 'popstate']);
  paletteOpen = true;
  app.emit('rh://history', 'back');
  assert.equal(paletteOpen, false);
  paletteOpen = true;
  app.emit('rh://history', 'forward');
  assert.equal(paletteOpen, false);
  paletteOpen = true;
  app.emit('rh://navigate', 'https://example.com');
  assert.equal(paletteOpen, true);
});
