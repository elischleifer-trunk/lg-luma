import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../overlay/logic.js', import.meta.url), 'utf8');
const context = { window: {} };
vm.runInNewContext(source, context);
const { settings, gallery, columns, move, frameUrl, closedUrl, arrangeUrl, reorder } =
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

test('the Cameras gallery keeps only loadable cameras and needs at least one', () => {
  const shown = gallery({
    mode: 'cameras',
    session: 's',
    nonce: 'n',
    port: 5862,
    cameras: [
      { id: 'camera.front', name: 'Front', stream: 'http://ha/stream', snapshot: 'http://ha/s' },
      { id: 'camera.back', snapshot: 'https://ha/b', hidden: true },
      { id: 'camera.bad', stream: 'javascript:alert(1)' },
      { name: 'No ID', stream: 'http://ha/x' },
      'nonsense',
    ],
  });
  assert.deepEqual(plain(shown), {
    session: 's',
    nonce: 'n',
    port: 5862,
    live: true,
    cameras: [
      {
        id: 'camera.front',
        name: 'Front',
        stream: 'http://ha/stream',
        snapshot: 'http://ha/s',
        hidden: false,
      },
      {
        id: 'camera.back',
        name: 'camera.back',
        stream: null,
        snapshot: 'https://ha/b',
        hidden: true,
      },
    ],
  });
  assert.equal(
    gallery({ mode: 'cameras', live: false, cameras: [{ id: 'a', snapshot: 'http://ha/a' }] }).live,
    false,
  );
  assert.equal(gallery({ mode: 'cameras', cameras: [] }), null);
  assert.equal(gallery({ cameras: [{ id: 'a', stream: 'http://ha/a' }] }), null);
  assert.equal(gallery(null), null);
});

test('gallery columns grow with the camera count and arrows stay inside the grid', () => {
  assert.deepEqual([1, 2, 4, 5, 9, 10, 32].map(columns), [1, 2, 2, 3, 3, 4, 4]);
  // Seven cameras in three columns: rows 0–2, 3–5 and 6.
  assert.equal(move(0, 37, 7, 3), 0);
  assert.equal(move(0, 39, 7, 3), 1);
  assert.equal(move(2, 39, 7, 3), 2);
  assert.equal(move(1, 38, 7, 3), 1);
  assert.equal(move(1, 40, 7, 3), 4);
  // Down from above a shorter last row lands on its last camera; from the last row stays put.
  assert.equal(move(5, 40, 7, 3), 6);
  assert.equal(move(6, 40, 7, 3), 6);
  assert.equal(move(6, 38, 7, 3), 3);
});

test('the gallery saves its arrangement to Luma on the TV and moves cameras in place', () => {
  assert.equal(arrangeUrl(5862), 'http://127.0.0.1:5862/api/overlay/cameras');
  assert.deepEqual(plain(reorder(['a', 'b', 'c', 'd'], 0, 2)), ['b', 'c', 'a', 'd']);
  assert.deepEqual(plain(reorder(['a', 'b', 'c', 'd'], 3, 1)), ['a', 'd', 'b', 'c']);
  assert.deepEqual(plain(reorder(['a', 'b'], 0, 5)), ['a', 'b']);
});
