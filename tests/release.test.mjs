import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import { bumpVersion, setVersion, syncVersions } from '../tools/sync-version.mjs';
import { mirrorFeed } from '../tools/mirror-feed.mjs';
import {
  classify,
  clip,
  composeNotes,
  fittedNotes,
  mapLimit,
  pullNumbers,
  retryDelay,
  stripComments,
  summaryOf,
  whatsNew,
} from '../tools/release-notes.mjs';

const root = path.resolve(import.meta.dirname, '..');
const metadata = ['appinfo.json', 'Cargo.toml', 'Cargo.lock', 'readme.md'];

async function fixture(t, version = '2.0.4') {
  const scratch = path.join(root, '.build');
  await fs.mkdir(scratch, { recursive: true });
  const directory = await fs.mkdtemp(path.join(scratch, 'release-test-'));
  t.after(() => fs.rm(directory, { recursive: true, force: true }));
  const project = JSON.parse(await fs.readFile(path.join(root, 'package.json'), 'utf8'));
  project.version = '1.2.3';
  await fs.writeFile(path.join(directory, 'package.json'), JSON.stringify(project));
  await Promise.all(
    metadata.map((filename) =>
      fs.copyFile(path.join(root, filename), path.join(directory, filename)),
    ),
  );
  await syncVersions(directory);
  project.version = version;
  await fs.writeFile(path.join(directory, 'package.json'), JSON.stringify(project));
  return directory;
}

const readMetadata = (directory) =>
  Promise.all(metadata.map((filename) => fs.readFile(path.join(directory, filename), 'utf8')));

test('version sync changes only release versions and is idempotent', async (t) => {
  const directory = await fixture(t, '3.1.0');
  const before = await readMetadata(directory);
  await assert.rejects(syncVersions(directory, { check: true }), /out of sync/);
  assert.deepEqual(await readMetadata(directory), before, 'check mode must not write files');

  assert.deepEqual((await syncVersions(directory)).changed, metadata);
  const after = await readMetadata(directory);
  const previous = JSON.parse(before[0]).version;
  assert.equal(JSON.parse(after[0]).version, '3.1.0');
  assert.equal(after[0], before[0].replace(`"version": "${previous}"`, '"version": "3.1.0"'));
  assert.equal(after[1], before[1].replace(`version = "${previous}"`, 'version = "3.1.0"'));
  assert.equal(
    after[2],
    before[2].replace(`name = "luma"\nversion = "${previous}"`, 'name = "luma"\nversion = "3.1.0"'),
    'dependency versions and helper crates must be preserved',
  );
  assert.equal(
    after[3],
    before[3].replaceAll(`com.ecklf.luma_${previous}_arm.ipk`, 'com.ecklf.luma_3.1.0_arm.ipk'),
  );
  assert.deepEqual((await syncVersions(directory, { check: true })).changed, []);
  assert.deepEqual((await syncVersions(directory)).changed, []);
});

test('invalid release versions and missing packages fail before writing metadata', async (t) => {
  for (const version of ['3.0.0-beta.1', '3.0', '03.0.0']) {
    const directory = await fixture(t, version);
    const before = await readMetadata(directory);
    await assert.rejects(syncVersions(directory), /numeric major.minor.patch/);
    assert.deepEqual(await readMetadata(directory), before);
  }
  const directory = await fixture(t);
  const lockfile = path.join(directory, 'Cargo.lock');
  await fs.writeFile(
    lockfile,
    (await fs.readFile(lockfile, 'utf8')).replace('name = "luma"', 'name = "other"'),
  );
  const before = await readMetadata(directory);
  await assert.rejects(syncVersions(directory), /one luma package in Cargo.lock/);
  assert.deepEqual(await readMetadata(directory), before);
});

test('release bumps default to the next minor version and synchronize every file', async (t) => {
  const directory = await fixture(t, '1.2.3');
  await syncVersions(directory);
  assert.deepEqual(await bumpVersion(directory), { previous: '1.2.3', version: '1.3.0' });
  assert.equal((await syncVersions(directory, { check: true })).version, '1.3.0');
  assert.deepEqual(await bumpVersion(directory, 'major'), { previous: '1.3.0', version: '2.0.0' });
  assert.equal((await syncVersions(directory, { check: true })).version, '2.0.0');
  const before = await fs.readFile(path.join(directory, 'package.json'), 'utf8');
  await assert.rejects(bumpVersion(directory, 'patch'), /Unknown version bump/);
  assert.equal(await fs.readFile(path.join(directory, 'package.json'), 'utf8'), before);

  const lockfile = path.join(directory, 'Cargo.lock');
  await fs.writeFile(
    lockfile,
    (await fs.readFile(lockfile, 'utf8')).replace('name = "luma"', 'name = "other"'),
  );
  const metadataBefore = await readMetadata(directory);
  await assert.rejects(bumpVersion(directory), /one luma package in Cargo.lock/);
  assert.equal(
    await fs.readFile(path.join(directory, 'package.json'), 'utf8'),
    before,
    'a failed bump must not leave package.json bumped',
  );
  assert.deepEqual(await readMetadata(directory), metadataBefore);
});

test("R2 mirror rewrites only this release's asset URLs", () => {
  const gh = 'https://github.com/EliSchleifer/lg-luma/releases/download/v2.16.2';
  const feed = {
    packages: [
      {
        id: 'com.ecklf.luma',
        fullDescriptionUrl: `${gh}/description.html`,
        iconUri: `${gh}/app-icon-large.png`,
        manifest: {
          id: 'com.ecklf.luma',
          version: '2.16.2',
          iconUri: `${gh}/app-icon-large.png`,
          ipkUrl: `${gh}/com.ecklf.luma_2.16.2_arm.ipk`,
          ipkHash: { sha256: 'a'.repeat(64) },
          ipkSize: 10,
        },
      },
    ],
  };
  const cf = 'https://luma.elischleifer.com/releases/v2.16.2';
  const mirrored = mirrorFeed(feed, 'v2.16.2', 'https://luma.elischleifer.com/');
  assert.equal(mirrored.packages[0].fullDescriptionUrl, `${cf}/description.html`);
  assert.equal(mirrored.packages[0].iconUri, `${cf}/app-icon-large.png`);
  assert.equal(mirrored.packages[0].manifest.iconUri, `${cf}/app-icon-large.png`);
  assert.equal(mirrored.packages[0].manifest.ipkUrl, `${cf}/com.ecklf.luma_2.16.2_arm.ipk`);
  assert.deepEqual(mirrored.packages[0].manifest.ipkHash, feed.packages[0].manifest.ipkHash);
  assert.equal(feed.packages[0].manifest.ipkUrl, `${gh}/com.ecklf.luma_2.16.2_arm.ipk`);
  assert.throws(() => mirrorFeed(feed, 'v2.16.3', 'https://luma.elischleifer.com'));
  assert.throws(() => mirrorFeed(feed, 'v2.16.2', 'http://luma.elischleifer.com'));
  const foreign = structuredClone(feed);
  foreign.packages[0].manifest.ipkUrl = 'https://example.com/x_arm.ipk';
  assert.throws(() => mirrorFeed(foreign, 'v2.16.2', 'https://luma.elischleifer.com'));
});

test('nightly builds set an exact version and synchronize every file', async (t) => {
  const directory = await fixture(t, '2.20.0');
  await syncVersions(directory);
  assert.deepEqual(await setVersion(directory, '2.20.7'), {
    previous: '2.20.0',
    version: '2.20.7',
  });
  assert.equal((await syncVersions(directory, { check: true })).version, '2.20.7');
  const before = await readMetadata(directory);
  for (const version of ['2.20', '2.20.7-nightly', '02.20.7', undefined]) {
    await assert.rejects(setVersion(directory, version), /numeric major.minor.patch/);
  }
  assert.deepEqual(await readMetadata(directory), before, 'a rejected version must not write');
});

test('release notes find each linked pull request once, in order', () => {
  const generated = [
    "## What's Changed",
    '* Add a Cameras app by @a in https://github.com/o/lg-luma/pull/14',
    '* Six-column grid by @a in https://github.com/o/lg-luma/pull/16',
    '* Follow-up by @a in https://github.com/o/lg-luma/pull/14',
    '* Elsewhere https://github.com/o/lg-lumaX/pull/99 and https://github.com/x/lg-luma/pull/98',
    '**Full Changelog**: https://github.com/o/lg-luma/compare/v2.20.0...v2.21.0',
  ].join('\n');
  assert.deepEqual(pullNumbers(generated, 'o/lg-luma'), [14, 16]);
});

test('release notes put the summary above the generated list', () => {
  const generated = "## What's Changed\n* Thing by @a in https://github.com/o/r/pull/1\n";
  const release = composeNotes({ summary: '- Better.\n', generated, previous: 'v2.20.0' });
  assert.equal(release, `## What's new\n\n- Better.\n\n${generated}`);
  assert.equal(composeNotes({ summary: '', generated }), generated, 'no summary, list only');
  const nightly = composeNotes({
    summary: '- Better.',
    generated,
    nightly: true,
    target: 'abc123',
    previous: 'v2.20.0',
  });
  assert.match(nightly, /^> \[!WARNING\]\n> Nightly build of `main` at abc123/);
  assert.match(nightly, /everything merged since v2\.20\.0/);
  assert.ok(nightly.indexOf("## What's new") < nightly.indexOf("## What's Changed"));
});

test('release notes group pull requests by the files they change', () => {
  assert.equal(classify(['src/signal-watch.rs', 'src/main.rs']), 'app');
  assert.equal(classify(['tv/home.sh']), 'app');
  assert.equal(classify(['development.md', 'appinfo.json']), 'app');
  assert.equal(classify(['custom_components/luma/light.py', 'hacs.json']), 'homeAssistant');
  assert.equal(classify(['src/app-artwork-tests.rs', 'src/tests.rs']), 'internal', 'tests only');
  assert.equal(classify(['tv-helper/src/tests.rs', 'tests/smoke.mjs']), 'internal');
  assert.equal(classify(['.github/workflows/ci.yml', 'tools/package.mjs']), 'internal');
  assert.equal(classify(['custom_components/luma/x.py', 'src/view.rs']), 'app', 'app wins');
  assert.equal(classify([]), 'internal');
});

test("release notes take each pull request's own summary", () => {
  // Shapes of real descriptions: a Summary paragraph, Summary bullets, and no headings.
  const paragraph =
    '## Summary\nAdd automatic return-to-Home when an HDMI input\nloses its signal.\n\n' +
    'More detail.\n\n## Key Changes\n- **New module**: x';
  assert.deepEqual(summaryOf(paragraph), [
    'Add automatic return-to-Home when an HDMI input loses its signal.',
  ]);
  const bullets =
    '## Summary\n\n- **Six-column grid:** Favorites wrap.\n  - nested detail\n' +
    '* **Most recent first:** Opened apps move.\n\n## Testing\n- `cargo test`';
  assert.deepEqual(summaryOf(bullets), [
    '- **Six-column grid:** Favorites wrap.',
    '- **Most recent first:** Opened apps move.',
  ]);
  assert.deepEqual(summaryOf('Fixes the clock.\r\n\r\nhttps://claude.ai/code/x'), [
    'Fixes the clock.',
  ]);
  assert.deepEqual(summaryOf('<!-- template -->\n## Testing\n- ran it'), []);
  assert.deepEqual(
    summaryOf('🤖 Generated with [Claude Code](https://claude.com/claude-code)'),
    [],
  );
  assert.deepEqual(summaryOf(null), []);
  const many = `## Summary\n${Array.from({ length: 8 }, (_, i) => `- item ${i}`).join('\n')}`;
  assert.deepEqual(summaryOf(many).slice(-2), ['- item 5', '- …and 2 more']);
});

test('release notes keep wrapped bullet lines and drop nested bullets', () => {
  const body =
    '## Summary\n- **Nightly builds**: runs daily and\n  builds only when main changed.\n' +
    '  - nested detail\n    nested continuation\n- Second\n\n- After a blank line\n' +
    'Trailing text ends the list\n- not this one';
  assert.deepEqual(summaryOf(body), [
    '- **Nightly builds**: runs daily and builds only when main changed.',
    '- Second',
    '- After a blank line',
  ]);
});

test('release notes clip long summaries and fall back to titles to fit GitHub', () => {
  const long = 'word '.repeat(200).trim();
  const clipped = clip(long);
  assert.ok(clipped.length <= 501 && clipped.endsWith('word…'), clipped.slice(-10));
  assert.equal(clip('short'), 'short');
  assert.equal(summaryOf(`## Summary\n${long}`)[0], clipped);

  const pulls = Array.from({ length: 3 }, (_, i) => ({
    title: `Change ${i}`,
    body: `## Summary\n${long}`,
    group: 'app',
  }));
  const context = { pulls, generated: 'LIST', previous: 'v1.0.0' };
  assert.match(fittedNotes(context), /\*\*Change 0\.\*\* word/);
  const titlesOnly = fittedNotes(context, 200);
  assert.match(titlesOnly, /^## What's new\n\n### On the TV\n\n- Change 0\n- Change 1\n/);
  assert.doesNotMatch(titlesOnly, /word/);
  assert.throws(() => fittedNotes(context, 20), /over GitHub's limit/);
});

test('release notes limit concurrent GitHub requests and keep their order', async () => {
  let active = 0;
  let peak = 0;
  const results = await mapLimit([5, 1, 4, 2, 3, 0, 6], 3, async (n) => {
    peak = Math.max(peak, ++active);
    await new Promise((resolve) => setTimeout(resolve, n));
    active--;
    return n * 10;
  });
  assert.deepEqual(results, [50, 10, 40, 20, 30, 0, 60]);
  assert.equal(peak, 3);
  assert.deepEqual(await mapLimit([], 3, async () => 1), []);
});

test('release notes retry only rate-limited GitHub responses', () => {
  const headers = (values) => new Headers(values);
  assert.equal(retryDelay(429, headers({ 'retry-after': '7' })), 7);
  assert.equal(retryDelay(403, headers({ 'retry-after': '500' })), 120);
  assert.equal(retryDelay(429, headers({})), 60);
  const reset = String(Math.floor(Date.now() / 1000) + 30);
  const wait = retryDelay(
    403,
    headers({ 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': reset }),
  );
  assert.ok(wait > 25 && wait <= 30, String(wait));
  assert.equal(retryDelay(403, headers({ 'x-ratelimit-remaining': '12' })), null, 'forbidden');
  assert.equal(retryDelay(404, headers({})), null);
  assert.equal(retryDelay(422, headers({ 'retry-after': '5' })), null);
});

test('release notes drop HTML comments without leaving an opener behind', () => {
  assert.equal(stripComments('a<!-- hint -->b<!--x-->c'), 'abc');
  assert.equal(stripComments('<!<!---->--'), '', 'an opener formed by a removal is dropped too');
  assert.equal(stripComments('x <!<!-- a -->-- b --> y'), 'x  y');
  assert.equal(stripComments('keep <!-- unclosed hint'), 'keep ');
  assert.equal(stripComments('no comments --> here'), 'no comments --> here');
});

test("release notes list what's new by group, app first", () => {
  const notes = whatsNew([
    { title: 'Run CI on Trunk runners', body: '## Summary\n- runners', group: 'internal' },
    { title: 'Auto-return to Home', body: '## Summary\nReturns Home.', group: 'app' },
    { title: 'Six-column grid', body: '## Summary\n- Wraps.\n- Recent first.', group: 'app' },
    { title: 'Example popup buttons', body: '', group: 'homeAssistant' },
  ]);
  assert.equal(
    notes,
    [
      '### On the TV',
      '',
      '- **Auto-return to Home.** Returns Home.',
      '- **Six-column grid**',
      '  - Wraps.',
      '  - Recent first.',
      '',
      '### Home Assistant',
      '',
      '- Example popup buttons',
      '',
      '### Behind the scenes',
      '',
      '- Run CI on Trunk runners',
    ].join('\n'),
  );
  assert.equal(whatsNew([]), '');
});
