import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  dotenvBlock,
  parseTtl,
  proxyScript,
  shellQuote,
  sshConfigBlock,
  upsertBlock,
} from '../tools/tv-access.mjs';

test('parseTtl reads hours, minutes and seconds', () => {
  assert.equal(parseTtl('8h'), 28800);
  assert.equal(parseTtl('90m'), 5400);
  assert.equal(parseTtl('600'), 600);
  assert.throws(() => parseTtl('soon'));
});

test('shellQuote survives quotes', () => {
  assert.equal(shellQuote("a'b"), `'a'\\''b'`);
});

test('upsertBlock puts the managed block first, replaces it and removes it', () => {
  const user = 'Host other\n  User me\n';
  const once = upsertBlock(user, 'Host tv\n  User root');
  assert.ok(once.startsWith('# >>> luma-dev'));
  assert.ok(once.endsWith(user));
  const twice = upsertBlock(once, 'Host tv\n  User admin');
  assert.equal(twice.match(/luma-dev \(/g).length, 1);
  assert.ok(twice.includes('User admin') && !twice.includes('User root'));
  assert.equal(upsertBlock(twice, null), user);
});

test('sshConfigBlock goes through the proxy script only for a tunnel', () => {
  const tunnel = sshConfigBlock({ hostname: 'tv.example.com', tunnel: true }, '/k', '/p');
  assert.match(tunnel, /HostName tv\.example\.com/);
  assert.match(tunnel, /ProxyCommand \/p %h/);
  assert.match(tunnel, /IdentitiesOnly yes/);
  const direct = sshConfigBlock(
    { hostname: '192.168.1.50', port: 2222, tunnel: false },
    '/k',
    '/p',
  );
  assert.match(direct, /Port 2222/);
  assert.doesNotMatch(direct, /ProxyCommand/);
});

test('proxyScript exports the service token only when given', () => {
  const withToken = proxyScript('/bin/cloudflared', { client_id: 'id', client_secret: "s'x" });
  assert.match(withToken, /TUNNEL_SERVICE_TOKEN_SECRET='s'\\''x'/);
  assert.match(withToken, /exec '\/bin\/cloudflared' access ssh --hostname "\$1"/);
  assert.doesNotMatch(proxyScript('/bin/cloudflared', null), /TUNNEL_SERVICE_TOKEN/);
});

test('dotenvBlock skips keys already set in the environment or elsewhere in .env', () => {
  const lines = dotenvBlock(
    { motn: 'm', tmdb: 't', mdblist: 'd' },
    'TMDB_API_TOKEN=mine\nMDBLIST_API_KEY=\n',
    { MOTN_API_KEY: 'from-env' },
  );
  assert.deepEqual(lines, ['MDBLIST_API_KEY=d']);
});
