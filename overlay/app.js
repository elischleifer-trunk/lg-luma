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
    post(logic.closedUrl(settings.port), { session: settings.session, nonce: settings.nonce });
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

  // Holding OK this long opens a camera's options instead of enlarging it.
  var HOLD_MS = 600;
  // An enlarged camera whose live stream shows nothing by then switches to snapshots.
  var STREAM_WAIT_MS = 6000;
  // Snapshots of an enlarged camera load back to back, at most this often.
  var SNAPSHOT_GAP_MS = 250;
  var hint = document.getElementById('gallery-hint');
  var gear = document.getElementById('gear');
  var empty = document.getElementById('empty');
  var singleMode = document.getElementById('single-mode');
  var menuSheet = document.getElementById('menu');
  var menuTitle = document.getElementById('menu-title');
  var menuList = document.getElementById('menu-list');
  var settingsSheet = document.getElementById('settings');
  var settingsList = document.getElementById('settings-list');
  // Every camera in the owner's order, hidden ones included; tiles show the visible ones.
  var cameras = [];
  var live = true;
  var gearFocused = false;
  var moving = false;
  // The open options sheet: { camera, items, index }.
  var menu = null;
  // The selected settings row while settings are open, otherwise -1.
  var settingsIndex = -1;
  var holdTimer = null;
  var pointerHeld = false;
  // Bumped whenever an enlarged camera opens or closes, so late frames from it are ignored.
  var playback = 0;

  function setStatus(element, text) {
    element.textContent = text;
    element.hidden = !text;
  }

  function post(url, data) {
    var body = JSON.stringify(data);
    try {
      if (navigator.sendBeacon && navigator.sendBeacon(url, body)) return;
    } catch (_error) {
      // Fall through to fetch.
    }
    try {
      fetch(url, { method: 'POST', mode: 'no-cors', body: body, keepalive: true });
    } catch (_error) {
      // Nothing else to try; the arrangement stays on screen until the gallery closes.
    }
  }

  // Luma remembers the arrangement and applies it to every later launch.
  function saveArrangement() {
    if (!current || !current.session) return;
    post(logic.arrangeUrl(current.port), {
      session: current.session,
      nonce: current.nonce,
      order: cameras.map(function (camera) {
        return camera.id;
      }),
      hidden: cameras
        .filter(function (camera) {
          return camera.hidden;
        })
        .map(function (camera) {
          return camera.id;
        }),
      live: live,
    });
  }

  function visibleCameras() {
    return cameras.filter(function (camera) {
      return !camera.hidden;
    });
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

  function hintText() {
    if (moving) return 'Arrows to move · OK when done';
    if (menu) return 'OK to choose · BACK to cancel';
    if (settingsIndex >= 0) return 'OK to change · BACK to close';
    if (gearFocused) return 'OK for settings · BACK to close';
    return 'OK to enlarge · Hold OK for options · BACK to close';
  }

  function highlight() {
    tiles.forEach(function (tile, index) {
      var className = 'tile';
      if (index === selected && !gearFocused) className += ' selected';
      if (index === selected && moving) className += ' moving';
      tile.element.className = className;
    });
    gear.className = gearFocused && !menu && settingsIndex < 0 ? 'gear focused' : 'gear';
    hint.textContent = hintText();
    var tile = tiles[selected];
    if (tile && !gearFocused && tile.element.scrollIntoView) {
      tile.element.scrollIntoView({ block: 'nearest' });
    }
  }

  function makeTile(camera) {
    var element = document.createElement('div');
    var frame = document.createElement('div');
    var img = document.createElement('img');
    var status = document.createElement('p');
    var name = document.createElement('p');
    var tile = { camera: camera, element: element, img: img, status: status, name: name };
    tile.loading = false;
    frame.className = 'frame';
    img.alt = '';
    img.onerror = function () {
      if (!tile.camera.snapshot) setStatus(status, 'Camera unavailable');
    };
    status.className = 'status';
    name.className = 'name';
    setStatus(status, 'Connecting to camera…');
    frame.appendChild(img);
    frame.appendChild(status);
    element.appendChild(frame);
    element.appendChild(name);
    // The Magic Remote's pointer: click enlarges, a long press opens the options.
    element.addEventListener('mousedown', function () {
      clearTimeout(holdTimer);
      holdTimer = setTimeout(function () {
        holdTimer = null;
        pointerHeld = true;
        selected = tiles.indexOf(tile);
        gearFocused = false;
        openMenu();
      }, HOLD_MS);
    });
    element.addEventListener('mouseleave', function () {
      clearTimeout(holdTimer);
      holdTimer = null;
    });
    element.addEventListener('click', function () {
      clearTimeout(holdTimer);
      holdTimer = null;
      if (pointerHeld) {
        pointerHeld = false;
        return;
      }
      if (menu || settingsIndex >= 0 || moving) return;
      selected = tiles.indexOf(tile);
      gearFocused = false;
      highlight();
      enlarge();
    });
    return tile;
  }

  // Shows the visible cameras in order, reusing tiles so their latest frames stay on screen.
  function renderGrid() {
    var previous = {};
    tiles.forEach(function (tile) {
      previous[tile.camera.id] = tile;
    });
    var next = visibleCameras().map(function (camera) {
      var tile = previous[camera.id] || makeTile(camera);
      delete previous[camera.id];
      tile.camera = camera;
      tile.name.textContent = camera.name;
      return tile;
    });
    Object.keys(previous).forEach(function (id) {
      previous[id].img.removeAttribute('src');
    });
    grid.textContent = '';
    grid.className = 'grid cols-' + logic.columns(next.length);
    next.forEach(function (tile) {
      grid.appendChild(tile.element);
    });
    tiles = next;
    empty.hidden = tiles.length > 0;
    if (selected >= tiles.length) selected = Math.max(0, tiles.length - 1);
    if (!tiles.length) gearFocused = true;
  }

  function showGallery(settings) {
    cancelClose();
    clearTimers();
    feed.removeAttribute('src');
    card.hidden = true;
    closing = false;
    var previous = tiles[selected] && tiles[selected].camera.id;
    var reopened = !gallery.hidden && current && current.session === settings.session;
    current = settings;
    if (reopened) {
      // Fresh URLs (Home Assistant rotated its tokens) or a changed list while open: keep the
      // arrangement on screen and take the new addresses, adding new cameras at the end.
      var fresh = {};
      settings.cameras.forEach(function (camera) {
        fresh[camera.id] = camera;
      });
      var merged = [];
      cameras.forEach(function (camera) {
        var next = fresh[camera.id];
        if (!next) return;
        next.hidden = camera.hidden;
        merged.push(next);
        delete fresh[camera.id];
      });
      settings.cameras.forEach(function (camera) {
        if (fresh[camera.id]) merged.push(camera);
      });
      cameras = merged;
    } else {
      closeGallery();
      cameras = settings.cameras.slice();
      live = settings.live;
      selected = 0;
    }
    renderGrid();
    tiles.forEach(function (tile, index) {
      if (tile.camera.id === previous) selected = index;
    });
    if (enlarged) {
      var match = visibleCameras().filter(function (camera) {
        return camera.id === enlarged.id;
      })[0];
      if (match) {
        // A connected motion-JPEG stream keeps working; snapshots read the new URLs.
        enlarged = match;
      } else {
        shrink();
      }
    }
    if (menu && cameras.indexOf(menu.camera) < 0) closeMenu();
    if (settingsIndex >= 0) renderSettings();
    gallery.hidden = false;
    highlight();
    // An enlarged camera keeps its own refresh, which reads the new URLs from `enlarged`.
    if (!enlarged) startPolling();
  }

  function setMode(text, isLive) {
    singleMode.textContent = text;
    singleMode.className = isLive ? 'single-mode live' : 'single-mode';
    singleMode.hidden = !text;
  }

  // Snapshots back to back: each one loads off-screen and the next starts once it is shown.
  function playSnapshots() {
    var run = playback;
    if (!enlarged || !enlarged.snapshot) {
      setMode('', false);
      setStatus(singleStatus, 'Camera unavailable');
      return;
    }
    setMode('Snapshots', false);
    function next() {
      if (run !== playback || !enlarged) return;
      var started = Date.now();
      var image = new Image();
      image.onload = function () {
        if (run !== playback) return;
        singleFeed.src = image.src;
        setStatus(singleStatus, '');
        later(Math.max(0, SNAPSHOT_GAP_MS - (Date.now() - started)));
      };
      image.onerror = function () {
        if (run !== playback) return;
        setStatus(singleStatus, 'Camera unavailable');
        later(2000);
      };
      image.src = logic.frameUrl(enlarged.snapshot, Date.now());
    }
    function later(wait) {
      galleryTimers.push(setTimeout(next, wait));
    }
    next();
  }

  function stopStream() {
    singleFeed.onload = null;
    singleFeed.onerror = null;
    // Dropping the source closes the motion-JPEG connection.
    singleFeed.removeAttribute('src');
  }

  // The live motion-JPEG stream. Browsers report only its first frame, so a stream that never
  // shows one (or fails) switches to snapshots rather than leaving a still on screen.
  function playStream() {
    var run = playback;
    var started = false;
    setMode('Live', true);
    singleFeed.onload = function () {
      if (run !== playback) return;
      started = true;
      setStatus(singleStatus, '');
    };
    singleFeed.onerror = function () {
      if (run !== playback) return;
      stopStream();
      playSnapshots();
    };
    galleryTimers.push(
      setTimeout(function () {
        if (run !== playback || started) return;
        stopStream();
        playSnapshots();
      }, STREAM_WAIT_MS),
    );
    singleFeed.src = enlarged.stream;
  }

  function enlarge() {
    var tile = tiles[selected];
    if (!tile) return;
    playback += 1;
    enlarged = tile.camera;
    clear(galleryTimers);
    stopStream();
    document.getElementById('single-name').textContent = enlarged.name;
    // The grid's latest frame shows underneath while the enlarged picture starts.
    var still = tile.img.getAttribute('src') ? tile.img.src : '';
    single.style.backgroundImage = still ? 'url("' + still.replace(/"/g, '%22') + '")' : '';
    setStatus(singleStatus, still ? '' : 'Connecting to camera…');
    single.hidden = false;
    if (live && enlarged.stream) playStream();
    else playSnapshots();
  }

  function shrink() {
    playback += 1;
    enlarged = null;
    clear(galleryTimers);
    stopStream();
    single.style.backgroundImage = '';
    setMode('', false);
    single.hidden = true;
    startPolling();
  }

  // ---- Options (hold OK) and settings (the gear) ----

  function renderMenu() {
    menuSheet.hidden = !menu;
    if (!menu) return;
    menuTitle.textContent = menu.camera.name;
    menuList.textContent = '';
    menu.items.forEach(function (item, index) {
      var row = document.createElement('li');
      row.className = index === menu.index ? 'selected' : '';
      row.textContent = item.label;
      row.addEventListener('click', function () {
        menu.index = index;
        chooseMenu();
      });
      menuList.appendChild(row);
    });
  }

  function openMenu() {
    var tile = tiles[selected];
    if (!tile) return;
    var camera = tile.camera;
    var items = [];
    if (tiles.length > 1) items.push({ label: 'Move', run: startMoving });
    items.push({
      label: 'Hide',
      run: function () {
        camera.hidden = true;
        renderGrid();
        startPolling();
        saveArrangement();
      },
    });
    items.push({ label: 'Camera settings', run: openSettings });
    menu = { camera: camera, items: items, index: 0 };
    renderMenu();
    highlight();
  }

  function closeMenu() {
    menu = null;
    renderMenu();
  }

  function chooseMenu() {
    var item = menu.items[menu.index];
    closeMenu();
    item.run();
    highlight();
  }

  function startMoving() {
    moving = true;
  }

  // Moves the selected camera one place with the arrows; hidden cameras keep to the end.
  function moveSelected(keyCode) {
    var target = logic.move(selected, keyCode, tiles.length, logic.columns(tiles.length));
    if (target === selected) return;
    var shown = logic.reorder(
      tiles.map(function (tile) {
        return tile.camera;
      }),
      selected,
      target,
    );
    cameras = shown.concat(
      cameras.filter(function (camera) {
        return camera.hidden;
      }),
    );
    selected = target;
    renderGrid();
  }

  function settingsRows() {
    var rows = [
      {
        label: 'Live video',
        value: live ? 'On' : 'Off',
        on: live,
        toggle: function () {
          live = !live;
        },
      },
    ];
    cameras.forEach(function (camera) {
      rows.push({
        label: camera.name,
        value: camera.hidden ? 'Hidden' : 'Shown',
        on: !camera.hidden,
        camera: true,
        toggle: function () {
          camera.hidden = !camera.hidden;
        },
      });
    });
    return rows;
  }

  function renderSettings() {
    settingsSheet.hidden = settingsIndex < 0;
    if (settingsIndex < 0) return;
    settingsList.textContent = '';
    var selectedRow = null;
    settingsRows().forEach(function (row, index) {
      if (index === 1) {
        var section = document.createElement('li');
        section.className = 'section';
        section.textContent = 'Shown in the grid';
        settingsList.appendChild(section);
      }
      var item = document.createElement('li');
      var label = document.createElement('span');
      var value = document.createElement('span');
      item.className = (index === settingsIndex ? 'selected' : '') + (row.on ? '' : ' dim');
      label.className = 'label';
      label.textContent = row.label;
      value.className = row.on ? 'value on' : 'value';
      value.textContent = row.value;
      item.appendChild(label);
      item.appendChild(value);
      item.addEventListener('click', function () {
        settingsIndex = index;
        toggleSetting();
      });
      settingsList.appendChild(item);
      if (index === settingsIndex) selectedRow = item;
    });
    if (selectedRow && selectedRow.scrollIntoView) selectedRow.scrollIntoView({ block: 'nearest' });
  }

  function openSettings() {
    settingsIndex = 0;
    renderSettings();
    highlight();
  }

  function closeSettings() {
    settingsIndex = -1;
    renderSettings();
    if (tiles.length) gearFocused = false;
  }

  function toggleSetting() {
    var row = settingsRows()[settingsIndex];
    if (!row) return;
    row.toggle();
    if (row.camera) {
      renderGrid();
      startPolling();
    }
    renderSettings();
    highlight();
    saveArrangement();
  }

  gear.addEventListener('click', function () {
    if (menu || moving) return;
    if (settingsIndex >= 0) closeSettings();
    else openSettings();
    highlight();
  });

  function closeGallery() {
    clear(galleryTimers);
    clearTimeout(holdTimer);
    holdTimer = null;
    playback += 1;
    enlarged = null;
    stopStream();
    single.style.backgroundImage = '';
    setMode('', false);
    single.hidden = true;
    gallery.hidden = true;
    menu = null;
    renderMenu();
    settingsIndex = -1;
    renderSettings();
    moving = false;
    gearFocused = false;
    tiles.forEach(function (tile) {
      tile.img.removeAttribute('src');
    });
    tiles = [];
    cameras = [];
    grid.textContent = '';
  }

  single.addEventListener('click', shrink);

  // Remote keys while the gallery is open. Returns whether the key was used.
  function galleryKey(key, repeat) {
    var back = key === 461 || key === 27;
    var arrow = key >= 37 && key <= 40;
    if (!back && !arrow && key !== 13) return false;
    if (repeat && key === 13) return true;
    if (enlarged) {
      if (back || key === 13) shrink();
      return true;
    }
    if (menu) {
      if (back) closeMenu();
      else if (key === 13) chooseMenu();
      else if (key === 38) menu.index = Math.max(0, menu.index - 1);
      else if (key === 40) menu.index = Math.min(menu.items.length - 1, menu.index + 1);
      renderMenu();
    } else if (settingsIndex >= 0) {
      if (back) closeSettings();
      else if (key === 13) toggleSetting();
      else if (key === 38) settingsIndex = Math.max(0, settingsIndex - 1);
      else if (key === 40) settingsIndex = Math.min(settingsRows().length - 1, settingsIndex + 1);
      renderSettings();
    } else if (moving) {
      if (back || key === 13) {
        moving = false;
        saveArrangement();
      } else {
        moveSelected(key);
      }
    } else if (back) {
      dismiss();
      return true;
    } else if (gearFocused) {
      if (key === 13) openSettings();
      else if ((key === 40 || key === 37) && tiles.length) gearFocused = false;
    } else if (key === 13) {
      // Enlarge on release; holding opens the options instead.
      clearTimeout(holdTimer);
      holdTimer = setTimeout(function () {
        holdTimer = null;
        openMenu();
      }, HOLD_MS);
      return true;
    } else if (key === 38 && selected < logic.columns(tiles.length)) {
      gearFocused = true;
    } else {
      selected = logic.move(selected, key, tiles.length, logic.columns(tiles.length));
    }
    highlight();
    return true;
  }

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
    if (!gallery.hidden) {
      if (galleryKey(key, event.repeat)) event.preventDefault();
      return;
    }
    // 461 is the webOS remote's Back key; Escape helps when testing in a desktop browser.
    if (key === 461 || key === 27) {
      event.preventDefault();
      dismiss();
    }
  });
  document.addEventListener('keyup', function (event) {
    // A short OK press enlarges the selected camera.
    if (event.keyCode !== 13 || !holdTimer) return;
    clearTimeout(holdTimer);
    holdTimer = null;
    if (!gallery.hidden && !enlarged && !menu && settingsIndex < 0 && !moving && !gearFocused) {
      enlarge();
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
