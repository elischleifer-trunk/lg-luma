// Luma's camera overlay: a transparent webOS "overlay" window, so the app underneath keeps
// playing. Luma launches (or relaunches) it with the camera details as launch parameters and
// shows one doorbell-style card, or (mode "video") an enlarged camera from Luma's Cameras app as
// Home Assistant's HLS video, full screen, decoded by the TV. A card given an HLS playlist plays
// it too, muted, over its motion-JPEG feed once the video is playing. OK on a card expands it to
// full screen, video and all, and stops its countdown. The Cameras video keeps its last two
// minutes: Left and Right scrub through them, OK pauses and plays, Back closes. Scrubbing back
// past them, or Up and Down to what UniFi Protect saw, hands over to Luma's own timeline. In
// mode "recorded" it plays what UniFi Protect recorded, from a moment Luma chose; any key the
// Cameras app handles closes it and tells Luma where it was, so Luma carries on from there.
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
  var videoScrub = document.getElementById('video-scrub');
  var videoTrack = videoScrub.querySelector('.scrub-track');
  var cardVideo = document.getElementById('card-video');
  // A video that shows nothing by then gives the screen back to the motion-JPEG stream.
  var VIDEO_WAIT_MS = 15000;
  // A playing video whose picture stops moving this long has stalled for good.
  var VIDEO_STALL_MS = 10000;
  // The Cameras video's cover comes off once it has played this long, so its first frames show.
  var VIDEO_SETTLE_S = 0.3;
  // Cover snapshots load back to back, at most this often.
  var COVER_GAP_MS = 250;
  // The Cameras video keeps this much behind its playhead to rewind into; Left and Right skip.
  var REWIND_S = 120;
  var SKIP_S = 5;
  var video = null;
  var cardPlayback = null;
  var current = null;
  var expanded = false;
  var countdown = null;
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
    clearTimeout(countdown);
    countdown = null;
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
  function report(settings, failed, handOff) {
    var fields = { failed: failed === true };
    if (handOff) {
      fields.key = handOff.key;
      fields.behind = handOff.behind;
      fields.paused = handOff.paused;
    }
    send(settings, logic.closedUrl(settings && settings.port), fields);
  }

  function send(settings, url, extra) {
    if (!settings || !settings.session) return;
    var fields = { session: settings.session, nonce: settings.nonce };
    Object.keys(extra).forEach(function (key) {
      fields[key] = extra[key];
    });
    var body = JSON.stringify(fields);
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

  // Closes, telling the Cameras app the key and how far behind live the video was, so it
  // carries on from there in its own timeline.
  function handOff(key, behind, paused) {
    dismiss(false, { key: key, behind: Math.max(0, behind), paused: paused === true });
  }

  function dismiss(failed, handed) {
    if (closing) return;
    closing = true;
    clearTimers();
    stopFeed();
    stopCardVideo();
    card.hidden = true;
    expanded = false;
    stopVideo();
    report(current, failed, handed);
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
    var playback = {
      stopped: false,
      playing: false,
      hls: null,
      wait: null,
      watch: null,
      startedAt: null,
    };
    var native = !!(element.canPlayType && element.canPlayType('application/vnd.apple.mpegurl'));
    function fail() {
      if (playback.stopped) return;
      var wasPlaying = playback.playing;
      playback.stop();
      onFail(wasPlaying);
    }
    // Whichever player is trying gets the whole wait to show something.
    function arm() {
      clearTimeout(playback.wait);
      playback.wait = setTimeout(function () {
        if (!playback.playing) fail();
      }, VIDEO_WAIT_MS);
    }
    // The other player takes over: progress is measured on its own timeline, with a fresh wait
    // (the TV's player can need most of it to fetch whole segments).
    function handOver() {
      playback.startedAt = null;
      arm();
    }
    function withNative() {
      element.onerror = function () {
        element.onerror = null;
        if (playback.playing || options.hlsFirst) fail();
        else {
          handOver();
          withHlsJs();
        }
      };
      element.src = url;
      start(element);
    }
    // hls.js going first hands over to the TV's player if it cannot play before anything shows.
    function nativeFallback() {
      if (options.hlsFirst && native && !playback.playing) {
        handOver();
        withNative();
      } else fail();
    }
    function withHlsJs() {
      loadHls(function (Hls) {
        if (playback.stopped) return;
        if (!Hls || !Hls.isSupported()) {
          nativeFallback();
          return;
        }
        element.removeAttribute('src');
        // To rewind, the video keeps what it played, and keeps loading while paused or behind,
        // since Home Assistant's playlist only lists the newest few seconds.
        playback.hls = new Hls(
          options.rewind
            ? {
                lowLatencyMode: true,
                backBufferLength: REWIND_S,
                maxBufferLength: REWIND_S + 10,
                maxMaxBufferLength: REWIND_S + 10,
              }
            : { lowLatencyMode: true, backBufferLength: 10, maxBufferLength: 10 }
        );
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
        // Paused on purpose (scrubbing the Cameras video) is not a stall.
        if (element.paused) {
          position = element.currentTime;
          still = 0;
        } else if (element.currentTime !== position) {
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
      element.ontimeupdate = function () {
        if (playback.playing) return;
        if (playback.startedAt === null) playback.startedAt = element.currentTime;
        if (element.currentTime - playback.startedAt >= VIDEO_SETTLE_S) began();
      };
    } else {
      element.onplaying = began;
    }
    element.onended = fail;
    arm();
    if (native && !options.hlsFirst) withNative();
    else withHlsJs();
    return playback;
  }

  // ---- Camera video (Cameras app) ----------------------------------------------------------

  // The seconds of video kept, oldest to newest.
  function kept() {
    var ranges = player.buffered;
    if (!ranges || !ranges.length) return null;
    return { start: ranges.start(0), end: ranges.end(ranges.length - 1) };
  }

  function playing() {
    return !!video && videoView.className.indexOf('playing') >= 0;
  }

  function goLive() {
    var range = kept();
    var edge = video && video.hls && video.hls.liveSyncPosition;
    if (range) player.currentTime = edge || Math.max(range.start, range.end - 1);
    start(player);
  }

  // Left and Right: SKIP_S back or forward, playing or paused as it was; forward to the newest
  // video goes live and plays.
  // The live video's lag behind real time is about its own live delay on top of what it shows.
  var LIVE_DELAY_S = 2;

  function behindLive() {
    var range = kept();
    return range ? range.end - player.currentTime + LIVE_DELAY_S : 0;
  }

  function scrub(step) {
    var range = kept();
    if (!range) return;
    // Back past what the video kept: Luma's timeline (and Protect's recordings) go further.
    if (step < 0 && player.currentTime + step < range.start - 0.5 && current.history) {
      handOff('left', range.end - range.start + LIVE_DELAY_S, player.paused);
      return;
    }
    var next = logic.seek(player.currentTime, range.start, range.end, step);
    if (next === null) goLive();
    else player.currentTime = next;
    showScrub();
  }

  function togglePause() {
    if (player.paused) start(player);
    else player.pause();
    showScrub();
  }

  function showScrub() {
    if (!playing()) return;
    var range = kept();
    var span = range ? range.end - range.start : 0;
    var lag = range ? logic.behind(player.currentTime, range.end) : 0;
    var live = lag === 0 && !player.paused;
    videoScrub.hidden = span < 1;
    videoView.className = 'video playing' + (span < 1 ? '' : ' scrubbing');
    videoLive.className = 'video-badge' + (live ? ' live-badge' : '');
    videoLive.textContent = live ? 'Live' : player.paused ? 'Paused' : logic.clock(lag);
    if (span < 1) return;
    var played = Math.min(1, Math.max(0, (player.currentTime - range.start) / span));
    var percent = played * 100 + '%';
    videoTrack.className = 'scrub-track' + (live ? ' live' : '');
    document.getElementById('video-played').style.width = percent;
    document.getElementById('video-knob').style.left = percent;
    document.getElementById('video-position').textContent = live ? 'Live' : logic.clock(lag);
    document.getElementById('video-window').textContent = logic.clock(span);
  }

  function stopVideo() {
    if (video) {
      video.stop();
      video = null;
    }
    videoCover.hidden = true;
    videoCover.removeAttribute('src');
    videoView.hidden = true;
    videoView.className = 'video';
    videoScrub.hidden = true;
    videoLive.className = 'video-badge live-badge';
    videoLive.textContent = 'Live';
  }

  // The camera's snapshots, refreshed, over the video until it plays. `once`: a picture of a
  // fixed moment (recorded video), which loads once rather than again and again.
  function cover(settings, once) {
    if (!settings.snapshot) return;
    function next() {
      if (closing || current !== settings || playing()) return;
      var started = Date.now();
      var image = new Image();
      image.onload = function () {
        if (current !== settings || playing()) return;
        videoCover.src = image.src;
        videoCover.hidden = false;
        if (once) return;
        timers.push(setTimeout(next, Math.max(0, COVER_GAP_MS - (Date.now() - started))));
      };
      image.onerror = function () {
        if (!once) timers.push(setTimeout(next, 1000));
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
        showScrub();
        timers.push(setInterval(showScrub, 250));
      },
      function () {
        dismiss(true);
      },
      { hlsFirst: true, settle: true, rewind: true }
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

  // OK on a card: the camera full screen, its video (or feed) as it was, until Back or OK. Luma
  // hears so the session outlives the card's duration.
  function expand() {
    expanded = true;
    clearTimeout(countdown);
    countdown = null;
    card.classList.add('expanded');
    send(current, logic.expandUrl(current.port), {});
  }

  function show(settings) {
    cancelClose();
    clearTimers();
    stopCardVideo();
    stopVideo();
    closing = false;
    expanded = false;
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
    countdown = setTimeout(function () {
      dismiss();
    }, settings.duration * 1000);
  }

  // ---- Recorded video (Cameras app) --------------------------------------------------------

  // Plays UniFi Protect's recording piece by piece through Luma, which redirects each piece to
  // Home Assistant with the camera's current token. Fails (and Luma shows recorded pictures
  // instead) when nothing plays within VIDEO_WAIT_MS or a piece cannot load.
  function playRecorded(settings) {
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
    videoLive.className = 'video-badge';
    videoView.hidden = false;
    var part = null;
    var wait = null;
    cover(settings, true);
    var playback = {
      recorded: true,
      stop: function () {
        clearTimeout(wait);
        player.onplaying = null;
        player.onended = null;
        player.onerror = null;
        player.pause();
        player.removeAttribute('src');
        try {
          player.load();
        } catch (_error) {
          // Nothing was loading.
        }
      },
    };
    playback.position = function () {
      return part ? part.start + (player.currentTime || 0) : settings.start;
    };
    function load(from, first) {
      part = logic.piece(settings, from, Date.now() / 1000, first);
      if (!part) {
        handOff('live', 0, false);
        return;
      }
      clearTimeout(wait);
      wait = setTimeout(function () {
        dismiss(true);
      }, VIDEO_WAIT_MS);
      player.src = part.url;
      start(player);
    }
    player.onplaying = function () {
      clearTimeout(wait);
      videoView.className = 'video playing';
      videoCover.hidden = true;
      videoLive.hidden = false;
    };
    player.onended = function () {
      load(part.end, false);
    };
    player.onerror = function () {
      dismiss(true);
    };
    video = playback;
    load(settings.start, true);
    timers.push(
      setInterval(function () {
        videoLive.textContent = new Date(playback.position() * 1000).toLocaleTimeString();
      }, 250)
    );
  }

  function recordedBehind() {
    return Date.now() / 1000 - video.position();
  }

  // Keys on recorded video: each goes to Luma's timeline with where the video was.
  var RECORDED_KEYS = {
    13: 'ok',
    19: 'ok',
    37: 'left',
    39: 'right',
    38: 'up',
    40: 'down',
    412: 'rewind',
    417: 'forward',
  };

  function apply(params) {
    var past = logic.recorded(params);
    if (past) {
      playRecorded(past);
      return;
    }
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

  // The remote's keys on a playing camera video: OK and the media keys play and pause, Left,
  // Right, rewind and fast-forward scrub.
  var VIDEO_KEYS = {
    13: togglePause,
    415: function () {
      if (player.paused) togglePause();
    },
    19: function () {
      if (!player.paused) togglePause();
    },
    37: function () {
      scrub(-SKIP_S);
    },
    412: function () {
      scrub(-SKIP_S);
    },
    39: function () {
      scrub(SKIP_S);
    },
    417: function () {
      scrub(SKIP_S);
    },
  };

  document.addEventListener('keydown', function (event) {
    if (video && video.recorded && RECORDED_KEYS[event.keyCode]) {
      event.preventDefault();
      // The video was playing: the gallery applies the key (OK pauses) from there.
      handOff(RECORDED_KEYS[event.keyCode], recordedBehind(), false);
      return;
    }
    // Up on the live video: back to what UniFi Protect last saw, in Luma's timeline (which
    // comes back to live when there is nothing). Down has nothing later than live.
    if (playing() && current.history && event.keyCode === 38) {
      event.preventDefault();
      handOff('up', behindLive(), player.paused);
      return;
    }
    if (playing() && VIDEO_KEYS[event.keyCode]) {
      event.preventDefault();
      VIDEO_KEYS[event.keyCode]();
      return;
    }
    // 461 is the webOS remote's Back key; Escape helps when testing in a desktop browser.
    // OK also closes a camera video that is not playing yet. On a doorbell card, OK first shows
    // it full screen.
    var ok = event.keyCode === 13;
    var full = !videoView.hidden || expanded;
    if (event.keyCode === 461 || event.keyCode === 27 || (full && ok)) {
      event.preventDefault();
      dismiss();
    } else if (ok && !card.hidden) {
      event.preventDefault();
      expand();
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
