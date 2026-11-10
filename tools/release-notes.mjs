// Writes the notes for a GitHub release: a "What's new" section built from the merged pull
// requests, followed by GitHub's generated list of those pull requests.
//
//   node tools/release-notes.mjs --tag v2.21.0 --previous v2.20.0 --target <sha> [--nightly] \
//     --out notes.md
//
// --previous may be empty for the first release. Needs GH_TOKEN (or GITHUB_TOKEN) and
// GITHUB_REPOSITORY.
//
// Each pull request is grouped by the files it changed: anything in the TV app is "On the TV",
// the Home Assistant integration is "Home Assistant", and the rest (CI, tooling, tests, docs) is
// "Behind the scenes". App and Home Assistant entries carry the pull request's own summary: the
// "## Summary" section of its description (or the text before its first heading), first
// paragraph or top-level bullets. A clear PR description is therefore what users read.
import fs from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';

const MAX_BULLETS = 6;

const GROUPS = [
  {
    key: 'app',
    heading: 'On the TV',
    paths: [
      /^src\//,
      /^tv\//,
      /^tv-helper\/src\//,
      /^assets\//,
      /^overlay\//,
      /^vector-geometry\//,
      /^appinfo\.json$/,
    ],
  },
  {
    key: 'homeAssistant',
    heading: 'Home Assistant',
    paths: [/^custom_components\//, /^hacs\.json$/],
  },
];

// Test files under app directories don't make a change visible on the TV.
const TEST_FILE = /(^|\/)tests?(\.rs|\/)|-tests\.rs$/;

// Pull request numbers linked from GitHub's generated notes, in order, without duplicates.
export function pullNumbers(generated, repository) {
  const escaped = repository.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pattern = new RegExp(`https://github\\.com/${escaped}/pull/(\\d+)`, 'g');
  return [...new Set([...generated.matchAll(pattern)].map((match) => Number(match[1])))];
}

// The first group whose paths a changed file falls under, or 'internal'.
export function classify(files) {
  for (const group of GROUPS) {
    if (files.some((file) => !TEST_FILE.test(file) && group.paths.some((p) => p.test(file)))) {
      return group.key;
    }
  }
  return 'internal';
}

// The pull request's own summary as Markdown lines: the first paragraph, or the top-level
// bullets, of its "Summary" section (or of the text before its first heading). Empty when the
// description has none.
// Drops HTML comments (PR templates use them for hints), and everything from an unclosed `<!--`
// on. Repeats until nothing changes, so no opener survives by forming across a removed comment.
export function stripComments(text) {
  for (;;) {
    let result = '';
    let rest = text;
    for (;;) {
      const open = rest.indexOf('<!--');
      if (open === -1) {
        result += rest;
        break;
      }
      result += rest.slice(0, open);
      const close = rest.indexOf('-->', open + 4);
      if (close === -1) break;
      rest = rest.slice(close + 3);
    }
    if (result === text) return result;
    text = result;
  }
}

export function summaryOf(body) {
  const lines = stripComments((body ?? '').replace(/\r\n/g, '\n')).split('\n');
  const headings = lines.flatMap((line, index) => (/^#{1,6}\s/.test(line) ? [index] : []));
  const named = headings.find((index) => /^#{1,6}\s+(summary|what|why)\b/i.test(lines[index]));
  const start = named === undefined ? 0 : named + 1;
  const next = headings.find((index) => index >= start);
  const section = lines.slice(start, next ?? lines.length);

  const content = section.findIndex((line) => line.trim() !== '');
  if (content === -1) return [];
  const bullet = /^[-*+]\s+/;
  if (bullet.test(section[content])) {
    const bullets = section
      .slice(content)
      .filter((line) => bullet.test(line))
      .map((line) => `- ${line.replace(bullet, '').trim()}`);
    return bullets.length > MAX_BULLETS
      ? [...bullets.slice(0, MAX_BULLETS), `- …and ${bullets.length - MAX_BULLETS} more`]
      : bullets;
  }
  const end = section.findIndex((line, index) => index > content && line.trim() === '');
  const paragraph = section
    .slice(content, end === -1 ? section.length : end)
    .map((line) => line.trim())
    .join(' ');
  return /^(https?:\/\/\S+|🤖.*)$/u.test(paragraph) ? [] : [paragraph];
}

export function whatsNew(pulls) {
  const sections = [];
  for (const group of GROUPS) {
    const entries = pulls.filter((pull) => pull.group === group.key);
    if (!entries.length) continue;
    const items = entries.map((pull) => {
      const summary = summaryOf(pull.body);
      if (summary.length === 1 && !summary[0].startsWith('- ')) {
        return `- **${pull.title}.** ${summary[0]}`;
      }
      return [`- **${pull.title}**`, ...summary.map((line) => `  ${line}`)].join('\n');
    });
    sections.push(`### ${group.heading}\n\n${items.join('\n')}`);
  }
  const internal = pulls.filter((pull) => pull.group === 'internal');
  if (internal.length) {
    sections.push(
      `### Behind the scenes\n\n${internal.map((pull) => `- ${pull.title}`).join('\n')}`,
    );
  }
  return sections.join('\n\n');
}

export function composeNotes({ summary, generated, nightly, target, previous }) {
  const parts = [];
  if (nightly) {
    parts.push(
      `> [!WARNING]\n> Nightly build of \`main\` at ${target}. It has passed CI but is not a release. ` +
        `It includes everything merged since ${previous}.`,
    );
  }
  if (summary) parts.push(`## What's new\n\n${summary.trim()}`);
  parts.push(generated.trim());
  return `${parts.join('\n\n')}\n`;
}

async function github(path, { method = 'GET', body } = {}) {
  const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  if (!token) throw new Error('GH_TOKEN or GITHUB_TOKEN is required.');
  const response = await fetch(`https://api.github.com${path}`, {
    method,
    headers: {
      Accept: 'application/vnd.github+json',
      Authorization: `Bearer ${token}`,
      'X-GitHub-Api-Version': '2022-11-28',
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!response.ok) {
    throw new Error(`GitHub ${method} ${path}: ${response.status} ${await response.text()}`);
  }
  return response.json();
}

async function changedFiles(repository, number) {
  const files = [];
  for (let page = 1; ; page++) {
    const batch = await github(
      `/repos/${repository}/pulls/${number}/files?per_page=100&page=${page}`,
    );
    files.push(...batch.map((file) => file.filename));
    // GitHub lists at most 3000 files; a PR that large is classified from those.
    if (batch.length < 100 || page === 30) return files;
  }
}

export async function releaseNotes({ repository, tag, previous, target, nightly = false }) {
  const { body: generated } = await github(`/repos/${repository}/releases/generate-notes`, {
    method: 'POST',
    body: {
      tag_name: tag,
      target_commitish: target,
      ...(previous ? { previous_tag_name: previous } : {}),
    },
  });
  const pulls = await Promise.all(
    pullNumbers(generated, repository).map(async (number) => {
      const [pull, files] = await Promise.all([
        github(`/repos/${repository}/pulls/${number}`),
        changedFiles(repository, number),
      ]);
      return { number, title: pull.title, body: pull.body, group: classify(files) };
    }),
  );
  return composeNotes({ summary: whatsNew(pulls), generated, nightly, target, previous });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const { values } = parseArgs({
    options: {
      tag: { type: 'string' },
      previous: { type: 'string' },
      target: { type: 'string' },
      out: { type: 'string' },
      nightly: { type: 'boolean', default: false },
    },
  });
  const repository = process.env.GITHUB_REPOSITORY;
  if (
    !values.tag ||
    values.previous === undefined ||
    !values.target ||
    !values.out ||
    !repository
  ) {
    console.error(
      'Usage: GITHUB_REPOSITORY=owner/repo node tools/release-notes.mjs --tag <tag> ' +
        '--previous <tag> --target <sha> --out <file> [--nightly]',
    );
    process.exit(2);
  }
  const notes = await releaseNotes({ repository, ...values });
  await fs.writeFile(values.out, notes);
  console.log(`Release notes for ${values.tag} (since ${values.previous}): ${values.out}`);
}
