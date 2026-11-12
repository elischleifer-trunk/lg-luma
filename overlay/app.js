// Luma's camera overlay: a transparent webOS "overlay" window, so the app underneath keeps
// playing. Luma launches (or relaunches) it with the camera details as launch parameters and
// shows one doorbell-style card. (The Cameras app is drawn by Luma itself.)
(function () {
  'use strict';
  var logic = window.LumaOverlay;
  var card = document.getElementById('card');
  var feed = document.getElementById('feed');
  var status = document.getElementById('status');
  var bar = document.getElementById('bar');
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
  function report(settings) {
    if (!settings || !settings.session) return;
    var url = logic.closedUrl(settings.port);
    var body = JSON.stringify({ session: settings.session, nonce: settings.nonce });
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

  function dismiss() {
    if (closing) return;
    closing = true;
    clearTimers();
    feed.removeAttribute('src');
    card.hidden = true;
    report(current);
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

  function show(settings) {
    cancelClose();
    clearTimers();
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
    var settings = logic.settings(params);
    if (!settings || settings.dismiss) {
      dismiss();
      return;
    }
    show(settings);
  }

  document.addEventListener('keydown', function (event) {
    // 461 is the webOS remote's Back key; Escape helps when testing in a desktop browser.
    if (event.keyCode === 461 || event.keyCode === 27) {
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
