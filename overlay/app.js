// Luma's camera overlay: a transparent webOS "overlay" window, so the app underneath keeps
// playing. Luma launches (or relaunches) it with the camera details as launch parameters and
// shows one doorbell-style card, or (mode "video") an enlarged camera from Luma's Cameras app as
// Home Assistant's HLS video, full screen, decoded by the TV. A card given an HLS playlist plays
// it too, muted, over its motion-JPEG feed once the video is playing.
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
  var cardVideo = document.getElementById('card-video');
  // A video that shows nothing by then gives the screen back to the motion-JPEG stream.
  var VIDEO_WAIT_MS = 15000;
  // A playing video whose picture stops moving this long has stalled for good.
  var VIDEO_STALL_MS = 10000;
  // The Cameras video's cover comes off once it has played this long, so its first frames show.
  var VIDEO_SETTLE_S = 0.3;
  // Cover snapshots load back to back, at most this often.
  var COVER_GAP_MS = 250;
  var video = null;
  var cardPlayback = null;
  var current = null;
  var timers = [];
  var feedTimer = null;
  // Bumped by stopFeed, so snapshot frames still loading from an earlier feed are dropped.
  var feedRun = 0;
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
    stopFeed();
    stopCardVideo();
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
    var run = feedRun;
    function next() {
      if (loading) return;
      loading = true;
      var image = new Image();
      image.onload = function () {
        if (run !== feedRun) return;
        loading = false;
        feed.src = image.src;
        showStatus('');
      };
      image.onerror = function () {
        if (run !== feedRun) return;
        loading = false;
        showStatus('Camera unavailable');
      };
      image.src = logic.frameUrl(settings.snapshot, Date.now());
    }
    next();
    feedTimer = setInterval(next, settings.refresh * 1000);
  }

  // The card's motion-JPEG stream, falling back to snapshots.
  function startFeed(settings) {
    stopFeed();
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
  }

  function stopFeed() {
    feedRun += 1;
    clearInterval(feedTimer);
    feedTimer = null;
    feed.onload = null;
    feed.onerror = null;
    feed.removeAttribute('src');
  }

  // ---- HLS video ---------------------------------------------------------------------------

  function start(element) {
    var playing = element.play();
    // Autoplay with sound can be refused; play muted rather than not at all.
    if (playing && playing.catch) {
      playing.catch(function () {
        element.muted = true;
        element.play();
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

  // Plays an HLS playlist in a <video>: the TV's own player first, where it says it plays HLS;
  // hls.js through Media Source Extensions otherwise or if that fails. With `options.hlsFirst`,
  // hls.js goes first instead: it reads Home Assistant's low-latency parts, so video starts
  // within a couple of seconds, where the TV's player may wait for whole segments. `onPlaying`
  // runs once, when the first frame shows (with `options.settle`, once the video has advanced
  // VIDEO_SETTLE_S); `onFail(wasPlaying)` once, when it cannot play, ends, stalls for
  // VIDEO_STALL_MS, or shows nothing within VIDEO_WAIT_MS. `stop()` ends it without calling either.
  function playHls(element, url, onPlaying, onFail, options) {
    options = options || {};
    var playback = { stopped: false, playing: false, hls: null, wait: null, watch: null };
    var native = !!(element.canPlayType && element.canPlayType('application/vnd.apple.mpegurl'));
    function fail() {
      if (playback.stopped) return;
      var wasPlaying = playback.playing;
      playback.stop();
      onFail(wasPlaying);
    }
    function withNative() {
      element.onerror = function () {
        element.onerror = null;
        if (playback.playing || options.hlsFirst) fail();
        else withHlsJs();
      };
      element.src = url;
      start(element);
    }
    // hls.js going first hands over to the TV's player if it cannot play before anything shows.
    function nativeFallback() {
      if (options.hlsFirst && native && !playback.playing) withNative();
      else fail();
    }
    function withHlsJs() {
      loadHls(function (Hls) {
        if (playback.stopped) return;
        if (!Hls || !Hls.isSupported()) {
          nativeFallback();
          return;
        }
        element.removeAttribute('src');
        playback.hls = new Hls({ lowLatencyMode: true, backBufferLength: 10, maxBufferLength: 10 });
        playback.hls.on(Hls.Events.ERROR, function (_event, data) {
          if (!data || !data.fatal) return;
          playback.hls.destroy();
          playback.hls = null;
          nativeFallback();
        });
        playback.hls.on(Hls.Events.MANIFEST_PARSED, function () {
          start(element);
        });
        playback.hls.loadSource(url);
        playback.hls.attachMedia(element);
      });
    }
    playback.stop = function () {
      playback.stopped = true;
      clearTimeout(playback.wait);
      clearInterval(playback.watch);
      if (playback.hls) {
        playback.hls.destroy();
        playback.hls = null;
      }
      element.onplaying = null;
      element.ontimeupdate = null;
      element.onerror = null;
      element.onended = null;
      element.pause();
      element.removeAttribute('src');
      try {
        element.load();
      } catch (_error) {
        // Nothing was loading.
      }
    };
    function began() {
      if (playback.playing) return;
      playback.playing = true;
      clearTimeout(playback.wait);
      // A live stream that stops updating freezes on its last frame without an error event.
      var position = element.currentTime;
      var still = 0;
      playback.watch = setInterval(function () {
        if (element.currentTime !== position) {
          position = element.currentTime;
          still = 0;
        } else if ((still += 1000) >= VIDEO_STALL_MS) {
          fail();
        }
      }, 1000);
      onPlaying();
    }
    if (options.settle) {
      // Shown once the video has advanced a little past its first frame: webOS shows hardware
      // video on a plane behind the page that is black until frames decode.
      var startedAt = null;
      element.ontimeupdate = function () {
        if (playback.playing) return;
        if (startedAt === null) startedAt = element.currentTime;
        if (element.currentTime - startedAt >= VIDEO_SETTLE_S) began();
      };
    } else {
      element.onplaying = began;
    }
    element.onended = fail;
    playback.wait = setTimeout(function () {
      if (!playback.playing) fail();
    }, VIDEO_WAIT_MS);
    if (native && !options.hlsFirst) withNative();
    else withHlsJs();
    return playback;
  }

  // ---- Camera video (Cameras app) ----------------------------------------------------------

  function stopVideo() {
    if (video) {
      video.stop();
      video = null;
    }
    videoCover.hidden = true;
    videoCover.removeAttribute('src');
    videoView.hidden = true;
    videoView.className = 'video';
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
    stopFeed();
    stopCardVideo();
    stopVideo();
    card.hidden = true;
    closing = false;
    current = settings;
    document.getElementById('video-title').textContent = settings.title;
    videoLive.hidden = true;
    videoView.hidden = false;
    cover(settings);
    video = playHls(
      player,
      settings.url,
      function () {
        videoView.className = 'video playing';
        videoCover.hidden = true;
        videoLive.hidden = false;
      },
      function () {
        dismiss(true);
      },
      { hlsFirst: true, settle: true }
    );
  }

  // ---- Doorbell card -----------------------------------------------------------------------

  function stopCardVideo() {
    if (cardPlayback) {
      cardPlayback.stop();
      cardPlayback = null;
    }
    card.classList.remove('playing');
  }

  // Full-frame-rate video in the card, over the motion-JPEG once it plays. The motion-JPEG stays
  // connected underneath: its URL's camera token may have rotated by the time a long card's video
  // fails, so reconnecting then could be refused. A video that fails just uncovers it.
  function playCardVideo(settings) {
    cardVideo.muted = true;
    cardPlayback = playHls(
      cardVideo,
      settings.hls,
      function () {
        card.classList.add('playing');
      },
      function () {
        cardPlayback = null;
        card.classList.remove('playing');
      }
    );
  }

  function show(settings) {
    cancelClose();
    clearTimers();
    stopCardVideo();
    stopVideo();
    closing = false;
    current = settings;
    card.className = 'card ' + settings.position + ' ' + settings.size;
    document.getElementById('title').textContent = settings.title;
    var message = document.getElementById('message');
    message.textContent = settings.message;
    message.hidden = !settings.message;
    startFeed(settings);
    if (settings.hls) playCardVideo(settings);
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
    var enlarged = logic.video(params);
    if (enlarged) {
      playVideo(enlarged);
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
    var enlarged = !videoView.hidden;
    if (event.keyCode === 461 || event.keyCode === 27 || (enlarged && event.keyCode === 13)) {
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
