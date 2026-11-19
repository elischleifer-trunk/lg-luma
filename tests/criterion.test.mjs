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
const { ORIGIN, target, parse, action } = context.window.LumaCriterion;

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
