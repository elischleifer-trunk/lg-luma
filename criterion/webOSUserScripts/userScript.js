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

  var SKIP = 10;
  root.LumaCriterion = { ORIGIN: ORIGIN, SKIP: SKIP, target: target, parse: parse, action: action };
  if (typeof document === 'undefined') return;

  var palm = root.PalmSystem;

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
      if (event.target && event.target.tagName === 'VIDEO') fill(event.target);
    },
    true
  );

  // Luma opened another film while this app was still running.
  document.addEventListener('webOSRelaunch', function (event) {
    var params = parse((event && event.detail) || (palm && palm.launchParams));
    var next = target(params);
    if (next !== location.href) location.href = next;
    if (palm && typeof palm.activate === 'function') palm.activate();
  });
})(typeof window === 'undefined' ? this : window);
