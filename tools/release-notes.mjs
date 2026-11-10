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
// Longest summary paragraph or bullet kept; longer ones end at a word with an ellipsis.
const MAX_ITEM = 500;
// GitHub rejects release bodies over 125,000 characters. Above this the summaries are dropped
// (titles only); a body that still doesn't fit fails before the release workflow moves main.
const MAX_BODY = 120_000;
// Concurrent GitHub API requests (GitHub's secondary limit is 100).
const CONCURRENCY = 8;

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
    // Top-level bullets, each with its wrapped (indented) continuation lines. Nested bullets
    // and their continuations are dropped; the list ends at unindented text.
    const bullets = [];
    let nested = false;
    for (const line of section.slice(content)) {
      if (bullet.test(line)) {
        bullets.push(line.replace(bullet, '').trim());
        nested = false;
      } else if (/^\s+[-*+]\s+/.test(line)) {
        nested = true;
      } else if (/^\s+\S/.test(line)) {
        if (!nested && bullets.length) bullets[bullets.length - 1] += ` ${line.trim()}`;
      } else if (line.trim() !== '') {
        break;
      }
    }
    const items = bullets.map((text) => `- ${clip(text)}`);
    return items.length > MAX_BULLETS
      ? [...items.slice(0, MAX_BULLETS), `- …and ${items.length - MAX_BULLETS} more`]
      : items;
  }
  const end = section.findIndex((line, index) => index > content && line.trim() === '');
  const paragraph = section
    .slice(content, end === -1 ? section.length : end)
    .map((line) => line.trim())
    .join(' ');
  return /^(https?:\/\/\S+|🤖.*)$/u.test(paragraph) ? [] : [clip(paragraph)];
}

export function clip(text, limit = MAX_ITEM) {
  if (text.length <= limit) return text;
  const cut = text.slice(0, limit);
  const space = cut.lastIndexOf(' ');
  return `${(space > limit / 2 ? cut.slice(0, space) : cut).trimEnd()}…`;
}

export function whatsNew(pulls, { summaries = true } = {}) {
  const sections = [];
  for (const group of GROUPS) {
    const entries = pulls.filter((pull) => pull.group === group.key);
    if (!entries.length) continue;
    const items = entries.map((pull) => {
      const summary = summaries ? summaryOf(pull.body) : [];
      if (summary.length === 1 && !summary[0].startsWith('- ')) {
        return `- **${pull.title}.** ${summary[0]}`;
      }
      if (!summary.length) return `- ${pull.title}`;
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

// The notes with every summary, or titles only when that would be too long for GitHub.
export function fittedNotes({ pulls, ...context }, limit = MAX_BODY) {
  const full = composeNotes({ ...context, summary: whatsNew(pulls) });
  if (full.length <= limit) return full;
  console.warn(`::warning::Release notes are ${full.length} characters; listing titles only.`);
  const short = composeNotes({ ...context, summary: whatsNew(pulls, { summaries: false }) });
  if (short.length <= limit) return short;
  throw new Error(`Release notes are ${short.length} characters, over GitHub's limit.`);
}

// Runs fn over items with at most `limit` calls in flight, keeping the results in order.
export async function mapLimit(items, limit, fn) {
  const results = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await fn(items[index]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
  return results;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Seconds to wait before retrying a rate-limited response, or null when it isn't one.
export function retryDelay(status, headers) {
  const after = Number(headers.get('retry-after'));
  if (
    status === 429 ||
    (status === 403 && (after || headers.get('x-ratelimit-remaining') === '0'))
  ) {
    if (after) return Math.min(after, 120);
    const reset = Number(headers.get('x-ratelimit-reset'));
    return reset ? Math.min(Math.max(reset - Date.now() / 1000, 1), 120) : 60;
  }
  return null;
}

async function github(path, { method = 'GET', body } = {}) {
  const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  if (!token) throw new Error('GH_TOKEN or GITHUB_TOKEN is required.');
  for (let attempt = 1; ; attempt++) {
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
    if (response.ok) return response.json();
    const delay = retryDelay(response.status, response.headers);
    if (delay !== null && attempt < 4) {
      console.warn(`GitHub rate limit on ${path}; retrying in ${Math.round(delay)} s.`);
      await sleep(delay * 1000);
      continue;
    }
    throw new Error(`GitHub ${method} ${path}: ${response.status} ${await response.text()}`);
  }
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
  const pulls = await mapLimit(pullNumbers(generated, repository), CONCURRENCY, async (number) => {
    const pull = await github(`/repos/${repository}/pulls/${number}`);
    const files = await changedFiles(repository, number);
    return { number, title: pull.title, body: pull.body, group: classify(files) };
  });
  return fittedNotes({ pulls, generated, nightly, target, previous });
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
