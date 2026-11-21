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

  // Before the Channel opens: Luma gives the player the longest-lasting sign-in it keeps.
  function prepareRequest(params) {
    return lumaRequest(params, '/api/criterion/prepare');
  }

  // Asks Luma to get the sign-in ready, then calls `then` once: on Luma's answer, on any
  // failure, or after `wait` ms, so a slow or older Luma never holds the film up.
  function prepare(params, wait, then) {
    var done = false;
    function go() {
      if (done) return;
      done = true;
      then();
    }
    var request = prepareRequest(params);
    if (!request || typeof fetch !== 'function') return go();
    setTimeout(go, wait);
    fetch(request.url, {
      method: 'POST',
      headers: { 'Content-Type': 'text/plain' },
      body: request.body,
      cache: 'no-store',
    }).then(go, go);
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

  var SKIP = 10;
  root.LumaCriterion = {
    ORIGIN: ORIGIN,
    SIGN_IN_ORIGIN: SIGN_IN_ORIGIN,
    SKIP: SKIP,
    target: target,
    parse: parse,
    action: action,
    signInRequest: signInRequest,
    prepareRequest: prepareRequest,
    prepare: prepare,
    playsFilm: playsFilm,
    launchKey: launchKey,
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
  var STYLE =
    '.luma-criterion-fill{position:fixed!important;left:0!important;top:0!important;' +
    'width:100vw!important;height:100vh!important;max-width:none!important;' +
    'max-height:none!important;margin:0!important;z-index:2147483647!important;' +
    'background:#000!important}' +
    '.luma-criterion-fill video{object-fit:contain!important}';

  function fill(video) {
    if (!document.getElementById('luma-criterion-style')) {
      var style = document.createElement('style');
      style.id = 'luma-criterion-style';
      style.textContent = STYLE;
      (document.head || document.documentElement).appendChild(style);
    }
    var box = video.closest('.jwplayer') || video.parentElement || video;
    if (box.classList.contains('luma-criterion-fill')) return;
    box.classList.add('luma-criterion-fill');
    if (box.requestFullscreen && !document.fullscreenElement) {
      // Older engines return nothing; the fixed box above already covers the screen.
      var request = box.requestFullscreen();
      if (request && request.catch) request.catch(function () {});
    }
  }

  function leave() {
    // Back to Luma, which webOS brings to the front when this app closes.
    if (palm && typeof palm.platformBack === 'function') palm.platformBack();
    else root.close();
  }

  document.addEventListener(
    'keydown',
    function (event) {
      var video = film();
      var name = action(event.keyCode, Boolean(video));
      if (!name) return;
      event.preventDefault();
      event.stopPropagation();
      if (name === 'back') return leave();
      if (!video) return;
      if (name === 'play' || (name === 'toggle' && video.paused)) video.play();
      else if (name === 'pause' || name === 'toggle') video.pause();
      else if (name === 'forward') video.currentTime = video.currentTime + SKIP;
      else if (name === 'rewind') video.currentTime = Math.max(0, video.currentTime - SKIP);
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

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', signIn);
  else signIn();

  // Luma opened another film while this app was still running.
  document.addEventListener('webOSRelaunch', function (event) {
    var params = parse((event && event.detail) || (palm && palm.launchParams));
    var next = target(params);
    // The next page covers itself for this film (see `launchParams`).
    session(NEXT, JSON.stringify(params));
    if (next === location.href) session(NEXT, null);
    else
      prepare(params, 4000, function () {
        location.href = next;
      });
    if (palm && typeof palm.activate === 'function') palm.activate();
  });
})(typeof window === 'undefined' ? this : window);
