(function () {
  var I = window.__TAURI_INTERNALS__;
  window.__RH_BUILD__ = { version: '__RH_VERSION__', sha: '__RH_SHA__' };
  window.__RH_IS_NATIVE__ = !!I;
  if (!I) { return; }
  window.__RH_NATIVE__ = {
    invoke: function (cmd, args) { return I.invoke(cmd, args || {}); },
    listen: function (event, cb) {
      return I.invoke('plugin:event|listen', {
        event: event,
        target: { kind: 'Any' },
        handler: I.transformCallback(function (e) { cb(e); })
      });
    }
  };

  // About is a separate document, not another navigation target. It needs
  // build metadata, but must never change the main window's history menus.
  if (!__RH_MAIN_WINDOW__) { return; }

  // Keep the native Go menu in step with SPA links as well as menu actions.
  // A marker belongs to an entry, so repeated visits to one route still go
  // back correctly. Merge it with router state instead of discarding it.
  var history = window.history;
  var marker = '__rhNavigationIndex';
  var stored = history.state && history.state[marker];
  var index = Number.isInteger(stored) && stored >= 0 ? stored : 0;
  var last = index;
  try {
    if (stored === index) {
      last = Math.max(index, Number(window.sessionStorage.getItem('rh.navigationLast')) || 0);
    }
  } catch (_) {}
  var push = history.pushState.bind(history);
  var replace = history.replaceState.bind(history);
  var currentUrl = window.location.href;
  function stateAt(state, position) {
    var next = Object.assign({}, state);
    next[marker] = position;
    return next;
  }
  function syncHistory() {
    currentUrl = window.location.href;
    try { window.sessionStorage.setItem('rh.navigationLast', String(last)); } catch (_) {}
    I.invoke('navigation_state', { back: index > 0, forward: index < last }).catch(function () {});
  }
  replace(stateAt(history.state, index), '', window.location.href);
  history.pushState = function (state, title, url) {
    // Only advance after a successful push (a malformed URL can throw).
    push(stateAt(state, index + 1), title, url);
    index += 1;
    last = index;
    syncHistory();
  };
  history.replaceState = function (state, title, url) {
    replace(stateAt(state, index), title, url);
    syncHistory();
  };
  window.addEventListener('popstate', function () {
    var position = history.state && history.state[marker];
    if (Number.isInteger(position) && position >= 0) {
      index = position;
    } else if (window.location.href !== currentUrl &&
        window.location.href.split('#')[0] === currentUrl.split('#')[0]) {
      // Native fragment links (including Skip to main content) create an
      // entry without calling pushState. Mark it as a new entry, including
      // when it branches after Back, so it cannot erase the route history.
      index += 1;
      last = index;
      replace(stateAt(history.state, index), '', window.location.href);
    } else {
      index = last = 0;
      replace(stateAt(history.state, index), '', window.location.href);
    }
    syncHistory();
  });
  syncHistory();

  // Keep Copy and Look Up in text/content regions; suppress browser-only
  // actions over native app chrome.
  window.addEventListener('contextmenu', function (e) {
    var t = e.target;
    if (t && t.closest &&
        t.closest('input, textarea, [contenteditable], .rh-rich, .rh-line, .rh-post, pre, code')) {
      return;
    }
    e.preventDefault();
  }, { capture: true });
  window.__RH_NATIVE__.listen('rh://navigate', function (e) {
    var to = e && e.payload;
    if (typeof to !== 'string' || to.charAt(0) !== '/' || to.charAt(1) === '/') { return; }
    // Let transient SPA navigation UI close even when this destination is
    // already active. Native menus bypass the SPA's own shortcut handler.
    window.dispatchEvent(new Event('rh-native-navigation'));
    var current = window.location.pathname + window.location.search + window.location.hash;
    if (to === current) { return; }
    history.pushState({}, '', to);
    window.dispatchEvent(new PopStateEvent('popstate', { state: history.state }));
  });
  window.__RH_NATIVE__.listen('rh://history', function (e) {
    if (e && e.payload === 'back' && index > 0) {
      window.dispatchEvent(new Event('rh-native-navigation'));
      history.back();
    }
    if (e && e.payload === 'forward' && index < last) {
      window.dispatchEvent(new Event('rh-native-navigation'));
      history.forward();
    }
  });
  window.__RH_NATIVE__.listen('rh://fullscreen', function (e) {
    document.documentElement.classList.toggle('rh-fullscreen', !!(e && e.payload));
  });
  // Events cover transitions; ask for the restored window's starting state.
  window.__RH_NATIVE__.invoke('fullscreen_state')
    .then(function (fs) {
      document.documentElement.classList.toggle('rh-fullscreen', !!fs);
    })
    .catch(function () {});
})();
