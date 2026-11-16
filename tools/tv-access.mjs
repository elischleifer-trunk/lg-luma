#!/usr/bin/env node
// Short-lived SSH access to the development TV for cloud sessions, approved on the maintainer's
// phone through the Luma Home Assistant integration's developer access (off by default; see
// custom_components/luma/dev_access.py and home-assistant.md#developer-access).
//
//   node tools/tv-access.mjs request [--reason "why"] [--ttl 8h]   ask, wait for the tap, set up `ssh tv`
//   node tools/tv-access.mjs check                                  run a harmless luna-send on the TV
//   node tools/tv-access.mjs status                                 what this session holds
//   node tools/tv-access.mjs release                                give access back and remove local files
//
// Needs LUMA_DEV_TOKEN; LUMA_DEV_URL defaults to https://ha.elischleifer.com. If Cloudflare Access
// guards Home Assistant too, TUNNEL_SERVICE_TOKEN_ID/SECRET are sent with each call. Secrets are
// written to files with mode 600 and never printed.
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const HOME = os.homedir();
const SSH_DIR = path.join(HOME, '.ssh');
const STATE_DIR = path.join(HOME, '.config', 'luma-dev');
export const FILES = {
  key: path.join(SSH_DIR, 'luma_tv'),
  proxy: path.join(SSH_DIR, 'luma-tv-proxy'),
  sshConfig: path.join(SSH_DIR, 'config'),
  state: path.join(STATE_DIR, 'session.json'),
  dotenv: path.join(ROOT, '.env'),
};
const BEGIN = '# >>> luma-dev (tools/tv-access.mjs) >>>';
const END = '# <<< luma-dev <<<';
const STREAMING_VARIABLES = {
  motn: 'MOTN_API_KEY',
  tmdb: 'TMDB_API_TOKEN',
  mdblist: 'MDBLIST_API_KEY',
};
const ANSWER_WITHIN_MS = 11 * 60 * 1000;

/** `8h`, `90m`, `3600` (seconds) → seconds. */
export function parseTtl(text) {
  const match = /^(\d+)\s*([hms]?)$/i.exec(String(text).trim());
  if (!match) throw new Error(`Cannot read --ttl ${text}; use e.g. 8h or 90m.`);
  const unit = { h: 3600, m: 60, s: 1, '': 1 }[match[2].toLowerCase()];
  return Number(match[1]) * unit;
}

/** A POSIX shell single-quoted literal. */
export function shellQuote(value) {
  return `'${String(value).replaceAll("'", `'\\''`)}'`;
}

/** `text` with the managed block replaced by `block` (or removed when `block` is null). The block
 * goes first so its settings win: ssh uses the first value it finds for each option. */
export function upsertBlock(text, block) {
  const pattern = new RegExp(`${escape(BEGIN)}[\\s\\S]*?${escape(END)}\\n?`, 'g');
  const rest = text.replace(pattern, '');
  if (block === null) return rest;
  return `${BEGIN}\n${block.trimEnd()}\n${END}\n${rest}`;
}

function escape(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** `Host tv` for the payload's ssh block: through cloudflared when it names a tunnel, otherwise
 * straight to the TV's address and port. */
export function sshConfigBlock(ssh, keyPath = FILES.key, proxyPath = FILES.proxy) {
  const route = ssh.tunnel ? `  ProxyCommand ${proxyPath} %h` : `  Port ${ssh.port ?? 22}`;
  return [
    'Host tv',
    `  HostName ${ssh.hostname}`,
    `  User ${ssh.user ?? 'root'}`,
    `  IdentityFile ${keyPath}`,
    '  IdentitiesOnly yes',
    route,
    '  StrictHostKeyChecking accept-new',
    '  ServerAliveInterval 30',
  ].join('\n');
}

export function proxyScript(cloudflared, cloudflare) {
  const lines = ['#!/bin/sh', '# Written by tools/tv-access.mjs; removed by `release`.'];
  if (cloudflare) {
    lines.push(
      `TUNNEL_SERVICE_TOKEN_ID=${shellQuote(cloudflare.client_id)}`,
      `TUNNEL_SERVICE_TOKEN_SECRET=${shellQuote(cloudflare.client_secret)}`,
      'export TUNNEL_SERVICE_TOKEN_ID TUNNEL_SERVICE_TOKEN_SECRET',
    );
  }
  lines.push(`exec ${shellQuote(cloudflared)} access ssh --hostname "$1"`);
  return `${lines.join('\n')}\n`;
}

/** .env lines for streaming keys the environment and the rest of .env do not already set. */
export function dotenvBlock(streaming, currentDotenv, env = process.env) {
  const outside = upsertBlock(currentDotenv, null);
  const lines = [];
  for (const [name, variable] of Object.entries(STREAMING_VARIABLES)) {
    const value = streaming?.[name];
    if (!value || env[variable]) continue;
    if (new RegExp(`^\\s*${variable}\\s*=\\s*\\S`, 'm').test(outside)) continue;
    lines.push(`${variable}=${value}`);
  }
  return lines;
}

function read(file) {
  try {
    return fs.readFileSync(file, 'utf8');
  } catch (error) {
    if (error.code === 'ENOENT') return '';
    throw error;
  }
}

function writePrivate(file, content, mode = 0o600) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  const tmp = `${file}.tmp-${process.pid}`;
  fs.writeFileSync(tmp, content, { mode });
  fs.chmodSync(tmp, mode);
  fs.renameSync(tmp, file);
}

function remove(file) {
  fs.rmSync(file, { force: true });
}

function editBlock(file, block) {
  const before = read(file);
  const after = upsertBlock(before, block);
  if (after === before) return;
  if (after.trim() === '' && block === null) remove(file);
  else writePrivate(file, after);
}

function loadState() {
  const text = read(FILES.state);
  return text ? JSON.parse(text) : null;
}

function which(command) {
  const result = spawnSync('sh', ['-c', `command -v ${command}`], { encoding: 'utf8' });
  return result.status === 0 ? result.stdout.trim() : null;
}

async function ensureCloudflared() {
  const found = which('cloudflared');
  if (found) return found;
  const arch = { x64: 'amd64', arm64: 'arm64' }[process.arch];
  if (process.platform !== 'linux' || !arch) {
    throw new Error('Install cloudflared (https://github.com/cloudflare/cloudflared/releases).');
  }
  const target = path.join(HOME, '.local', 'bin', 'cloudflared');
  if (fs.existsSync(target)) return target;
  console.log('Downloading cloudflared…');
  const url = `https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-${arch}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`Downloading cloudflared failed: HTTP ${response.status}`);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, Buffer.from(await response.arrayBuffer()), { mode: 0o755 });
  return target;
}

class Broker {
  constructor(env = process.env) {
    this.base = (env.LUMA_DEV_URL || 'https://ha.elischleifer.com').replace(/\/+$/, '');
    this.token = env.LUMA_DEV_TOKEN;
    if (!this.token) {
      throw new Error(
        'Set LUMA_DEV_TOKEN (the dev token from Luma → Configure → Developer access in Home Assistant).',
      );
    }
    this.headers = { 'X-Luma-Dev-Token': this.token, 'Content-Type': 'application/json' };
    if (env.TUNNEL_SERVICE_TOKEN_ID && env.TUNNEL_SERVICE_TOKEN_SECRET) {
      this.headers['CF-Access-Client-Id'] = env.TUNNEL_SERVICE_TOKEN_ID;
      this.headers['CF-Access-Client-Secret'] = env.TUNNEL_SERVICE_TOKEN_SECRET;
    }
  }

  async call(method, endpoint, body) {
    const response = await fetch(`${this.base}/api/luma/dev/${endpoint}`, {
      method,
      headers: this.headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: 'manual',
    });
    const text = await response.text();
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {
      // A login page (Cloudflare Access) or proxy error: reported below by status.
    }
    if (response.status >= 300 && response.status < 400) {
      throw new Error(
        `${this.base} redirected (Cloudflare Access login?). Set TUNNEL_SERVICE_TOKEN_ID/SECRET.`,
      );
    }
    return { status: response.status, json };
  }
}

const FAILURES = {
  401: 'Home Assistant refused LUMA_DEV_TOKEN.',
  403: 'The request was denied on the phone.',
  404: 'Developer access is off in Home Assistant (Luma → Configure → Developer access), or it restarted and forgot this request.',
  429: 'Too many requests, or locked after wrong tokens. Try again later.',
};

function failure(status, json) {
  const code = json?.error ? ` (${json.error})` : '';
  return new Error(`${FAILURES[status] ?? `Home Assistant answered HTTP ${status}.`}${code}`);
}

async function request({ reason, ttl, force }) {
  const state = loadState();
  if (state && Date.parse(state.expires_at) > Date.now() && !force) {
    console.log(`Already have TV access until ${state.expires_at}. Use --force for a new request.`);
    return;
  }
  const broker = new Broker();
  if (!which('ssh')) {
    throw new Error(
      'Install the OpenSSH client first: apt-get update && apt-get install -y openssh-client',
    );
  }
  const asked = await broker.call('POST', 'request', { reason, ttl });
  if (asked.status !== 202) throw failure(asked.status, asked.json);
  const id = asked.json.request_id;
  console.log(`Waiting for approval on the maintainer's phone (request ${id.slice(0, 6)})…`);
  const deadline = Date.now() + ANSWER_WITHIN_MS;
  let result;
  while (Date.now() < deadline) {
    result = await broker.call('GET', `result/${encodeURIComponent(id)}?wait=25`);
    if (result.status !== 202) break;
  }
  if (result?.status !== 200) {
    if (result?.status === 202 || result?.status === 410) {
      throw new Error('Nobody answered in time; the request expired.');
    }
    throw failure(result?.status, result?.json);
  }
  await install(result.json, id);
}

async function install(payload, id) {
  const { ssh, cloudflare, streaming, expires_at: expiresAt } = payload;
  fs.mkdirSync(SSH_DIR, { recursive: true, mode: 0o700 });
  writePrivate(
    FILES.key,
    ssh.private_key.endsWith('\n') ? ssh.private_key : `${ssh.private_key}\n`,
  );
  if (ssh.tunnel)
    writePrivate(FILES.proxy, proxyScript(await ensureCloudflared(), cloudflare), 0o700);
  else remove(FILES.proxy);
  editBlock(FILES.sshConfig, sshConfigBlock(ssh));
  const lines = dotenvBlock(streaming, read(FILES.dotenv));
  editBlock(FILES.dotenv, lines.length ? lines.join('\n') : null);
  writePrivate(
    FILES.state,
    `${JSON.stringify({ request_id: id, expires_at: expiresAt, hostname: ssh.hostname }, null, 2)}\n`,
  );
  const keysWritten = lines.map((line) => line.split('=')[0]);
  console.log(`Access to ${payload.tv ?? 'the TV'} approved until ${expiresAt}.`);
  const route = ssh.tunnel
    ? `via cloudflared${cloudflare ? ' (service token)' : ''}`
    : `port ${ssh.port ?? 22}`;
  console.log(`  ssh tv  → root@${ssh.hostname} ${route}`);
  if (keysWritten.length) console.log(`  .env: ${keysWritten.join(', ')}`);
  console.log('Check it with: node tools/tv-access.mjs check');
}

function check() {
  const result = spawnSync(
    'ssh',
    [
      '-tt',
      '-o',
      'ConnectTimeout=20',
      'tv',
      'luna-send -n 1 luna://com.webos.applicationManager/getForegroundAppInfo "{}"; exit',
    ],
    { encoding: 'utf8' },
  );
  const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
  if (result.status === 0 && /"returnValue"\s*:\s*true/.test(output)) {
    const app = /"appId"\s*:\s*"([^"]*)"/.exec(output)?.[1];
    console.log(`The TV answered${app ? `: foreground app ${app}` : ''}.`);
    return;
  }
  throw new Error(`The TV check failed (exit ${result.status}):\n${output.trim()}`);
}

function status() {
  const state = loadState();
  if (!state) {
    console.log('No TV access in this session.');
    return;
  }
  const left = Date.parse(state.expires_at) - Date.now();
  console.log(
    left > 0
      ? `TV access until ${state.expires_at} (${Math.round(left / 60000)} min left), ssh tv → ${state.hostname}.`
      : `TV access expired at ${state.expires_at}; run release to tidy up, then request again.`,
  );
}

async function release() {
  const state = loadState();
  if (state) {
    try {
      const done = await new Broker().call(
        'POST',
        `release/${encodeURIComponent(state.request_id)}`,
      );
      if (done.status !== 200 && done.status !== 404 && done.status !== 410) {
        console.warn(`${failure(done.status, done.json).message} The key expires on its own.`);
      }
    } catch (error) {
      console.warn(
        `Could not reach Home Assistant (${error.message}). The key expires on its own.`,
      );
    }
  }
  remove(FILES.key);
  remove(FILES.proxy);
  editBlock(FILES.sshConfig, null);
  editBlock(FILES.dotenv, null);
  remove(FILES.state);
  console.log('TV access released and local files removed.');
}

async function main() {
  // Node's fetch ignores HTTPS_PROXY unless NODE_USE_ENV_PROXY is set at startup.
  if ((process.env.HTTPS_PROXY || process.env.https_proxy) && !process.env.NODE_USE_ENV_PROXY) {
    const flags = ['--disable-warning=UNDICI-EHPA'];
    const child = spawnSync(process.execPath, [...flags, ...process.argv.slice(1)], {
      stdio: 'inherit',
      env: { ...process.env, NODE_USE_ENV_PROXY: '1' },
    });
    process.exit(child.status ?? 1);
  }
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      reason: { type: 'string', default: 'Luma development on the TV' },
      ttl: { type: 'string', default: '8h' },
      force: { type: 'boolean', default: false },
    },
  });
  const command = positionals[0];
  if (command === 'request') {
    await request({ reason: values.reason, ttl: parseTtl(values.ttl), force: values.force });
  } else if (command === 'check') check();
  else if (command === 'status') status();
  else if (command === 'release') await release();
  else {
    console.log(
      'Usage: node tools/tv-access.mjs request|check|status|release [--reason ..] [--ttl 8h]',
    );
    process.exit(command ? 1 : 0);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
