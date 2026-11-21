import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(
  new URL('../criterion/webOSUserScripts/userScript.js', import.meta.url),
  'utf8',
);
// No document: only the shared helpers load, as on the start page before it navigates.
const context = { URL, window: {} };
vm.runInNewContext(source, context);
const { ORIGIN, target, parse, action, signInRequest } = context.window.LumaCriterion;

test('only Criterion Channel pages are opened', () => {
  const film = `${ORIGIN}/films/oZkRW7X1/laura?play=1`;
  assert.equal(target({ target: film }), film);
  for (const bad of [
    'https://criterionchannel.com.evil.example/films/x',
    'http://www.criterionchannel.com/films/x',
    'https://user:pass@www.criterionchannel.com/films/x',
    'javascript:alert(1)',
    `${ORIGIN}/${'x'.repeat(600)}`,
    42,
    undefined,
  ]) {
    assert.equal(target({ target: bad }), `${ORIGIN}/`, String(bad));
  }
  assert.equal(target(undefined), `${ORIGIN}/`);
});

test('launch parameters arrive as JSON text or an object', () => {
  assert.equal(parse('{"target":"x"}').target, 'x');
  assert.equal(parse({ target: 'y' }).target, 'y');
  assert.equal(Object.keys(parse('not json')).length, 0);
  assert.equal(Object.keys(parse('')).length, 0);
  assert.equal(Object.keys(parse('"text"')).length, 0);
});

test('OK and the arrows control a film only while one is on screen', () => {
  assert.equal(action(461, false), 'back');
  assert.equal(action(415, false), 'play');
  assert.equal(action(19, true), 'pause');
  assert.equal(action(13, false), null);
  assert.equal(action(13, true), 'toggle');
  assert.equal(action(39, false), null);
  assert.equal(action(39, true), 'forward');
  assert.equal(action(37, true), 'rewind');
  assert.equal(action(38, true), null);
});

test('the sign-in code goes only to Luma on the TV itself', () => {
  const code = '0123456789abcdef0123456789abcdef';
  const request = signInRequest({ code, port: 5862 });
  assert.equal(request.url, 'http://127.0.0.1:5862/api/criterion/sign-in');
  assert.equal(request.body, JSON.stringify({ code }));
  for (const params of [
    {},
    { code, port: 'x' },
    { code, port: 0 },
    { code, port: 70000 },
    { code: 'short', port: 5862 },
    { code: `${code}"`, port: 5862 },
    undefined,
  ]) {
    assert.equal(signInRequest(params), null, JSON.stringify(params));
  }
});

test('only a launch that plays a film is covered while it loads', () => {
  const { playsFilm, launchKey } = context.window.LumaCriterion;
  assert.equal(playsFilm({ target: `${ORIGIN}/films/oZkRW7X1/laura?play=1` }), true);
  assert.equal(playsFilm({ target: `${ORIGIN}/films/oZkRW7X1/laura` }), false);
  assert.equal(playsFilm({ target: `${ORIGIN}/search?q=laura` }), false);
  assert.equal(playsFilm({ target: 'https://evil.example/films/x?play=1' }), false);
  assert.equal(playsFilm({}), false);
  const code = '0123456789abcdef0123456789abcdef';
  assert.equal(launchKey({ code, target: `${ORIGIN}/films/x?play=1` }), code);
  assert.equal(launchKey({ target: `${ORIGIN}/films/x?play=1` }), `${ORIGIN}/films/x?play=1`);
});
