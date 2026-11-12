// Pure helpers for the overlay page (doorbell card and Cameras gallery). Plain script (no modules) for webOS 4's Chromium 53;
// tests load it in a Node VM. Luma validates every value; this only applies safe defaults.
(function (root) {
  'use strict';
  var POSITIONS = ['top-right', 'top-left', 'bottom-right', 'bottom-left'];
  var SIZES = ['medium', 'small', 'large'];

  function http(url) {
    return typeof url === 'string' && /^https?:\/\/[^/\s]/i.test(url) ? url : null;
  }

  function clamp(value, min, max, fallback) {
    var number = Number(value);
    if (value === null || value === undefined || !isFinite(number)) return fallback;
    return Math.min(max, Math.max(min, number));
  }

  function settings(params) {
    params = params && typeof params === 'object' ? params : {};
    var stream = http(params.stream);
    var snapshot = http(params.snapshot);
    if (!stream && !snapshot && !params.dismiss) return null;
    return {
      dismiss: params.dismiss === true,
      session: typeof params.session === 'string' ? params.session : '',
      nonce: typeof params.nonce === 'string' ? params.nonce : '',
      port: clamp(params.port, 1, 65535, 5862),
      stream: stream,
      snapshot: snapshot,
      refresh: clamp(params.refresh, 0.25, 30, 1),
      duration: clamp(params.duration, 5, 600, 30),
      title: typeof params.title === 'string' && params.title ? params.title : 'Camera',
      message: typeof params.message === 'string' ? params.message : '',
      position: POSITIONS.indexOf(params.position) >= 0 ? params.position : POSITIONS[0],
      size: SIZES.indexOf(params.size) >= 0 ? params.size : SIZES[0],
    };
  }

  // A fresh snapshot URL each refresh, so neither the browser nor a proxy serves a cached frame.
  function frameUrl(url, now) {
    return url + (url.indexOf('?') >= 0 ? '&' : '?') + '_luma=' + now;
  }

  function closedUrl(port) {
    return 'http://127.0.0.1:' + port + '/api/overlay/closed';
  }

  // Where the gallery saves the owner's order, hidden cameras and live-video choice.
  function arrangeUrl(port) {
    return 'http://127.0.0.1:' + port + '/api/overlay/cameras';
  }

  // A copy of the list with one item moved from one index to another.
  function reorder(list, from, to) {
    var next = list.slice();
    if (from < 0 || from >= next.length || to < 0 || to >= next.length) return next;
    var item = next.splice(from, 1)[0];
    next.splice(to, 0, item);
    return next;
  }

  function text(value, max) {
    return typeof value === 'string' ? value.slice(0, max) : '';
  }

  // The Cameras gallery: every camera Home Assistant shared, in the owner's order, hidden ones
  // included so settings can show them again. Cameras without a loadable URL are skipped;
  // null means there is nothing to show.
  function gallery(params) {
    params = params && typeof params === 'object' ? params : {};
    if (params.mode !== 'cameras' || !Array.isArray(params.cameras)) return null;
    var cameras = [];
    params.cameras.slice(0, 32).forEach(function (camera) {
      if (!camera || typeof camera !== 'object') return;
      var stream = http(camera.stream);
      var snapshot = http(camera.snapshot);
      var id = text(camera.id, 255);
      if (!id || (!stream && !snapshot)) return;
      cameras.push({
        id: id,
        name: text(camera.name, 80) || id,
        stream: stream,
        snapshot: snapshot,
        hidden: camera.hidden === true,
      });
    });
    if (!cameras.length) return null;
    return {
      session: typeof params.session === 'string' ? params.session : '',
      nonce: typeof params.nonce === 'string' ? params.nonce : '',
      port: clamp(params.port, 1, 65535, 5862),
      // Enlarged cameras open the live stream unless the owner chose snapshots.
      live: params.live !== false,
      cameras: cameras,
    };
  }

  // Grid columns for a camera count: one large view, then 2×2, 3×3 and 4 across.
  function columns(count) {
    if (count <= 1) return 1;
    if (count <= 4) return 2;
    if (count <= 9) return 3;
    return 4;
  }

  // Remote arrow keys (webOS and desktop key codes 37–40) move through the grid by rows.
  function move(index, keyCode, count, cols) {
    var next = index;
    if (keyCode === 37 && index % cols > 0) next = index - 1;
    if (keyCode === 39 && index % cols < cols - 1) next = index + 1;
    if (keyCode === 38) next = index - cols;
    if (keyCode === 40) next = index + cols;
    if (next < 0) return index;
    if (next >= count) {
      // Down from a row above a shorter last row lands on that row's last camera.
      return keyCode === 40 && Math.floor(index / cols) < Math.floor((count - 1) / cols)
        ? count - 1
        : index;
    }
    return next;
  }

  var api = {
    settings: settings,
    gallery: gallery,
    columns: columns,
    move: move,
    frameUrl: frameUrl,
    closedUrl: closedUrl,
    arrangeUrl: arrangeUrl,
    reorder: reorder,
  };
  if (typeof module === 'object' && module.exports) module.exports = api;
  root.LumaOverlay = api;
})(typeof window === 'object' ? window : globalThis);
