// Captures Movie of the Night (Streaming Availability API v4) and TMDB responses once, so search
// and ranking can be built and tested against real data without spending API quota.
//
//   pnpm fixtures:streaming            # fetch only requests not already captured
//   pnpm fixtures:streaming --refresh  # re-fetch everything
//
// Keys come from the environment or the gitignored `.env` at the repo root (see `.env.example`):
// MOTN_API_KEY for Movie of the Night, TMDB_API_TOKEN (API Read Access Token) for TMDB. Movie of
// the Night Developers Platform keys (`motn-key-...`) go to api.movieofthenight.com/v4 as
// X-API-Key; RapidAPI keys go to RapidAPI, matching the official client's rule. Keys are never
// written to disk: captured files hold the request (path + query) and the response body.
//
// Output: tests/fixtures/streaming/ (Movie of the Night) and tests/fixtures/tmdb/, each with an
// index.json listing every capture. Tests read these files; they must never call the real APIs.
import fs from 'node:fs/promises';
import path from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const MOTN_BASE = 'https://api.movieofthenight.com/v4';
const RAPIDAPI_HOST = 'streaming-availability.p.rapidapi.com';
const TMDB_BASE = 'https://api.themoviedb.org';
const COUNTRY = 'us';
// Details are captured for this many top search results per query.
const TMDB_DETAILS_PER_SEARCH = 12;

// Queries the search screen is tested with: [slug, text].
const QUERIES = [
  ['gho', 'gho'], // 3-character minimum: Ghosts (series) and Ghostbusters (movies)
  ['ghostbusters', 'ghostbusters'], // a franchise: five US films, 1984 first
  ['star-wars', 'star wars'], // many results, movies and series
  ['the-office', 'the office'], // series, US/UK versions
  ['dune', 'dune'], // short query, remakes
  ['amelie', 'amélie'], // non-ASCII title
  ['bluey', 'bluey'], // kids, Disney+
  ['the', 'the'], // very broad prefix
  ['fan', 'fan'], // short prefix: the API pads with titles in other languages (One Piece)
  ['sup', 'sup'], // short prefix with no whole-word matches
  ['super', 'super'], // Super 8, Superbad; Superman is not a whole-word match
  ['no-results', 'zzqxjvw'], // empty result list
];

// Half-typed text, captured from TMDB only: its free search says which word the text is
// completed to before Movie of the Night is asked (relevance::complete). [slug, text].
const PARTIAL_QUERIES = [
  ['ghostb', 'ghostb'], // completed to "ghostbusters"
  ['star-w', 'star w'], // completed to "star wars"
];

// Movie of the Night requests. Keep names stable: tests refer to them.
const MOTN_REQUESTS = [
  // Reference data: service IDs, names, logos and catalog types per country.
  { name: 'countries-us', path: `/countries/${COUNTRY}`, query: { output_language: 'en' } },
  { name: 'genres', path: '/genres', query: { output_language: 'en' } },

  // Title search, the core of the search screen.
  ...QUERIES.map(([slug, title]) => ({
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

const APIS = {
  motn: {
    dir: path.join(ROOT, 'tests/fixtures/streaming'),
    variable: 'MOTN_API_KEY',
    // See https://docs.movieofthenight.com/guide/authentication
    endpoint: (key) =>
      key.startsWith('motn-key-')
        ? { base: MOTN_BASE, headers: { 'X-API-Key': key } }
        : {
            base: `https://${RAPIDAPI_HOST}`,
            headers: { 'X-RapidAPI-Key': key, 'X-RapidAPI-Host': RAPIDAPI_HOST },
          },
  },
  tmdb: {
    dir: path.join(ROOT, 'tests/fixtures/tmdb'),
    variable: 'TMDB_API_TOKEN',
    endpoint: (key) => ({ base: TMDB_BASE, headers: { Authorization: `Bearer ${key}` } }),
  },
};

async function loadKey(variable) {
  if (process.env[variable]) return process.env[variable].trim();
  try {
    const env = await fs.readFile(path.join(ROOT, '.env'), 'utf8');
    const match = env.match(new RegExp(`^\\s*${variable}\\s*=\\s*["']?([^"'\\s#]+)`, 'm'));
    if (match) return match[1];
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  return null;
}

/** Captures requests into one API's fixture directory, reusing files already captured. */
class Capture {
  constructor(api, refresh) {
    this.api = APIS[api];
    this.name = api;
    this.refresh = refresh;
    this.index = [];
    this.fetched = 0;
  }

  async get(name, route, query = {}) {
    if (!this.index.some((entry) => entry.name === name)) {
      this.index.push({ name, path: route, query });
    }
    const file = path.join(this.api.dir, `${name}.json`);
    if (!this.refresh) {
      const cached = await fs.readFile(file, 'utf8').catch(() => null);
      if (cached) return JSON.parse(cached).response;
    }
    this.key ??= await loadKey(this.api.variable);
    if (!this.key) {
      throw new Error(`Set ${this.api.variable} in the environment or in .env (.env.example).`);
    }
    const { base, headers } = this.api.endpoint(this.key);
    const url = new URL(base + route);
    for (const [k, v] of Object.entries(query)) url.searchParams.set(k, v);
    // Sequential on purpose: free plans are rate limited per second.
    const response = await fetch(url, { headers, redirect: 'error' });
    const text = await response.text();
    if (!response.ok) {
      throw new Error(`${this.name} ${name}: HTTP ${response.status} ${text.slice(0, 300)}`);
    }
    const body = JSON.parse(text);
    const record = { request: { path: route, query }, capturedAt: new Date().toISOString() };
    await fs.mkdir(this.api.dir, { recursive: true });
    await fs.writeFile(file, `${JSON.stringify({ ...record, response: body }, null, 2)}\n`);
    this.fetched += 1;
    const remaining = response.headers.get('x-ratelimit-requests-remaining');
    console.log(`  ${this.name} ${name}${remaining == null ? '' : ` (quota left: ${remaining})`}`);
    return body;
  }

  async finish() {
    await fs.mkdir(this.api.dir, { recursive: true });
    const index = `${JSON.stringify(this.index, null, 2)}\n`;
    await fs.writeFile(path.join(this.api.dir, 'index.json'), index);
    console.log(`${this.name}: ${this.index.length} fixtures, ${this.fetched} fetched now`);
  }
}

async function captureTmdb(tmdb, motnResults) {
  const collections = new Set();
  const details = async (kind, id) => {
    const body = await tmdb.get(`${kind}-${id}`, `/3/${kind}/${id}`, { language: 'en-US' });
    if (body.belongs_to_collection?.id) collections.add(body.belongs_to_collection.id);
  };
  // Every Movie of the Night search result, as the search screen enriches them by TMDB ID.
  // The app appends `alternative_titles` to its details call; captured separately here so
  // older details fixtures stay as they are. Tests merge the two.
  for (const tmdbId of motnResults) {
    const [kind, id] = tmdbId.split('/');
    if ((kind === 'movie' || kind === 'tv') && /^\d+$/.test(id)) {
      await details(kind, id);
      await tmdb.get(`${kind}-${id}-titles`, `/3/${kind}/${id}/alternative_titles`, {});
    }
  }
  // One search for movies and series together: TMDB has no batch endpoints, so this is
  // the cheapest way to get every result's popularity, votes and original language.
  for (const [slug, text] of QUERIES) {
    const search = await tmdb.get(`search-multi-${slug}`, '/3/search/multi', {
      query: text,
      include_adult: 'false',
      language: 'en-US',
      page: '1',
    });
    const titles = search.results.filter((result) => ['movie', 'tv'].includes(result.media_type));
    for (const result of titles.slice(0, TMDB_DETAILS_PER_SEARCH)) {
      await details(result.media_type, result.id);
    }
  }
  for (const [slug, text] of PARTIAL_QUERIES) {
    await tmdb.get(`search-multi-${slug}`, '/3/search/multi', {
      query: text,
      include_adult: 'false',
      language: 'en-US',
      page: '1',
    });
  }
  for (const id of [...collections].sort((a, b) => a - b)) {
    await tmdb.get(`collection-${id}`, `/3/collection/${id}`, { language: 'en-US' });
  }
}

async function main() {
  const { values } = parseArgs({ options: { refresh: { type: 'boolean' } } });
  const motn = new Capture('motn', values.refresh);
  const motnResults = new Set();
  for (const request of MOTN_REQUESTS) {
    const body = await motn.get(request.name, request.path, request.query);
    if (request.name.startsWith('search-title-')) {
      for (const show of body) if (show.tmdbId) motnResults.add(show.tmdbId);
    }
  }
  await motn.finish();
  const tmdb = new Capture('tmdb', values.refresh);
  await captureTmdb(tmdb, motnResults);
  await tmdb.finish();
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
