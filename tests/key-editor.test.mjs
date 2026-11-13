import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { change, checkKeys, describeKey, loadKeys, post } from '../assets/key-editor/keys.js';

const keys = {
  motn: { set: true, hint: '…cdef' },
  tmdb: { set: false, hint: '' },
};

test('key status is read without caching and rejected when malformed', async () => {
  assert.deepEqual(
    await loadKeys(async (url, options) => {
      assert.equal(url, 'keys.json');
      assert.deepEqual(options, { cache: 'no-store' });
      return { ok: true, json: async () => keys };
    }),
    keys,
  );
  await assert.rejects(
    loadKeys(async () => ({ ok: false })),
    /unavailable/,
  );
  assert.throws(() => checkKeys({ motn: { set: 'yes', hint: '' }, tmdb: keys.tmdb }), /Invalid/);
  assert.throws(() => checkKeys({ motn: keys.motn }), /Invalid/);
});

test('stored keys are described by their last characters only', () => {
  assert.equal(describeKey(keys.motn), 'Saved key ends …cdef.');
  assert.equal(describeKey(keys.tmdb), 'Not set yet.');
});

test('blank fields keep stored keys instead of erasing them', () => {
  assert.deepEqual(change({ motn: '  motn-key-abc  ', tmdb: '   ' }), { motn: 'motn-key-abc' });
  assert.deepEqual(change({ motn: '', tmdb: '' }), {});
  assert.deepEqual(change({}), {});
});

test('saving repeats the session token and reports the TV’s error', async () => {
  const result = await post(
    async (url, options) => {
      assert.equal(url, 'save');
      assert.equal(options.method, 'POST');
      assert.equal(options.headers['X-Luma-Token'], 'Token123');
      assert.equal(options.headers['Content-Type'], 'application/json');
      assert.deepEqual(JSON.parse(options.body), { tmdb: '' });
      return { ok: true, json: async () => ({ ...keys, warnings: [] }) };
    },
    'Token123',
    { tmdb: '' },
  );
  assert.deepEqual(result.warnings, []);
  await assert.rejects(
    post(
      async () => ({ ok: false, json: async () => ({ error: 'TMDB refused this key.' }) }),
      'Token123',
      {},
    ),
    /TMDB refused/,
  );
  await assert.rejects(
    post(async () => ({ ok: false, json: async () => Promise.reject(new Error()) }), 'T', {}),
    /could not save/,
  );
});

test('the page loads only its own scripts and links out only to key providers', async () => {
  const html = await readFile(new URL('../assets/key-editor/index.html', import.meta.url), 'utf8');
  const scripts = [...html.matchAll(/<script[^>]*src="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(scripts, ['app.js']);
  const links = [...html.matchAll(/href="(https?:[^"]+)"/g)].map((m) => new URL(m[1]).host);
  assert.deepEqual(links.sort(), ['www.movieofthenight.com', 'www.themoviedb.org']);
});
