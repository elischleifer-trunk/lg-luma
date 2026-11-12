import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(import.meta.dirname, '..');

function replaceVersion(source, pattern, version, filename) {
  if ([...source.matchAll(pattern)].length !== 1) {
    throw new Error(`Expected one release version in ${filename}.`);
  }
  return source.replace(pattern, (_match, prefix, suffix) => `${prefix}${version}${suffix}`);
}

function updateTomlVersion(source, header, version, filename) {
  let packages = 0;
  const updated = source
    .split(/(?=^\[)/m)
    .map((section) => {
      if (
        section.split(/\r?\n/, 1)[0].trim() !== header ||
        !/^name[ \t]*=[ \t]*"luma"[ \t]*\r?$/m.test(section)
      ) {
        return section;
      }
      packages++;
      return replaceVersion(
        section,
        /^([ \t]*version[ \t]*=[ \t]*")[^"\r\n]+(")/gm,
        version,
        filename,
      );
    })
    .join('');
  if (packages !== 1) throw new Error(`Expected one luma package in ${filename}.`);
  return updated;
}

const releaseVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

// Reads and validates every release file, then computes its updated text. Nothing is written, so
// a failed check leaves the checkout untouched. With `bump`, package.json itself moves to the next
// minor (x.Y.0) or major (X.0.0) version, or to an exact `{ set: 'X.Y.Z' }` version, and is
// updated along with the rest.
async function planVersions(directory, bump) {
  const read = (filename) => fs.readFile(path.join(directory, filename), 'utf8');
  const project = await read('package.json');
  const { version: previous } = JSON.parse(project);
  const match = typeof previous === 'string' ? releaseVersion.exec(previous) : null;
  if (!match) {
    throw new Error('package.json must have a numeric major.minor.patch release version.');
  }
  let version = previous;
  if (bump === 'major') version = `${Number(match[1]) + 1}.0.0`;
  else if (bump === 'minor') version = `${match[1]}.${Number(match[2]) + 1}.0`;
  else if (typeof bump === 'object' && bump !== null) {
    if (typeof bump.set !== 'string' || !releaseVersion.test(bump.set)) {
      throw new Error(`Version "${bump.set}" must be a numeric major.minor.patch version.`);
    }
    version = bump.set;
  } else if (bump !== undefined) {
    throw new Error(`Unknown version bump "${bump}". Use minor or major.`);
  }

  const filenames = ['appinfo.json', 'Cargo.toml', 'Cargo.lock', 'readme.md'];
  const sources = await Promise.all(filenames.map(read));
  JSON.parse(sources[0]);
  const ipkPattern = /\bcom\.ecklf\.luma_\d+\.\d+\.\d+_arm\.ipk\b/g;
  if (![...sources[3].matchAll(ipkPattern)].length) {
    throw new Error('Expected versioned Luma IPK examples in readme.md.');
  }
  const files = [
    {
      filename: 'appinfo.json',
      source: sources[0],
      updated: replaceVersion(
        sources[0],
        /^([ \t]*"version"[ \t]*:[ \t]*")[^"\r\n]+(")/gm,
        version,
        'appinfo.json',
      ),
    },
    {
      filename: 'Cargo.toml',
      source: sources[1],
      updated: updateTomlVersion(sources[1], '[package]', version, 'Cargo.toml'),
    },
    {
      filename: 'Cargo.lock',
      source: sources[2],
      updated: updateTomlVersion(sources[2], '[[package]]', version, 'Cargo.lock'),
    },
    {
      filename: 'readme.md',
      source: sources[3],
      updated: sources[3].replace(ipkPattern, `com.ecklf.luma_${version}_arm.ipk`),
    },
  ];
  if (bump !== undefined) {
    files.unshift({
      filename: 'package.json',
      source: project,
      updated: replaceVersion(project, /("version"\s*:\s*")[^"\r\n]+(")/g, version, 'package.json'),
    });
  }
  return { previous, version, files };
}

const writeChanged = (directory, files) =>
  Promise.all(
    files.map(({ filename, source, updated }) =>
      source === updated ? undefined : fs.writeFile(path.join(directory, filename), updated),
    ),
  );

export async function syncVersions(directory = root, { check = false } = {}) {
  const { version, files } = await planVersions(directory);
  const changed = files
    .filter(({ source, updated }) => source !== updated)
    .map(({ filename }) => filename);
  if (check && changed.length) {
    throw new Error(
      `Release versions are out of sync: ${changed.join(', ')}. Run pnpm version:sync.`,
    );
  }
  if (!check) await writeChanged(directory, files);
  return { version, changed };
}

// Bumps package.json to the next minor or major version and synchronizes the other release
// metadata from it. Every file is validated before any is written.
export async function bumpVersion(directory = root, kind = 'minor') {
  if (kind !== 'minor' && kind !== 'major') {
    throw new Error(`Unknown version bump "${kind}". Use minor or major.`);
  }
  const { previous, version, files } = await planVersions(directory, kind);
  await writeChanged(directory, files);
  return { previous, version };
}

// Sets package.json to an exact version (nightly builds use X.Y.N) and synchronizes the other
// release metadata from it. Every file is validated before any is written.
export async function setVersion(directory = root, version) {
  const { previous, files } = await planVersions(directory, { set: version });
  await writeChanged(directory, files);
  return { previous, version };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const bump = process.argv.indexOf('--bump');
  const set = process.argv.indexOf('--set');
  if (set !== -1) {
    const { previous, version } = await setVersion(root, process.argv[set + 1]);
    console.log(`Set ${previous} → ${version}.`);
  } else if (bump !== -1) {
    const { previous, version } = await bumpVersion(root, process.argv[bump + 1] ?? 'minor');
    console.log(`Bumped ${previous} → ${version}.`);
  } else {
    const check = process.argv.includes('--check');
    const { version, changed } = await syncVersions(root, { check });
    console.log(
      check
        ? `Release versions match ${version}.`
        : `Synchronized ${version}: ${changed.join(', ') || 'no changes'}.`,
    );
  }
}
