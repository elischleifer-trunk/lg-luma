import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';
import { parse } from 'acorn';

const source = await readFile(new URL('../overlay/logic.js', import.meta.url), 'utf8');
const context = { window: {} };
vm.runInNewContext(source, context);
const { settings, video, recorded, piece, frameUrl, closedUrl, expandUrl, seek, behind, clock } =
  context.window.LumaOverlay;
// Values built inside the VM have its own Object prototype; compare them as plain JSON.
const plain = (value) => JSON.parse(JSON.stringify(value));

test('launch parameters get safe defaults and invalid values are replaced', () => {
  assert.deepEqual(
    plain(settings({ stream: 'http://ha:8123/api/camera_proxy_stream/x?token=t' })),
    {
      dismiss: false,
      session: '',
      nonce: '',
      port: 5862,
      stream: 'http://ha:8123/api/camera_proxy_stream/x?token=t',
      snapshot: null,
      hls: null,
      refresh: 1,
      duration: 30,
      title: 'Camera',
      message: '',
      position: 'top-right',
      size: 'medium',
    },
  );
  const odd = settings({
    snapshot: 'https://nvr/snap.jpg',
    stream: 'javascript:alert(1)',
    duration: 9999,
    refresh: 0,
    position: 'middle',
    size: 'huge',
    port: 'x',
  });
  assert.equal(odd.stream, null);
  assert.equal(odd.duration, 600);
  assert.equal(odd.refresh, 0.25);
  assert.equal(odd.position, 'top-right');
  assert.equal(odd.size, 'medium');
  assert.equal(odd.port, 5862);
});

test('a card may carry an http(s) HLS playlist, but still needs a motion-JPEG or snapshot', () => {
  const playlist = 'http://ha:8123/api/hls/abc/master_playlist.m3u8';
  assert.equal(settings({ stream: 'http://ha/s.mjpeg', hls: playlist }).hls, playlist);
  assert.equal(settings({ stream: 'http://ha/s.mjpeg', hls: 'javascript:alert(1)' }).hls, null);
  assert.equal(settings({ hls: playlist }), null);
});

test('an overlay without a camera closes unless it is a dismissal', () => {
  assert.equal(settings({}), null);
  assert.equal(settings(null), null);
  assert.equal(settings({ stream: 'file:///etc/passwd' }), null);
  assert.equal(settings({ dismiss: true }).dismiss, true);
});

test('snapshot frames bypass caches and closing reports to Luma on the TV', () => {
  assert.equal(frameUrl('http://ha/s.jpg', 5), 'http://ha/s.jpg?_luma=5');
  assert.equal(frameUrl('http://ha/s.jpg?token=a', 5), 'http://ha/s.jpg?token=a&_luma=5');
  assert.equal(closedUrl(5862), 'http://127.0.0.1:5862/api/overlay/closed');
  assert.equal(expandUrl(5862), 'http://127.0.0.1:5862/api/overlay/expand');
});

test('the overlay app keeps the foreground app playing and stays out of app lists', async () => {
  const info = JSON.parse(
    await readFile(new URL('../overlay/appinfo.json', import.meta.url), 'utf8'),
  );
  assert.equal(info.id, 'com.ecklf.luma.overlay');
  assert.equal(info.type, 'web');
  assert.equal(info.transparent, true);
  assert.equal(info.defaultWindowType, 'overlay');
  assert.equal(info.visible, false);
  assert.equal(info.handlesRelaunch, true);
  assert.equal(info.disableBackHistoryAPI, true);
});

test('a camera video needs an http(s) playlist and keeps its session for reporting back', () => {
  assert.deepEqual(
    plain(
      video({
        mode: 'video',
        url: 'http://ha:8123/api/hls/x/master_playlist.m3u8',
        snapshot: 'http://ha:8123/api/camera_proxy/camera.door?token=t',
        title: 'Front door',
        session: 's',
        nonce: 'n',
        port: 5862,
      }),
    ),
    {
      session: 's',
      nonce: 'n',
      port: 5862,
      url: 'http://ha:8123/api/hls/x/master_playlist.m3u8',
      snapshot: 'http://ha:8123/api/camera_proxy/camera.door?token=t',
      title: 'Front door',
      history: false,
    },
  );
  // A cover snapshot that is not http(s) is left out.
  assert.equal(
    video({ mode: 'video', url: 'http://ha/x.m3u8', snapshot: 'javascript:alert(1)' }).snapshot,
    null,
  );
  assert.equal(video({ mode: 'video', url: 'javascript:alert(1)' }), null);
  assert.equal(video({ url: 'http://ha/x.m3u8' }), null);
  assert.equal(video(null), null);
});

test("the overlay's scripts parse in webOS 4's Chromium 53, which knows ES2016 at most", async () => {
  // Call-argument trailing commas (ES2017) and the like would stop every overlay from showing.
  for (const file of ['overlay/app.js', 'overlay/logic.js', 'overlay/vendor/hls.light.min.js']) {
    const source = await readFile(new URL(`../${file}`, import.meta.url), 'utf8');
    assert.doesNotThrow(() => parse(source, { ecmaVersion: 2016, sourceType: 'script' }), file);
  }
});

test('the camera video scrubs within what the TV kept and goes live at the newest', () => {
  // Kept from 10 s to 130 s; the playhead 2 s behind the newest counts as live.
  assert.equal(behind(128, 130), 0);
  assert.equal(seek(128, 10, 130, -5), 123);
  assert.equal(seek(12, 10, 130, -5), 10);
  assert.equal(seek(100, 10, 130, 5), 105);
  assert.equal(seek(124, 10, 130, 5), null);
  assert.equal(behind(88, 130), 42);
  assert.equal(clock(42.9), '-0:42');
  assert.equal(clock(125), '-2:05');
});

test('recorded video loads from Luma a piece at a time until it reaches live', () => {
  const past = recorded({
    mode: 'recorded',
    start: 1000.7,
    session: 's',
    nonce: 'n&x',
    port: 5862,
  });
  assert.equal(past.start, 1000);
  assert.equal(recorded({ mode: 'recorded', start: 'soon' }), null);
  assert.equal(recorded({ mode: 'video', start: 1000 }), null);
  assert.deepEqual(plain(piece(past, 1000, 5000, true)), {
    start: 1000,
    end: 1060,
    url: 'http://127.0.0.1:5862/api/overlay/recorded?session=s&nonce=n%26x&start=1000&end=1060',
  });
  assert.equal(piece(past, 1060, 5000, false).end, 1360);
  // Never past now; within ten seconds of it, it is live.
  assert.equal(piece(past, 4900, 5000.9, false).end, 5000);
  assert.equal(piece(past, 4995, 5000, false), null);
});
