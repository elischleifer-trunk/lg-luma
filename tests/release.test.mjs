import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import { bumpVersion, syncVersions } from '../tools/sync-version.mjs';
import { mirrorFeed } from '../tools/mirror-feed.mjs';

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
