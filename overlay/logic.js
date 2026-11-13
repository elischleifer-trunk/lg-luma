// Pure helpers for the overlay page's doorbell card. Plain script (no modules) for webOS 4's Chromium 53;
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
      // Home Assistant's HLS playlist: full-frame-rate video in the card once it plays.
      hls: http(params.hls),
      refresh: clamp(params.refresh, 0.25, 30, 1),
      duration: clamp(params.duration, 5, 600, 30),
      title: typeof params.title === 'string' && params.title ? params.title : 'Camera',
      message: typeof params.message === 'string' ? params.message : '',
      position: POSITIONS.indexOf(params.position) >= 0 ? params.position : POSITIONS[0],
      size: SIZES.indexOf(params.size) >= 0 ? params.size : SIZES[0],
    };
  }

  // A camera video from the Cameras app: Home Assistant's HLS playlist, played full screen.
  function video(params) {
    params = params && typeof params === 'object' ? params : {};
    var url = http(params.url);
    if (params.mode !== 'video' || !url) return null;
    return {
      session: typeof params.session === 'string' ? params.session : '',
      nonce: typeof params.nonce === 'string' ? params.nonce : '',
      port: clamp(params.port, 1, 65535, 5862),
      url: url,
      // Shown over the video until it plays.
      snapshot: http(params.snapshot),
      title: typeof params.title === 'string' ? params.title.slice(0, 80) : '',
    };
  }

  // A fresh snapshot URL each refresh, so neither the browser nor a proxy serves a cached frame.
  function frameUrl(url, now) {
    return url + (url.indexOf('?') >= 0 ? '&' : '?') + '_luma=' + now;
  }

  function closedUrl(port) {
    return 'http://127.0.0.1:' + port + '/api/overlay/closed';
  }

  function expandUrl(port) {
    return 'http://127.0.0.1:' + port + '/api/overlay/expand';
  }

  // The Cameras video's scrubber: seconds of video the TV has kept, oldest to newest. Closer
  // than this to the newest counts as live.
  var LIVE_EDGE_S = 3;

  // Where Left or Right (`step` seconds, negative back) lands within what is kept, or null when
  // that is live.
  function seek(position, start, end, step) {
    var next = Math.min(end, Math.max(start, position + step));
    return end - next < LIVE_EDGE_S ? null : next;
  }

  // Seconds behind live; 0 when close enough to count as live.
  function behind(position, end) {
    var lag = end - position;
    return lag < LIVE_EDGE_S ? 0 : lag;
  }

  // `-m:ss` behind live.
  function clock(seconds) {
    var whole = Math.floor(Math.max(0, seconds));
    var rest = whole % 60;
    return '-' + Math.floor(whole / 60) + ':' + (rest < 10 ? '0' : '') + rest;
  }

  var api = {
    settings: settings,
    video: video,
    seek: seek,
    behind: behind,
    clock: clock,
    frameUrl: frameUrl,
    closedUrl: closedUrl,
    expandUrl: expandUrl,
  };
  if (typeof module === 'object' && module.exports) module.exports = api;
  root.LumaOverlay = api;
})(typeof window === 'object' ? window : globalThis);
