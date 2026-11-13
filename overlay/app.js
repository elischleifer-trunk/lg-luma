// Luma's camera overlay: a transparent webOS "overlay" window, so the app underneath keeps
// playing. Luma launches (or relaunches) it with the camera details as launch parameters and
// shows one doorbell-style card, or (mode "video") an enlarged camera from Luma's Cameras app as
// Home Assistant's HLS video, full screen, decoded by the TV.
(function () {
  'use strict';
  var logic = window.LumaOverlay;
  var card = document.getElementById('card');
  var feed = document.getElementById('feed');
  var status = document.getElementById('status');
  var bar = document.getElementById('bar');
  var videoView = document.getElementById('video');
  var player = document.getElementById('player');
  var videoCover = document.getElementById('video-cover');
  var videoLive = document.getElementById('video-live');
  // A video that shows nothing by then gives the screen back to Luma's motion-JPEG stream.
  var VIDEO_WAIT_MS = 15000;
  // The cover comes off once the video has played this long, so its first frames are on screen.
  var VIDEO_SETTLE_S = 0.3;
  // Cover snapshots load back to back, at most this often.
  var COVER_GAP_MS = 250;
  var hls = null;
  var current = null;
  var timers = [];
  var closing = false;
  var closeTimer = null;

  function launchParams() {
    var raw;
    try {
      raw =
        (window.PalmSystem && window.PalmSystem.launchParams) ||
        (window.webOSSystem && window.webOSSystem.launchParams);
    } catch (_error) {
      raw = null;
    }
    if (typeof raw === 'string') {
      try {
        return JSON.parse(raw || '{}');
      } catch (_error) {
        return {};
      }
    }
    return raw || {};
  }

  function clearTimers() {
    timers.forEach(function (timer) {
      clearTimeout(timer);
      clearInterval(timer);
    });
    timers = [];
  }

  function showStatus(text) {
    status.textContent = text;
    status.hidden = !text;
  }

  // Tell Luma the overlay closed so it can resume paused playback. Opaque "no-cors" requests
  // need no CORS reply; Luma accepts this endpoint only from the TV itself with this nonce.
  // `failed`: a camera video could not play, so the Cameras app keeps its own stream on screen.
  function report(settings, failed) {
    if (!settings || !settings.session) return;
    var url = logic.closedUrl(settings.port);
    var body = JSON.stringify({
      session: settings.session,
      nonce: settings.nonce,
      failed: failed === true,
    });
    try {
      if (navigator.sendBeacon && navigator.sendBeacon(url, body)) return;
    } catch (_error) {
      // Fall through to fetch.
    }
    try {
      fetch(url, { method: 'POST', mode: 'no-cors', body: body, keepalive: true });
    } catch (_error) {
      // Luma also ends the session after the overlay's duration.
    }
  }

  function dismiss(failed) {
    if (closing) return;
    closing = true;
    clearTimers();
    feed.removeAttribute('src');
    card.hidden = true;
    stopVideo();
    report(current, failed);
    closeTimer = setTimeout(function () {
      window.close();
    }, 150);
  }

  // A page suspended while hidden may resume through a relaunch before its close fires.
  function cancelClose() {
    clearTimeout(closeTimer);
    closeTimer = null;
  }

  function snapshots(settings) {
    var loading = false;
    function next() {
      if (loading) return;
      loading = true;
      var image = new Image();
      image.onload = function () {
        loading = false;
        feed.src = image.src;
        showStatus('');
      };
      image.onerror = function () {
        loading = false;
        showStatus('Camera unavailable');
      };
      image.src = logic.frameUrl(settings.snapshot, Date.now());
    }
    next();
    timers.push(setInterval(next, settings.refresh * 1000));
  }

  // ---- Camera video (Cameras app) ----------------------------------------------------------

  function stopVideo() {
    if (hls) {
      hls.destroy();
      hls = null;
    }
    player.onplaying = null;
    player.ontimeupdate = null;
    player.onerror = null;
    videoCover.hidden = true;
    videoCover.removeAttribute('src');
    player.pause();
    player.removeAttribute('src');
    try {
      player.load();
    } catch (_error) {
      // Nothing was loading.
    }
    videoView.hidden = true;
    videoView.className = 'video';
  }

  function start() {
    var playing = player.play();
    // Autoplay with sound can be refused; play muted rather than not at all.
    if (playing && playing.catch) {
      playing.catch(function () {
        player.muted = true;
        player.play();
      });
    }
  }

  function loadHls(done) {
    if (window.Hls) return done(window.Hls);
    var script = document.createElement('script');
    script.src = 'vendor/hls.light.min.js';
    script.onload = function () {
      done(window.Hls);
    };
    script.onerror = function () {
      done(null);
    };
    document.head.appendChild(script);
  }

  function nativeHls() {
    return !!(player.canPlayType && player.canPlayType('application/vnd.apple.mpegurl'));
  }

  // The TV's own player: it may ignore Home Assistant's low-latency parts and wait for whole
  // segments, so it is the fallback.
  function playNative(settings, isPlaying) {
    player.onerror = function () {
      player.onerror = null;
      dismiss(true);
    };
    if (isPlaying()) return;
    player.src = settings.url;
    start();
  }

  // hls.js first: it reads Home Assistant's low-latency HLS parts, so the video starts within a
  // couple of seconds and stays close to live. It plays through Media Source Extensions.
  function playWithHlsJs(settings, isPlaying) {
    loadHls(function (Hls) {
      if (closing || current !== settings) return;
      if (!Hls || !Hls.isSupported()) {
        if (nativeHls()) playNative(settings, isPlaying);
        else dismiss(true);
        return;
      }
      hls = new Hls({
        lowLatencyMode: true,
        backBufferLength: 10,
        maxBufferLength: 10,
      });
      hls.on(Hls.Events.ERROR, function (_event, data) {
        if (!data || !data.fatal) return;
        hls.destroy();
        hls = null;
        // Before anything played, the TV's own player gets a try.
        if (!isPlaying() && nativeHls()) playNative(settings, isPlaying);
        else dismiss(true);
      });
      hls.on(Hls.Events.MANIFEST_PARSED, start);
      hls.loadSource(settings.url);
      hls.attachMedia(player);
    });
  }

  // The camera's snapshots, refreshed, over the video until it plays.
  function cover(settings) {
    if (!settings.snapshot) return;
    function next() {
      if (closing || current !== settings || videoView.className === 'video playing') return;
      var started = Date.now();
      var image = new Image();
      image.onload = function () {
        if (current !== settings || videoView.className === 'video playing') return;
        videoCover.src = image.src;
        videoCover.hidden = false;
        timers.push(setTimeout(next, Math.max(0, COVER_GAP_MS - (Date.now() - started))));
      };
      image.onerror = function () {
        timers.push(setTimeout(next, 1000));
      };
      image.src = logic.frameUrl(settings.snapshot, Date.now());
    }
    next();
  }

  function playVideo(settings) {
    cancelClose();
    clearTimers();
    stopVideo();
    card.hidden = true;
    closing = false;
    current = settings;
    document.getElementById('video-title').textContent = settings.title;
    videoLive.hidden = true;
    videoView.hidden = false;
    var playing = false;
    var startedAt = null;
    // Shown once the video has advanced a little past its first frame, so the cover never
    // gives way to the black video plane.
    player.ontimeupdate = function () {
      if (playing) return;
      if (startedAt === null) startedAt = player.currentTime;
      if (player.currentTime - startedAt < VIDEO_SETTLE_S) return;
      playing = true;
      videoView.className = 'video playing';
      videoCover.hidden = true;
      videoLive.hidden = false;
    };
    function isPlaying() {
      return playing;
    }
    timers.push(
      setTimeout(function () {
        if (!playing) dismiss(true);
      }, VIDEO_WAIT_MS)
    );
    cover(settings);
    playWithHlsJs(settings, isPlaying);
  }

  function show(settings) {
    cancelClose();
    clearTimers();
    stopVideo();
    closing = false;
    current = settings;
    card.className = 'card ' + settings.position + ' ' + settings.size;
    document.getElementById('title').textContent = settings.title;
    var message = document.getElementById('message');
    message.textContent = settings.message;
    message.hidden = !settings.message;
    showStatus('Connecting to camera…');
    feed.onload = function () {
      showStatus('');
    };
    feed.onerror = function () {
      // A motion-JPEG stream that fails falls back to still snapshots when one was given.
      // Snapshot frames are decoded before being shown, so they never reach this handler.
      feed.onerror = null;
      if (settings.stream && settings.snapshot) {
        snapshots(settings);
      } else {
        showStatus('Camera unavailable');
      }
    };
    if (settings.stream) {
      feed.src = settings.stream;
    } else {
      snapshots(settings);
    }
    // Restart the countdown animation for a replaced overlay.
    bar.style.transition = 'none';
    bar.style.transform = 'scaleX(1)';
    void bar.offsetWidth;
    bar.style.transition = 'transform ' + settings.duration + 's linear';
    bar.style.transform = 'scaleX(0)';
    card.hidden = false;
    timers.push(setTimeout(dismiss, settings.duration * 1000));
  }

  function apply(params) {
    var video = logic.video(params);
    if (video) {
      playVideo(video);
      return;
    }
    var settings = logic.settings(params);
    if (!settings || settings.dismiss) {
      dismiss();
      return;
    }
    show(settings);
  }

  document.addEventListener('keydown', function (event) {
    // 461 is the webOS remote's Back key; Escape helps when testing in a desktop browser.
    // OK also closes a camera video, as it does an enlarged camera in Luma.
    var video = !videoView.hidden;
    if (event.keyCode === 461 || event.keyCode === 27 || (video && event.keyCode === 13)) {
      event.preventDefault();
      dismiss();
    }
  });
  window.addEventListener('pagehide', function () {
    report(current);
  });

  document.addEventListener('webOSRelaunch', function (event) {
    apply((event && event.detail) || launchParams());
  });
  apply(launchParams());
})();
