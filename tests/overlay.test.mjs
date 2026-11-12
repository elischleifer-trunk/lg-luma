import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../overlay/logic.js', import.meta.url), 'utf8');
const context = { window: {} };
vm.runInNewContext(source, context);
const { settings, video, frameUrl, closedUrl } = context.window.LumaOverlay;
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
      title: 'Front door',
    },
  );
  assert.equal(video({ mode: 'video', url: 'javascript:alert(1)' }), null);
  assert.equal(video({ url: 'http://ha/x.m3u8' }), null);
  assert.equal(video(null), null);
});
