// Captures Movie of the Night (Streaming Availability API v4) responses once, so search can be
// built and tested against real data without spending API quota.
//
//   pnpm fixtures:streaming            # fetch only requests not already captured
//   pnpm fixtures:streaming --refresh  # re-fetch everything
//   pnpm fixtures:streaming --only search-title-star-wars
//
// The key comes from MOTN_API_KEY in the environment or in the gitignored `.env` at the repo root
// (see `.env.example`). It is sent only to the RapidAPI host below and never written to disk:
// captured files hold the request (path + query) and the response body, not headers.
//
// Output: tests/fixtures/streaming/<name>.json plus index.json listing every capture. Tests and
// the desktop preview read these files; they must never call the real API.
import fs from 'node:fs/promises';
import path from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const OUT = path.join(ROOT, 'tests/fixtures/streaming');
const HOST = 'streaming-availability.p.rapidapi.com';
const COUNTRY = 'us';

// One entry per captured request. Keep names stable: tests refer to them.
const REQUESTS = [
  // Reference data: service IDs, names, logos and catalog types per country.
  { name: 'countries-us', path: `/countries/${COUNTRY}`, query: { output_language: 'en' } },
  { name: 'genres', path: '/genres', query: { output_language: 'en' } },

  // Title search, the core of the search screen.
  ...[
    ['star-wars', 'star wars'], // many results, movies and series
    ['the-office', 'the office'], // series, US/UK versions
    ['dune', 'dune'], // short query, remakes
    ['amelie', 'amélie'], // non-ASCII title
    ['bluey', 'bluey'], // kids, Disney+
    ['the', 'the'], // very broad prefix
    ['no-results', 'zzqxjvw'], // empty result list
  ].map(([slug, title]) => ({
    name: `search-title-${slug}`,
    path: '/shows/search/title',
    query: { country: COUNTRY, title, series_granularity: 'show', output_language: 'en' },
  })),
  {
    name: 'search-title-star-wars-movies',
    path: '/shows/search/title',
    query: {
      country: COUNTRY,
      title: 'star wars',
      show_type: 'movie',
      series_granularity: 'show',
      output_language: 'en',
    },
  },

  // Browse rows for an empty search screen: popular titles on a few services.
  ...['netflix', 'prime.subscription', 'disney', 'hbo', 'apple'].map((catalog) => ({
    name: `popular-${catalog.split('.')[0]}`,
    path: '/shows/search/filters',
    query: {
      country: COUNTRY,
      catalogs: catalog,
      order_by: 'popularity_1week',
      series_granularity: 'show',
      output_language: 'en',
    },
  })),

  // Detail pages: a movie and a series with seasons/episodes.
  { name: 'show-movie-godfather', path: '/shows/tt0068646', query: { country: COUNTRY } },
  {
    name: 'show-series-the-office',
    path: '/shows/tt0386676',
    query: { country: COUNTRY, series_granularity: 'episode', output_language: 'en' },
  },
];

async function loadKey() {
  if (process.env.MOTN_API_KEY) return process.env.MOTN_API_KEY.trim();
  try {
    const env = await fs.readFile(path.join(ROOT, '.env'), 'utf8');
    const match = env.match(/^\s*MOTN_API_KEY\s*=\s*["']?([^"'\s#]+)/m);
    if (match) return match[1];
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  return null;
}

async function exists(file) {
  return fs.access(file).then(
    () => true,
    () => false,
  );
}

async function capture(request, key) {
  const url = new URL(request.path, `https://${HOST}`);
  for (const [k, v] of Object.entries(request.query)) url.searchParams.set(k, v);
  const response = await fetch(url, {
    headers: { 'X-RapidAPI-Key': key, 'X-RapidAPI-Host': HOST },
    redirect: 'error',
  });
  const remaining = response.headers.get('x-ratelimit-requests-remaining');
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`${request.name}: HTTP ${response.status} ${text.slice(0, 300)}`);
  }
  return { body: JSON.parse(text), remaining };
}

async function main() {
  const { values } = parseArgs({
    options: { refresh: { type: 'boolean' }, only: { type: 'string', multiple: true } },
  });
  const selected = values.only?.length
    ? REQUESTS.filter((request) => values.only.includes(request.name))
    : REQUESTS;
  if (values.only?.length && selected.length !== values.only.length) {
    throw new Error(`Unknown fixture name in --only. Known: ${REQUESTS.map((r) => r.name)}`);
  }

  await fs.mkdir(OUT, { recursive: true });
  const pending = [];
  for (const request of selected) {
    const file = path.join(OUT, `${request.name}.json`);
    if (values.refresh || !(await exists(file))) pending.push({ request, file });
  }
  if (pending.length === 0) {
    console.log(`All ${selected.length} fixtures already captured. Use --refresh to re-fetch.`);
  } else {
    const key = await loadKey();
    if (!key) {
      throw new Error('Set MOTN_API_KEY in the environment or in .env (see .env.example).');
    }
    console.log(`Fetching ${pending.length} of ${selected.length} fixtures from ${HOST}`);
    // Sequential on purpose: free plans are rate limited per second.
    for (const { request, file } of pending) {
      const { body, remaining } = await capture(request, key);
      const record = {
        request: { path: request.path, query: request.query },
        capturedAt: new Date().toISOString(),
        response: body,
      };
      await fs.writeFile(file, `${JSON.stringify(record, null, 2)}\n`);
      console.log(`  ${request.name}${remaining == null ? '' : ` (quota left: ${remaining})`}`);
    }
  }

  const index = REQUESTS.map(({ name, path: p, query }) => ({ name, path: p, query }));
  await fs.writeFile(path.join(OUT, 'index.json'), `${JSON.stringify(index, null, 2)}\n`);
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
