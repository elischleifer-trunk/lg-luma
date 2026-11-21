// Luma's Criterion player. webOS runs this script on every page the app opens, including the
// Criterion Channel's own pages, which makes the Channel's web player behave like a TV app:
// the film fills the screen, the remote's media keys work and Back returns to Luma.
// Only pages of the Channel itself are ever opened: links come from Luma's launch parameters.
(function (root) {
  if (root.LumaCriterion) return;
  var ORIGIN = 'https://www.criterionchannel.com';

  // The film or page Luma asked for, or the Channel's home page.
  function target(params) {
    var link = params && typeof params.target === 'string' ? params.target : '';
    if (link.length <= 512) {
      try {
        var url = new URL(link);
        if (url.origin === ORIGIN && !url.username && !url.password) return url.href;
      } catch (_error) {
        // Not a URL: the home page.
      }
    }
    return ORIGIN + '/';
  }

  // webOS hands launch parameters over as a JSON string (an object in a relaunch event).
  function parse(value) {
    if (value && typeof value === 'object') return value;
    if (!value) return {};
    try {
      var parsed = JSON.parse(value);
      return parsed && typeof parsed === 'object' ? parsed : {};
    } catch (_error) {
      return {};
    }
  }

  // What a remote key does. Back and the media keys always work; OK and the left and right
  // arrows control the film only while one is on screen, and move around the page otherwise.
  function action(keyCode, playing) {
    switch (keyCode) {
      case 461:
        return 'back';
      case 415:
        return 'play';
      case 19:
        return 'pause';
      case 417:
        return 'forward';
      case 412:
        return 'rewind';
      case 13:
        return playing ? 'toggle' : null;
      case 39:
        return playing ? 'forward' : null;
      case 37:
        return playing ? 'rewind' : null;
      default:
        return null;
    }
  }

  // The Channel's sign-in pages. Luma gives the player a one-time code with each film; on these
  // pages it is traded for the account Home Assistant holds, at Luma's API on the TV itself.
  var SIGN_IN_ORIGIN = 'https://login.criterion.com';

  // A request to Luma's API on the TV, proven with the launch's one-time code.
  function lumaRequest(params, path) {
    var code = params && params.code;
    var port = Number(params && params.port);
    if (typeof code !== 'string' || !/^[0-9a-f]{32}$/.test(code)) return null;
    if (!(port >= 1 && port <= 65535 && Math.floor(port) === port)) return null;
    return {
      url: 'http://127.0.0.1:' + port + path,
      // Plain text, so the browser sends it without asking Luma first.
      body: JSON.stringify({ code: code }),
    };
  }

  function signInRequest(params) {
    return lumaRequest(params, '/api/criterion/sign-in');
  }

  // Whether Luma asked to play a film (its page with `?play=1`), rather than show a page such as
  // the Channel's search. Only then does a cover hide the Channel's pages until the film plays.
  function playsFilm(params) {
    var url = new URL(target(params));
    return url.pathname.indexOf('/films/') === 0 && url.searchParams.get('play') === '1';
  }

  // One launch of a film: its one-time sign-in code, else its page.
  function launchKey(params) {
    var code = params && params.code;
    return typeof code === 'string' && code ? code : target(params);
  }

  // The Channel's own sign-in, returning to `page` (a path on the Channel) afterwards.
  function loginUrl(page) {
    var path =
      typeof page === 'string' && page.charAt(0) === '/' && page.charAt(1) !== '/' ? page : '/';
    return ORIGIN + '/login?returnTo=' + encodeURIComponent(path);
  }

  // A button or link that signs in: "Log In", "Log in or subscribe".
  function isLogIn(text) {
    return /^\s*log\s*in\b/i.test(String(text || ''));
  }

  var SKIP = 10;
  root.LumaCriterion = {
    ORIGIN: ORIGIN,
    SIGN_IN_ORIGIN: SIGN_IN_ORIGIN,
    SKIP: SKIP,
    target: target,
    parse: parse,
    action: action,
    signInRequest: signInRequest,
    playsFilm: playsFilm,
    launchKey: launchKey,
    loginUrl: loginUrl,
    isLogIn: isLogIn,
  };
  if (typeof document === 'undefined') return;

  var palm = root.PalmSystem;

  // ---- Loading cover --------------------------------------------------------------------------

  // While a film loads, the Channel's pages (and its sign-in, when the account fills it in) stay
  // behind a black cover with the Channel's mark, which lifts when the film plays. It lifts
  // early wherever the viewer has something to do: a sign-in by hand, or a page that never plays.
  var COVER_ID = 'luma-criterion-cover';
  var COVER_LIMIT = 20000;
  var COVER_STYLE =
    'html{background:#000!important}' +
    '#' +
    COVER_ID +
    '{position:fixed;left:0;top:0;width:100vw;height:100vh;margin:0;' +
    'z-index:2147483647;background:#000;display:flex;align-items:center;' +
    'justify-content:center;opacity:1;transition:opacity .3s ease-out;cursor:none}' +
    '#' +
    COVER_ID +
    ' svg{width:120px;height:120px;color:#fff;' +
    'animation:luma-criterion-breathe 1.6s ease-in-out infinite alternate}' +
    '@keyframes luma-criterion-breathe{from{opacity:.35}to{opacity:.9}}';
  var MARK =
    '<svg viewBox="0 0 60 60" fill="currentColor" aria-hidden="true"><path d="M4.14577 14.847C-4.25441 29.1014 0.50232 47.4716 14.8737 55.8034C14.9749 55.8034 14.9749 55.9038 15.0761 55.9038C21.1486 59.4172 28.3343 60.7222 35.2163 59.618L32.9898 46.267C32.1801 46.3674 31.4717 46.4678 30.662 46.4678C27.6258 46.4678 24.5896 45.6647 21.9582 44.1589C14.0641 39.6417 11.3315 29.6034 15.8858 21.7735C15.8858 21.7735 15.8858 21.6731 15.987 21.6731C20.6425 13.8432 30.9656 11.2332 38.8598 15.8508C42.1996 17.7581 44.7298 20.7696 46.0455 24.3834L59 19.7658C53.2312 4.10599 35.7224 -3.92468 19.9341 1.89755C13.2544 4.30675 7.78923 8.824 4.14577 14.847Z"/></svg>';
  var DONE = 'luma-cover-done';
  var NEXT = 'luma-cover-next';
  var cover = null;
  var coverTimer = 0;

  function session(name, value) {
    try {
      if (value === undefined) return sessionStorage.getItem(name);
      if (value === null) sessionStorage.removeItem(name);
      else sessionStorage.setItem(name, value);
    } catch (_error) {
      // No storage: the cover shows, and lifts on its timer.
    }
    return null;
  }

  // The launch this page belongs to: a relaunch leaves its film here before navigating, since
  // webOS may still report the first launch's parameters.
  function launchParams() {
    var next = session(NEXT);
    if (next) {
      session(NEXT, null);
      return parse(next);
    }
    return parse(palm && palm.launchParams);
  }

  function uncover(played) {
    clearTimeout(coverTimer);
    if (played && cover) session(DONE, cover.getAttribute('data-launch'));
    var leaving = cover;
    cover = null;
    if (!leaving) return;
    leaving.style.opacity = '0';
    setTimeout(function () {
      if (leaving.parentNode) leaving.parentNode.removeChild(leaving);
    }, 350);
  }

  function coverUp(params) {
    if (!playsFilm(params)) return;
    var key = launchKey(params);
    // This launch's film already played: the viewer is browsing the Channel now.
    if (session(DONE) === key) return;
    if (location.origin === SIGN_IN_ORIGIN && !signInRequest(params)) return;
    var style = document.createElement('style');
    style.textContent = COVER_STYLE;
    cover = document.createElement('div');
    cover.id = COVER_ID;
    cover.setAttribute('data-launch', key);
    cover.innerHTML = MARK;
    cover.appendChild(style);
    // At the very start of a page the document has no root yet: add the cover once it does.
    if (document.documentElement) document.documentElement.appendChild(cover);
    else {
      var watch = new MutationObserver(function () {
        if (!document.documentElement) return;
        watch.disconnect();
        if (cover) document.documentElement.appendChild(cover);
      });
      watch.observe(document, { childList: true });
    }
    // Above whatever the page adds: kept last in the document once its body is there.
    document.addEventListener('DOMContentLoaded', function () {
      if (cover) document.documentElement.appendChild(cover);
    });
    // Never a black screen for good: a page that does not play, or plays only on a press.
    coverTimer = setTimeout(function () {
      uncover(true);
    }, COVER_LIMIT);
  }

  var launch = launchParams();
  coverUp(launch);

  function film() {
    var videos = document.getElementsByTagName('video');
    for (var i = 0; i < videos.length; i++) {
      if (videos[i].readyState > 0 || !videos[i].paused) return videos[i];
    }
    return null;
  }

  // The player's own box (JW Player's root) fills the screen, over the rest of the page.
  // The remote's highlight: whichever button or link the arrows reached.
  var STYLE =
    '.luma-criterion-fill{position:fixed!important;left:0!important;top:0!important;' +
    'width:100vw!important;height:100vh!important;max-width:none!important;' +
    'max-height:none!important;margin:0!important;z-index:2147483647!important;' +
    'background:#000!important}' +
    '.luma-criterion-fill video{object-fit:contain!important}' +
    ':focus-visible,.luma-criterion-focus{outline:4px solid #fff!important;' +
    'outline-offset:3px!important}';

  function addStyle() {
    if (document.getElementById('luma-criterion-style')) return;
    var style = document.createElement('style');
    style.id = 'luma-criterion-style';
    style.textContent = STYLE;
    (document.head || document.documentElement).appendChild(style);
  }

  // The Channel's player: a full-screen panel holding the video's frame and, beside it, the
  // Channel's own controls (progress, skip, captions), drawn while the mouse moves. The panel,
  // not the video's frame, fills the screen, so the controls stay above the picture.
  function playerBox(video) {
    for (var box = video.parentElement; box && box !== document.body; box = box.parentElement) {
      if (box.querySelector('[class*="_controls"]')) return box;
    }
    return video.closest('.jwplayer') || video.parentElement || video;
  }

  // The Channel shows its controls on mouse movement; a remote press counts as movement.
  function wake(video) {
    var box = playerBox(video);
    var x = Math.round(innerWidth / 2);
    var y = Math.round(innerHeight / 2);
    ['pointermove', 'mousemove'].forEach(function (type) {
      var init = { bubbles: true, cancelable: true, clientX: x, clientY: y, view: root };
      var event =
        type === 'pointermove' && typeof PointerEvent === 'function'
          ? new PointerEvent(type, init)
          : new MouseEvent(type, init);
      box.dispatchEvent(event);
    });
  }

  // Arrows move the highlight between the page's buttons and links while no film plays, in
  // reading order, staying inside an open dialog ("Unable to play video": Try again, Close).
  // OK presses the highlighted one, as Enter does.
  function focusables() {
    var scope = document;
    var dialogs = document.querySelectorAll(
      '[role="dialog"], [role="alertdialog"], [aria-modal="true"]'
    );
    for (var i = dialogs.length - 1; i >= 0; i--) {
      if (visible(dialogs[i])) {
        scope = dialogs[i];
        break;
      }
    }
    var all = scope.querySelectorAll(
      'button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    var out = [];
    for (var j = 0; j < all.length; j++) {
      var rect = all[j].getBoundingClientRect();
      if (visible(all[j]) && rect.width > 0 && rect.height > 0) out.push(all[j]);
    }
    return out;
  }

  function moveFocus(step) {
    var items = focusables();
    if (!items.length) return;
    var at = items.indexOf(document.activeElement);
    var next = items[at < 0 ? 0 : (at + step + items.length) % items.length];
    next.focus();
    if (next.scrollIntoView) next.scrollIntoView({ block: 'nearest' });
  }

  function fill(video) {
    addStyle();
    var box = playerBox(video);
    if (box.classList.contains('luma-criterion-fill')) return;
    box.classList.add('luma-criterion-fill');
    if (box.requestFullscreen && !document.fullscreenElement) {
      // Older engines return nothing; the fixed box above already covers the screen.
      var request = box.requestFullscreen();
      if (request && request.catch) request.catch(function () {});
    }
  }

  function leave() {
    // Back to Luma's Criterion screen: Luma is Home, which webOS brings to the front when this
    // app closes. Not platformBack(), which asks whether to exit the app.
    var video = film();
    if (video) video.pause();
    root.close();
  }

  document.addEventListener(
    'keydown',
    function (event) {
      var video = film();
      var name = action(event.keyCode, Boolean(video));
      if (!name) {
        var step = { 37: -1, 38: -1, 39: 1, 40: 1 }[event.keyCode];
        if (!video && step && location.origin === ORIGIN) {
          event.preventDefault();
          addStyle();
          moveFocus(step);
        }
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      if (name === 'back') return leave();
      if (!video) return;
      if (name === 'play' || (name === 'toggle' && video.paused)) video.play();
      else if (name === 'pause' || name === 'toggle') video.pause();
      else if (name === 'forward') video.currentTime = video.currentTime + SKIP;
      else if (name === 'rewind') video.currentTime = Math.max(0, video.currentTime - SKIP);
      // Show the progress bar for whatever the press did.
      wake(video);
    },
    true
  );

  document.addEventListener(
    'playing',
    function (event) {
      if (event.target && event.target.tagName === 'VIDEO') {
        fill(event.target);
        uncover(true);
      }
    },
    true
  );

  // ---- Signing in ----------------------------------------------------------------------------

  // Typed the way the page's own script notices: through the input's native value setter.
  function type(input, value) {
    var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    setter.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  }

  function visible(element) {
    return element && element.offsetParent !== null && !element.disabled;
  }

  // A wrong password must never become many: Auth0 blocks an account after a run of failed
  // sign-ins. The password is entered at most twice an hour, counted across launches, and an
  // error on the page pauses signing in for an hour; the viewer then sees the page and its message.
  var TRIES = 'luma-sign-in';
  var PAUSED = 'luma-sign-in-paused';
  var HOUR = 60 * 60 * 1000;

  function stored(name) {
    try {
      return JSON.parse(localStorage.getItem(name) || 'null');
    } catch (_error) {
      return null;
    }
  }

  function store(name, value) {
    try {
      localStorage.setItem(name, JSON.stringify(value));
    } catch (_error) {
      // No storage: a page with an error still stops this page from trying again.
    }
  }

  function paused() {
    var until = stored(PAUSED);
    return typeof until === 'number' && Date.now() < until;
  }

  // At most two trips a few minutes from a film's "Log in or subscribe" to the sign-in, so a
  // sign-in that does not stick never loops.
  function mayVisit(key) {
    try {
      var now = Date.now();
      var tries = JSON.parse(sessionStorage.getItem(key) || '[]').filter(function (t) {
        return now - t < 5 * 60 * 1000;
      });
      if (tries.length >= 2) return false;
      tries.push(now);
      sessionStorage.setItem(key, JSON.stringify(tries));
      return true;
    } catch (_error) {
      return true;
    }
  }

  // Whether another password may be entered now, counting this one.
  function mayTry() {
    var now = Date.now();
    var tries = (stored(TRIES) || []).filter(function (t) {
      return typeof t === 'number' && now - t < HOUR;
    });
    if (tries.length >= 2) return false;
    tries.push(now);
    store(TRIES, tries);
    return true;
  }

  // The sign-in page reports a problem: a wrong password, a blocked account, too many tries.
  function refused() {
    var errors = document.querySelectorAll(
      '[id^="error-element"], .ulp-input-error-message, .ulp-error-info, [role="alert"]'
    );
    for (var i = 0; i < errors.length; i++) {
      if (visible(errors[i]) && errors[i].textContent.trim()) return true;
    }
    return false;
  }

  function stop() {
    store(PAUSED, Date.now() + HOUR);
    uncover(false);
  }

  var account = null;
  var asked = false;

  // Fills whichever of the email and password fields the page shows (the sign-in may ask for
  // them one page at a time), then submits.
  function fillIn() {
    if (!account) return false;
    var email = document.querySelector(
      'input#username, input[name="username"], input[type="email"]'
    );
    var password = document.querySelector(
      'input#password, input[name="password"], input[type="password"]'
    );
    var filled = false;
    if (visible(email) && !email.value) {
      type(email, account.email);
      filled = true;
    }
    if (visible(password) && !password.value) {
      type(password, account.password);
      filled = true;
    }
    if (!filled) return false;
    var form = (visible(password) ? password : email).form;
    // Only the password counts: entering the email alone cannot lock the account.
    var submit =
      (form && form.querySelector('button[type="submit"][name="action"], button[type="submit"]')) ||
      document.querySelector('button[type="submit"]');
    if (visible(password) && !mayTry()) {
      // Twice already this hour: show the page and let the viewer sign in.
      stop();
      return true;
    }
    if (submit) submit.click();
    else if (form) form.submit();
    return true;
  }

  function signIn() {
    if (location.origin !== SIGN_IN_ORIGIN || asked) return;
    var request = signInRequest(launch);
    if (!request) return;
    asked = true;
    if (paused()) return uncover(false);
    fetch(request.url, {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain' },
      body: request.body,
      cache: 'no-store',
    })
      .then(function (response) {
        return response.ok ? response.json() : null;
      })
      .then(function (reply) {
        if (!reply || typeof reply.email !== 'string' || typeof reply.password !== 'string') {
          return uncover(false);
        }
        account = reply;
        // The form may still be rendering: try until it appears, for a little while.
        var attempts = 0;
        var timer = setInterval(function () {
          // An error on the page: never enter the account again over it.
          if (refused()) {
            clearInterval(timer);
            return stop();
          }
          var done = fillIn();
          if (done || ++attempts > 40) clearInterval(timer);
          // No form to fill (or the tries ran out): the viewer signs in by hand.
          if (!done && attempts > 40) uncover(false);
        }, 250);
      })
      .catch(function () {
        // No account in Home Assistant, or an older Luma: the page is signed in by hand.
        uncover(false);
      });
  }

  // On the Channel's own pages: a film opened signed out shows "Log in or subscribe". When Luma
  // gave the player a sign-in code, go to the sign-in (filled in above), then back to the film.
  function signedOut() {
    var controls = document.querySelectorAll('button, a');
    for (var i = 0; i < controls.length; i++) {
      if (visible(controls[i]) && isLogIn(controls[i].textContent)) return true;
    }
    return false;
  }

  function remember(page) {
    try {
      if (page) sessionStorage.setItem('luma-return', page);
      else sessionStorage.removeItem('luma-return');
    } catch (_error) {
      // No storage: no way back after signing in, which only costs choosing the film again.
    }
  }

  function remembered() {
    try {
      return sessionStorage.getItem('luma-return');
    } catch (_error) {
      return null;
    }
  }

  // While the page may still be drawing its prompt: sign in if it asks, else keep looking.
  // Returns true once there is nothing more to do on this page.
  function toSignIn() {
    if (location.origin !== ORIGIN) return true;
    if (!signedOut()) return false;
    var here = location.pathname + location.search;
    if (!signInRequest(parse(palm && palm.launchParams)) || !mayVisit('luma-to-sign-in')) {
      // Left to the viewer: forget any earlier trip so it can't send them elsewhere later, and
      // show them the page's prompt.
      remember(null);
      uncover(false);
      return true;
    }
    remember(here);
    location.href = loginUrl(here);
    return true;
  }

  // The page showed no sign-in prompt for the whole time it was watched: signed in. If a sign-in
  // trip landed somewhere other than its film, finish the trip.
  function backToFilm() {
    var back = remembered();
    remember(null);
    var here = location.pathname + location.search;
    if (location.pathname.indexOf('/login') === 0) return;
    if (back && back !== here && back.charAt(0) === '/' && back.charAt(1) !== '/') {
      location.href = ORIGIN + back;
    }
  }

  function start() {
    signIn();
    if (location.origin !== ORIGIN) return;
    // The page draws its sign-in prompt after it loads: look for a while.
    var attempts = 0;
    var timer = setInterval(function () {
      if (toSignIn()) return clearInterval(timer);
      if (++attempts > 20) {
        clearInterval(timer);
        backToFilm();
      }
    }, 500);
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
  else start();

  // Luma opened another film while this app was still running.
  document.addEventListener('webOSRelaunch', function (event) {
    // Another film: an earlier sign-in trip no longer applies.
    remember(null);
    var params = parse((event && event.detail) || (palm && palm.launchParams));
    var next = target(params);
    // The next page covers itself for this film (see `launchParams`).
    session(NEXT, JSON.stringify(params));
    // Luma gave the open player its sign-in before relaunching it.
    if (next !== location.href) location.href = next;
    else session(NEXT, null);
    if (palm && typeof palm.activate === 'function') palm.activate();
  });
})(typeof window === 'undefined' ? this : window);
