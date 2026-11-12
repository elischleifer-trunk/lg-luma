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
  var videoStatus = document.getElementById('video-status');
  var videoLive = document.getElementById('video-live');
  // A video that shows nothing by then gives the screen back to Luma's motion-JPEG stream.
  var VIDEO_WAIT_MS = 15000;
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
    player.onerror = null;
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

  // hls.js plays HLS through Media Source Extensions where the TV's own player cannot.
  function playWithHlsJs(settings) {
    loadHls(function (Hls) {
      if (closing || current !== settings) return;
      if (!Hls || !Hls.isSupported()) {
        dismiss(true);
        return;
      }
      player.removeAttribute('src');
      hls = new Hls({ lowLatencyMode: true, backBufferLength: 10 });
      hls.on(Hls.Events.ERROR, function (_event, data) {
        if (data && data.fatal) dismiss(true);
      });
      hls.on(Hls.Events.MANIFEST_PARSED, start);
      hls.loadSource(settings.url);
      hls.attachMedia(player);
    });
  }

  function playVideo(settings) {
    cancelClose();
    clearTimers();
    stopVideo();
    card.hidden = true;
    closing = false;
    current = settings;
    document.getElementById('video-title').textContent = settings.title;
    videoStatus.hidden = false;
    videoLive.hidden = true;
    videoView.hidden = false;
    var playing = false;
    player.onplaying = function () {
      playing = true;
      videoView.className = 'video playing';
      videoStatus.hidden = true;
      videoLive.hidden = false;
    };
    timers.push(
      setTimeout(function () {
        if (!playing) dismiss(true);
      }, VIDEO_WAIT_MS)
    );
    // The TV's own player first, where it says it plays HLS; hls.js otherwise or if it fails.
    var native = player.canPlayType && player.canPlayType('application/vnd.apple.mpegurl');
    if (native) {
      player.onerror = function () {
        player.onerror = null;
        if (playing) dismiss(true);
        else playWithHlsJs(settings);
      };
      player.src = settings.url;
      start();
    } else {
      playWithHlsJs(settings);
    }
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
