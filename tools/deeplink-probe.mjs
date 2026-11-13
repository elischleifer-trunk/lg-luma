// Tries direct-launch formats on a real TV so src/deeplink.rs only ships formats that work.
//
//   pnpm deeplink:probe --device tv --service netflix --link https://www.netflix.com/title/81223232/
//   pnpm deeplink:probe --device tv --service hulu --link <url> --params '{"contentTarget":"..."}'
//
// For each candidate it launches the app with those params through ares-launch (developer mode,
// no root needed), then asks whether the TV opened the title. Answer y (opened the title),
// h (opened the app's home screen only) or n (nothing / error). The summary at the end is what
// to record in src/deeplink.rs.
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline/promises';
import { parseArgs } from 'node:util';

const ROOT = path.resolve(import.meta.dirname, '..');

// Service → TV app IDs to try (first installed wins) and candidate param builders.
const SERVICES = {
  netflix: {
    apps: ['netflix'],
    candidates: (link) => {
      const id = link.match(/\/(?:title|watch)\/(\d+)/)?.[1];
      if (!id) return [];
      return [
        { contentTarget: `m=https://www.netflix.com/title/${id}&source_type=4` },
        { contentTarget: `https://www.netflix.com/watch/${id}` },
        { contentTarget: `https://www.netflix.com/title/${id}` },
        { contentId: `m=https://www.netflix.com/title/${id}&source_type=4` },
      ];
    },
  },
  prime: { apps: ['amazon'], candidates: webLink },
  disney: { apps: ['com.disney.disneyplus-prod'], candidates: webLink },
  hbo: { apps: ['com.wbd.stream', 'com.hbo.hbomax', 'hbo-go-2'], candidates: webLink },
  hulu: { apps: ['hulu'], candidates: webLink },
  apple: { apps: ['com.apple.appletv', 'com.apple.appletv.web'], candidates: webLink },
  peacock: { apps: ['com.peacocktv.peacock'], candidates: webLink },
  paramount: { apps: ['com.pplus.webapp.prod-intl', 'cbs'], candidates: webLink },
};

function webLink(link) {
  return [{ contentTarget: link }, { contentId: link }, { target: link }, { url: link }];
}

function ares(tool, args) {
  const cli = path.join(ROOT, 'node_modules/@webos-tools/cli/bin', `${tool}.js`);
  return spawnSync(process.execPath, [cli, ...args], {
    cwd: ROOT,
    encoding: 'utf8',
    timeout: 60000,
  });
}

async function main() {
  const { values } = parseArgs({
    options: {
      device: { type: 'string', default: 'tv' },
      service: { type: 'string' },
      link: { type: 'string' },
      app: { type: 'string' },
      params: { type: 'string', multiple: true },
    },
  });
  const service = SERVICES[values.service];
  if (!service || !values.link) {
    throw new Error(`Pass --service (${Object.keys(SERVICES).join(', ')}) and --link <url>`);
  }
  const candidates = values.params?.length
    ? values.params.map((p) => JSON.parse(p))
    : service.candidates(values.link);
  if (candidates.length === 0) throw new Error('No candidates for that link.');

  let app = values.app;
  if (!app) {
    const list = ares('ares-install', ['--device', values.device, '--list']);
    if (list.status !== 0) throw new Error(list.stderr || list.stdout);
    app = service.apps.find((id) => list.stdout.split(/\s+/).includes(id));
    if (!app) throw new Error(`None of ${service.apps.join(', ')} is installed. Pass --app.`);
  }

  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const results = [];
  for (const params of candidates) {
    // Close first so every candidate is a cold launch; relaunch handling is tested separately.
    ares('ares-launch', ['--device', values.device, '--close', app]);
    const json = JSON.stringify(params);
    console.log(`\n${app} ${json}`);
    const run = ares('ares-launch', ['--device', values.device, app, '--params', json]);
    if (run.status !== 0) console.log(`  launch failed: ${(run.stderr || run.stdout).trim()}`);
    const answer = (await rl.question('  Opened the title? [y/h/n] ')).trim().toLowerCase();
    results.push({ params, result: { y: 'title', h: 'app home' }[answer] ?? 'failed' });
    if (answer === 'y') break;
  }
  rl.close();
  console.log(`\n${values.service} (${app})`);
  for (const { params, result } of results)
    console.log(`  ${result.padEnd(8)} ${JSON.stringify(params)}`);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
