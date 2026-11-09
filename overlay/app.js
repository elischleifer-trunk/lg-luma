// Luma's camera overlay: a transparent webOS "overlay" window, so the app underneath keeps
// playing. Luma launches (or relaunches) it with the camera details as launch parameters:
// either one doorbell-style card, or (mode "cameras") the full-screen Cameras gallery.
(function () {
  'use strict';
  var logic = window.LumaOverlay;
  var card = document.getElementById('card');
  var feed = document.getElementById('feed');
  var status = document.getElementById('status');
  var bar = document.getElementById('bar');
  var gallery = document.getElementById('gallery');
  var grid = document.getElementById('grid');
  var single = document.getElementById('single');
  var singleFeed = document.getElementById('single-feed');
  var singleStatus = document.getElementById('single-status');
  var current = null;
  var timers = [];
  var galleryTimers = [];
  var closing = false;
  var closeTimer = null;
  // Gallery state: one entry per camera tile, the selected index, and the enlarged camera.
  var tiles = [];
  var selected = 0;
  var enlarged = null;

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

  function clear(list) {
    list.forEach(function (timer) {
      clearTimeout(timer);
      clearInterval(timer);
    });
    list.length = 0;
  }

  function clearTimers() {
    clear(timers);
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
    closeGallery();
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
    closeGallery();
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

  // ---- Cameras gallery -------------------------------------------------------------------

  function setStatus(element, text) {
    element.textContent = text;
    element.hidden = !text;
  }

  // Loads the next snapshot off-screen and swaps it in once decoded, so frames never flash.
  function poll(tile) {
    var camera = tile.camera;
    if (tile.loading || !camera.snapshot) return;
    tile.loading = true;
    var image = new Image();
    image.onload = function () {
      tile.loading = false;
      if (tiles.indexOf(tile) < 0) return;
      tile.img.src = image.src;
      setStatus(tile.status, '');
    };
    image.onerror = function () {
      tile.loading = false;
      if (tiles.indexOf(tile) < 0) return;
      if (!tile.img.getAttribute('src')) setStatus(tile.status, 'Camera unavailable');
    };
    image.src = logic.frameUrl(camera.snapshot, Date.now());
  }

  function startPolling() {
    clear(galleryTimers);
    if (enlarged) return;
    tiles.forEach(function (tile, index) {
      if (tile.camera.snapshot) {
        // Stagger the first frames so a large grid does not hit Home Assistant all at once.
        galleryTimers.push(
          setTimeout(function () {
            poll(tile);
          }, index * 120),
        );
      } else if (tile.img.getAttribute('src') !== tile.camera.stream) {
        // No snapshot URL: show the motion-JPEG stream itself.
        tile.img.src = tile.camera.stream;
      }
    });
    // Larger grids refresh less often to keep the load on Home Assistant and the TV small.
    var every = tiles.length > 9 ? 4000 : 2000;
    galleryTimers.push(
      setInterval(function () {
        tiles.forEach(poll);
      }, every),
    );
  }

  function highlight() {
    tiles.forEach(function (tile, index) {
      tile.element.className = index === selected ? 'tile selected' : 'tile';
    });
    var tile = tiles[selected];
    if (tile && tile.element.scrollIntoView) tile.element.scrollIntoView({ block: 'nearest' });
  }

  function buildGrid(cameras) {
    grid.textContent = '';
    tiles = [];
    var cols = logic.columns(cameras.length);
    grid.className = 'grid cols-' + cols;
    cameras.forEach(function (camera, index) {
      var element = document.createElement('div');
      var frame = document.createElement('div');
      var img = document.createElement('img');
      var status = document.createElement('p');
      var name = document.createElement('p');
      frame.className = 'frame';
      img.alt = '';
      img.onerror = function () {
        if (!camera.snapshot) setStatus(status, 'Camera unavailable');
      };
      status.className = 'status';
      name.className = 'name';
      name.textContent = camera.name;
      setStatus(status, 'Connecting to camera…');
      frame.appendChild(img);
      frame.appendChild(status);
      element.appendChild(frame);
      element.appendChild(name);
      element.addEventListener('click', function () {
        selected = index;
        highlight();
        enlarge();
      });
      grid.appendChild(element);
      tiles.push({ camera: camera, element: element, img: img, status: status, loading: false });
    });
  }

  function showGallery(settings) {
    cancelClose();
    clearTimers();
    feed.removeAttribute('src');
    card.hidden = true;
    closing = false;
    var previous = tiles[selected] && tiles[selected].camera.id;
    var sameCameras =
      !gallery.hidden &&
      tiles.length === settings.cameras.length &&
      tiles.every(function (tile, index) {
        return tile.camera.id === settings.cameras[index].id;
      });
    current = settings;
    if (sameCameras) {
      // Fresh URLs for the same cameras (Home Assistant rotated its tokens): keep the frames
      // on screen and load the next ones from the new addresses.
      tiles.forEach(function (tile, index) {
        tile.camera = settings.cameras[index];
      });
    } else {
      buildGrid(settings.cameras);
      selected = 0;
      settings.cameras.forEach(function (camera, index) {
        if (camera.id === previous) selected = index;
      });
    }
    if (enlarged) {
      var match = settings.cameras.filter(function (camera) {
        return camera.id === enlarged.id;
      })[0];
      if (match) {
        // A connected motion-JPEG stream keeps working; only snapshot fallbacks need new URLs.
        enlarged = match;
      } else {
        shrink();
      }
    }
    gallery.hidden = false;
    highlight();
    // An enlarged camera keeps its own refresh, which reads the new URLs from `enlarged`.
    if (!enlarged) startPolling();
  }

  function enlarge() {
    var tile = tiles[selected];
    if (!tile) return;
    var camera = tile.camera;
    enlarged = camera;
    clear(galleryTimers);
    document.getElementById('single-name').textContent = camera.name;
    single.hidden = false;
    setStatus(singleStatus, 'Connecting to camera…');
    // Start from the grid's latest frame while the live stream connects.
    if (tile.img.getAttribute('src')) singleFeed.src = tile.img.src;
    function snapshots() {
      var loading = false;
      function next() {
        if (loading || !enlarged || !enlarged.snapshot) return;
        loading = true;
        var image = new Image();
        image.onload = function () {
          loading = false;
          if (!enlarged) return;
          singleFeed.src = image.src;
          setStatus(singleStatus, '');
        };
        image.onerror = function () {
          loading = false;
          if (enlarged) setStatus(singleStatus, 'Camera unavailable');
        };
        image.src = logic.frameUrl(enlarged.snapshot, Date.now());
      }
      next();
      galleryTimers.push(setInterval(next, 1000));
    }
    if (camera.stream) {
      singleFeed.onload = function () {
        setStatus(singleStatus, '');
      };
      singleFeed.onerror = function () {
        singleFeed.onerror = null;
        if (enlarged && enlarged.snapshot) snapshots();
        else setStatus(singleStatus, 'Camera unavailable');
      };
      singleFeed.src = camera.stream;
    } else {
      snapshots();
    }
  }

  function shrink() {
    enlarged = null;
    clear(galleryTimers);
    singleFeed.onerror = null;
    singleFeed.onload = null;
    // Dropping the source closes the motion-JPEG connection.
    singleFeed.removeAttribute('src');
    single.hidden = true;
    startPolling();
  }

  function closeGallery() {
    clear(galleryTimers);
    enlarged = null;
    singleFeed.onerror = null;
    singleFeed.removeAttribute('src');
    single.hidden = true;
    gallery.hidden = true;
    tiles.forEach(function (tile) {
      tile.img.removeAttribute('src');
    });
    tiles = [];
    grid.textContent = '';
  }

  single.addEventListener('click', shrink);

  function apply(params) {
    var cameras = logic.gallery(params);
    if (cameras) {
      showGallery(cameras);
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
    var key = event.keyCode;
    // 461 is the webOS remote's Back key; Escape helps when testing in a desktop browser.
    if (key === 461 || key === 27) {
      event.preventDefault();
      if (enlarged) shrink();
      else dismiss();
      return;
    }
    if (gallery.hidden || !tiles.length) return;
    if (key === 13) {
      event.preventDefault();
      if (enlarged) shrink();
      else enlarge();
    } else if (key >= 37 && key <= 40 && !enlarged) {
      event.preventDefault();
      selected = logic.move(selected, key, tiles.length, logic.columns(tiles.length));
      highlight();
    }
  });
  // Home, another app or webOS can take the gallery away without Back. Report it closed so
  // Luma stops relaunching it with fresh camera URLs. The doorbell card keeps its own timer.
  function leftGallery() {
    if (!gallery.hidden) dismiss();
  }
  document.addEventListener('visibilitychange', function () {
    if (document.visibilityState === 'hidden') leftGallery();
  });
  window.addEventListener('pagehide', function () {
    if (gallery.hidden) report(current);
    else leftGallery();
  });

  document.addEventListener('webOSRelaunch', function (event) {
    apply((event && event.detail) || launchParams());
  });
  apply(launchParams());
})();
