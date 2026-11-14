// Generates typed Rust API clients from OpenAPI specs.
//
//   pnpm openapi           # regenerate src/<api>-api.rs from the vendored specs
//   pnpm openapi --check   # fail if the generated files are out of date (run by `pnpm check`)
//   pnpm openapi --fetch   # refresh tools/openapi/<api>.json from upstream, then regenerate
//
// Vendored specs keep only the operations listed below (and the schemas they reference), so a
// 3 MB upstream spec does not land in the repository. Add an operation here, then --fetch.
//
// The generated code uses `src/openapi.rs` for HTTP (blocking ureq, bounded replies) and serde
// for decoding. Mapping rules:
// - object → struct; a property is `T` when the schema lists it as required, else `Option<T>`
// - allOf → one struct with the merged properties
// - additionalProperties → HashMap<String, T>
// - string enum → enum with an `Unknown` fallback, so a value added upstream does not break decoding
// - every type is also Serialize, so replies can be cached on disk and read back
// - integer → i64 (TMDB revenues exceed i32), number → f64, other schemas → serde_json::Value
// - array query parameters → one comma-separated value (OpenAPI form style, explode: false)
import fs from 'node:fs/promises';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import YAML from 'yaml';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const APIS = [
  {
    name: 'motn',
    source:
      'https://raw.githubusercontent.com/movieofthenight/streaming-availability-api/main/openapi.yaml',
    operations: [
      'getCountries',
      'getCountry',
      'getGenres',
      'getShow',
      'searchShowsByFilters',
      'searchShowsByTitle',
      'getTopShows',
      'getChanges',
    ],
  },
  {
    name: 'tmdb',
    source: 'https://developer.themoviedb.org/openapi/tmdb-api.json',
    operations: [
      'search-multi',
      'movie-details',
      'tv-series-details',
      'collection-details',
      'find-by-id',
    ],
    // Corrections where the upstream spec disagrees with what the API sends, found by
    // decoding captured fixtures. Each is a path into the spec and the value to put there;
    // `add` patches describe fields the spec leaves out, such as `append_to_response` blocks.
    patches: [
      {
        // Season ratings are decimals such as 7.9, not integers.
        path: [
          'paths',
          '/3/tv/{series_id}',
          'get',
          'responses',
          '200',
          'content',
          'application/json',
          'schema',
          'properties',
          'seasons',
          'items',
          'properties',
          'vote_average',
          'type',
        ],
        value: 'number',
      },
      {
        // `append_to_response=release_dates`: certifications by country (PG-13 in the US).
        add: true,
        path: [
          'paths',
          '/3/movie/{movie_id}',
          'get',
          'responses',
          '200',
          'content',
          'application/json',
          'schema',
          'properties',
          'release_dates',
        ],
        value: {
          type: 'object',
          properties: {
            results: {
              type: 'array',
              items: {
                type: 'object',
                properties: {
                  iso_3166_1: { type: 'string' },
                  release_dates: {
                    type: 'array',
                    items: {
                      type: 'object',
                      properties: {
                        certification: { type: 'string' },
                        type: { type: 'integer' },
                      },
                    },
                  },
                },
              },
            },
          },
        },
      },
      {
        // `append_to_response=content_ratings`: TV ratings by country (TV-14 in the US).
        add: true,
        path: [
          'paths',
          '/3/tv/{series_id}',
          'get',
          'responses',
          '200',
          'content',
          'application/json',
          'schema',
          'properties',
          'content_ratings',
        ],
        value: {
          type: 'object',
          properties: {
            results: {
              type: 'array',
              items: {
                type: 'object',
                properties: {
                  iso_3166_1: { type: 'string' },
                  rating: { type: 'string' },
                },
              },
            },
          },
        },
      },
      {
        // `append_to_response=alternative_titles`: a film's other titles by country. Movie of
        // the Night matches these ("Star Wars: Return of the Jedi").
        add: true,
        path: [
          'paths',
          '/3/movie/{movie_id}',
          'get',
          'responses',
          '200',
          'content',
          'application/json',
          'schema',
          'properties',
          'alternative_titles',
        ],
        value: {
          type: 'object',
          properties: {
            titles: {
              type: 'array',
              items: {
                type: 'object',
                properties: { iso_3166_1: { type: 'string' }, title: { type: 'string' } },
              },
            },
          },
        },
      },
      {
        // `append_to_response=alternative_titles`: a series' other titles ("Star Wars: Andor").
        add: true,
        path: [
          'paths',
          '/3/tv/{series_id}',
          'get',
          'responses',
          '200',
          'content',
          'application/json',
          'schema',
          'properties',
          'alternative_titles',
        ],
        value: {
          type: 'object',
          properties: {
            results: {
              type: 'array',
              items: {
                type: 'object',
                properties: { iso_3166_1: { type: 'string' }, title: { type: 'string' } },
              },
            },
          },
        },
      },
    ],
  },
];

const specFile = (api) => path.join(ROOT, 'tools/openapi', `${api.name}.json`);
const outFile = (api) => path.join(ROOT, 'src', `${api.name}-api.rs`);

// ---------------------------------------------------------------------------------------------
// Fetching

function lookup(spec, ref) {
  if (!ref.startsWith('#/')) throw new Error(`Only local $refs are supported: ${ref}`);
  let value = spec;
  for (const part of ref.slice(2).split('/')) {
    value = value?.[part.replaceAll('~1', '/').replaceAll('~0', '~')];
  }
  if (value === undefined) throw new Error(`Unresolved $ref ${ref}`);
  return value;
}

function collectRefs(spec, value, refs) {
  if (Array.isArray(value)) {
    for (const item of value) collectRefs(spec, item, refs);
  } else if (value && typeof value === 'object') {
    if (typeof value.$ref === 'string' && !refs.has(value.$ref)) {
      refs.add(value.$ref);
      collectRefs(spec, lookup(spec, value.$ref), refs);
    }
    for (const item of Object.values(value)) collectRefs(spec, item, refs);
  }
}

async function fetchSpec(api) {
  const response = await fetch(api.source, { redirect: 'error' });
  if (!response.ok) throw new Error(`${api.source}: HTTP ${response.status}`);
  const text = await response.text();
  const spec = api.source.endsWith('.json') ? JSON.parse(text) : YAML.parse(text);

  const paths = {};
  const found = new Set();
  for (const [route, item] of Object.entries(spec.paths)) {
    for (const [method, operation] of Object.entries(item)) {
      if (!api.operations.includes(operation?.operationId)) continue;
      paths[route] ??= item.parameters ? { parameters: item.parameters } : {};
      paths[route][method] = operation;
      found.add(operation.operationId);
    }
  }
  const missing = api.operations.filter((id) => !found.has(id));
  if (missing.length) throw new Error(`${api.name}: operations not in spec: ${missing}`);

  // Keep only the components the selected operations reference, plus security schemes.
  const refs = new Set();
  collectRefs(spec, paths, refs);
  const components = {};
  if (spec.components?.securitySchemes) {
    components.securitySchemes = spec.components.securitySchemes;
  }
  for (const ref of [...refs].sort()) {
    const [, , kind, name] = ref.split('/');
    (components[kind] ??= {})[name] = spec.components[kind][name];
  }

  const trimmed = {
    ...spec,
    info: { ...spec.info, 'x-luma-source': api.source },
    paths,
    components,
  };
  for (const patch of api.patches ?? []) {
    const parent = patch.path.slice(0, -1).reduce((value, key) => value?.[key], trimmed);
    const key = patch.path.at(-1);
    if (!parent || (!patch.add && !(key in parent)) || (patch.add && key in parent)) {
      throw new Error(`${api.name}: stale patch ${patch.path}`);
    }
    parent[key] = patch.value;
  }
  if (api.patches?.length) trimmed.info['x-luma-patches'] = api.patches;
  await fs.mkdir(path.dirname(specFile(api)), { recursive: true });
  await fs.writeFile(specFile(api), `${JSON.stringify(trimmed, null, 2)}\n`);
  console.log(`${api.name}: ${found.size} operations from ${api.source}`);
}

// ---------------------------------------------------------------------------------------------
// Rust naming

const KEYWORDS = new Set(
  'as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self static struct super trait true type unsafe use where while yield'.split(
    ' ',
  ),
);

function words(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .split(/[^A-Za-z0-9]+/)
    .filter(Boolean);
}

const pascal = (name) =>
  words(name)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join('')
    .replace(/^(\d)/, '_$1');

function snake(name) {
  const result = words(name)
    .map((w) => w.toLowerCase())
    .join('_')
    .replace(/^(\d)/, '_$1');
  return KEYWORDS.has(result) ? `r#${result}` : result;
}

/** Naming an array's item type after the array: "productionCompanies" → "ProductionCompany". */
function singular(name) {
  if (/ies$/.test(name)) return name.replace(/ies$/, 'y');
  if (/(ss|us)$/.test(name)) return name;
  if (/s$/.test(name)) return name.slice(0, -1);
  return name;
}

/** First paragraph of a description as `///` lines wrapped at 96 columns. */
function doc(text, indent = '') {
  if (!text) return '';
  const paragraph = String(text)
    .trim()
    .split(/\n\s*\n/)[0]
    .replace(/\s+/g, ' ')
    .trim();
  if (!paragraph) return '';
  const lines = [];
  let line = '';
  for (const word of paragraph.split(' ')) {
    if (line && indent.length + 4 + line.length + 1 + word.length > 96) {
      lines.push(line);
      line = word;
    } else {
      line = line ? `${line} ${word}` : word;
    }
  }
  lines.push(line);
  return lines.map((l) => `${indent}/// ${l}\n`).join('');
}

// ---------------------------------------------------------------------------------------------
// Code generation

class Generator {
  constructor(api, spec) {
    this.api = api;
    this.spec = spec;
    this.items = []; // emitted Rust items, in definition order
    this.names = new Map(); // Rust type name → identity key
    this.refNames = new Map(); // $ref → Rust type name
    this.enumNames = new Map(); // enum values → Rust type name (identical enums are shared)
    this.usesMap = false;
  }

  resolve(schema) {
    while (schema?.$ref) schema = lookup(this.spec, schema.$ref);
    return schema;
  }

  claim(name, key) {
    let candidate = name;
    for (let n = 2; this.names.has(candidate) && this.names.get(candidate) !== key; n++) {
      candidate = `${name}${n}`;
    }
    this.names.set(candidate, key);
    return candidate;
  }

  /** The Rust type for `schema`; inline objects and enums are named after `hint`. */
  type(schema, hint) {
    if (schema?.$ref) {
      const ref = schema.$ref;
      if (!ref.startsWith('#/components/schemas/')) return this.type(this.resolve(schema), hint);
      if (this.refNames.has(ref)) return this.refNames.get(ref);
      const resolved = this.resolve(schema);
      const name = pascal(ref.split('/').pop());
      // Primitives stay primitives rather than becoming aliases.
      if (!this.isNamed(resolved)) {
        const inner = this.type(resolved, name);
        this.refNames.set(ref, inner);
        return inner;
      }
      const claimed = this.claim(name, ref);
      this.refNames.set(ref, claimed);
      this.define(resolved, claimed);
      return claimed;
    }
    if (!schema || typeof schema !== 'object') return 'serde_json::Value';
    if (this.isNamed(schema)) {
      if (schema.enum) {
        const key = JSON.stringify(schema.enum);
        if (this.enumNames.has(key)) return this.enumNames.get(key);
        const name = this.claim(hint, key);
        this.enumNames.set(key, name);
        this.define(schema, name);
        return name;
      }
      const name = this.claim(hint, Symbol(hint));
      this.define(schema, name);
      return name;
    }
    const type = this.primitive(schema);
    switch (type) {
      case 'string':
        return 'String';
      case 'integer':
        return 'i64';
      case 'number':
        return 'f64';
      case 'boolean':
        return 'bool';
      case 'array':
        return `Vec<${this.type(schema.items, singular(hint))}>`;
      case 'object':
        if (schema.additionalProperties && typeof schema.additionalProperties === 'object') {
          this.usesMap = true;
          return `HashMap<String, ${this.type(schema.additionalProperties, `${hint}Value`)}>`;
        }
        return 'serde_json::Value';
      default:
        return 'serde_json::Value';
    }
  }

  primitive(schema) {
    let type = schema.type;
    if (Array.isArray(type)) type = type.find((t) => t !== 'null');
    if (!type && (schema.properties || schema.allOf)) type = 'object';
    if (!type && schema.const !== undefined) type = typeof schema.const;
    return type;
  }

  /** Objects with properties, allOf and string enums become named Rust items. */
  isNamed(schema) {
    if (!schema || typeof schema !== 'object') return false;
    if (schema.allOf) return true;
    if (this.primitive(schema) === 'string' && Array.isArray(schema.enum)) return true;
    return this.primitive(schema) === 'object' && !!schema.properties;
  }

  define(schema, name) {
    if (schema.enum) return this.defineEnum(schema, name);
    let properties = { ...schema.properties };
    let required = [...(schema.required ?? [])];
    for (const part of schema.allOf ?? []) {
      const resolved = this.resolve(part);
      properties = { ...properties, ...resolved.properties };
      required = [...required, ...(resolved.required ?? [])];
    }
    const slot = this.items.push('') - 1; // reserve the position before nested types
    let body = '';
    for (const [property, propertySchema] of Object.entries(properties)) {
      const field = snake(property);
      const fieldHint = `${name}${pascal(property)}`;
      let type = this.type(propertySchema, fieldHint);
      if (!required.includes(property)) type = `Option<${type}>`;
      body += doc(this.resolve(propertySchema)?.description ?? propertySchema.description, '    ');
      if (field.replace(/^r#/, '') !== property) body += `    #[serde(rename = "${property}")]\n`;
      body += `    pub ${field}: ${type},\n`;
    }
    this.items[slot] =
      `${doc(schema.description)}#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]\n` +
      `pub struct ${name} {\n${body}}\n`;
  }

  defineEnum(schema, name) {
    const variants = schema.enum.map((value) => [String(value), pascal(String(value))]);
    const descriptions = schema['x-enumDescriptions'] ?? {};
    let body = '';
    for (const [value, variant] of variants) {
      body += doc(descriptions[value], '    ');
      body += `    #[serde(rename = "${value}")]\n    ${variant},\n`;
    }
    body += '    /// A value added to the API after this client was generated.\n';
    body += '    #[serde(other)]\n    Unknown,\n';
    const arms = variants.map(
      ([value, variant]) => `            Self::${variant} => "${value}",\n`,
    );
    this.items.push(
      `${doc(schema.description)}#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]\n` +
        `pub enum ${name} {\n${body}}\n\n` +
        `impl ${name} {\n    pub fn as_str(self) -> &'static str {\n        match self {\n` +
        `${arms.join('')}            Self::Unknown => "",\n        }\n    }\n}\n\n` +
        `impl QueryValue for ${name} {\n    fn to_query(&self) -> String {\n` +
        `        self.as_str().to_owned()\n    }\n}\n`,
    );
  }

  operation(route, pathItem, method, operation) {
    if (method !== 'get') throw new Error(`${operation.operationId}: only GET is supported`);
    const id = operation.operationId;
    const parameters = [...(pathItem.parameters ?? []), ...(operation.parameters ?? [])].map(
      (p) => ({ hint: p.$ref?.split('/').pop(), ...this.resolve(p) }),
    );
    const paramsName = `${pascal(id)}Params`;

    let fields = '';
    let fill = '';
    const pathArgs = [];
    let defaultable = true;
    for (const parameter of parameters) {
      const field = snake(parameter.name);
      const hint = pascal(parameter.hint?.replace(/Param$/, '') ?? parameter.name);
      const type = this.type(parameter.schema, hint);
      const schema = this.resolve(parameter.schema);
      if (this.primitive(schema) === 'array' && parameter.explode !== false) {
        throw new Error(`${id}.${parameter.name}: only explode: false arrays are supported`);
      }
      const required = parameter.in === 'path' || parameter.required === true;
      if (required && this.isNamed(schema)) defaultable = false;
      fields += doc(parameter.description, '    ');
      fields += `    pub ${field}: ${required ? type : `Option<${type}>`},\n`;
      if (parameter.in === 'path') {
        pathArgs.push(`encode_path(&params.${field}.to_query())`);
      } else if (parameter.in === 'query') {
        fill += required
          ? `        query.add("${parameter.name}", &params.${field});\n`
          : `        if let Some(value) = &params.${field} {\n            query.add("${parameter.name}", value);\n        }\n`;
      } else {
        throw new Error(`${id}.${parameter.name}: ${parameter.in} parameters are not supported`);
      }
    }
    if (fields) {
      this.items.push(
        `/// Parameters of [\`Client::${snake(id)}\`].\n` +
          `#[derive(Clone, Debug, ${defaultable ? 'Default, ' : ''}PartialEq)]\n` +
          `pub struct ${paramsName} {\n${fields}}\n`,
      );
    }

    const ok = this.resolve(operation.responses?.['200']);
    const schema = ok?.content?.['application/json']?.schema;
    const returns = this.type(schema, `${pascal(id)}Response`);
    const template = route.replace(/\{[^}]+\}/g, '{}');
    const pathExpr = pathArgs.length
      ? `format!("${template}", ${pathArgs.join(', ')})`
      : `"${route}".to_owned()`;
    const summary = doc(operation.summary, '    ') + (operation.summary ? '    ///\n' : '');
    return (
      `${summary}    /// \`GET ${route}\`\n` +
      `    pub fn ${snake(id)}(&self${fields ? `, params: &${paramsName}` : ''}) -> Result<${returns}, Error> {\n` +
      `        let path = ${pathExpr};\n` +
      `        ${fill ? 'let mut query' : 'let query'} = Query::default();\n${fill}` +
      `        self.http.get(&path, &query)\n    }\n`
    );
  }

  generate() {
    const methods = [];
    for (const [route, pathItem] of Object.entries(this.spec.paths)) {
      for (const [method, operation] of Object.entries(pathItem)) {
        if (method === 'parameters') continue;
        methods.push(this.operation(route, pathItem, method, operation));
      }
    }
    const servers = (this.spec.servers ?? []).map((s) => `    "${s.url}",\n`).join('');
    const info = this.spec.info;
    const header =
      `//! ${info.title} ${info.version} client, generated by \`pnpm openapi\` from\n` +
      `//! \`tools/openapi/${this.api.name}.json\` (trimmed from <${info['x-luma-source']}>).\n` +
      `//! Do not edit: change \`tools/openapi.mjs\` and regenerate. HTTP lives in \`openapi.rs\`.\n` +
      `use crate::openapi::{Error, Http, Query, QueryValue, encode_path};\n` +
      `use serde::{Deserialize, Serialize};\n` +
      (this.usesMap ? 'use std::collections::HashMap;\n' : '') +
      `\n/// Server URLs from the spec, in spec order.\npub const SERVERS: &[&str] = &[\n${servers}];\n\n` +
      `#[derive(Clone)]\npub struct Client {\n    http: Http,\n}\n\n` +
      `impl Client {\n    pub fn new(http: Http) -> Self {\n        Self { http }\n    }\n\n` +
      methods.join('\n') +
      `}\n`;
    return `${header}\n${this.items.join('\n')}`;
  }
}

function rustfmt(source) {
  return execFileSync('rustfmt', ['--edition', '2024', '--emit', 'stdout'], {
    input: source,
    encoding: 'utf8',
  });
}

async function generate(api) {
  const spec = JSON.parse(await fs.readFile(specFile(api), 'utf8'));
  const generator = new Generator(api, spec);
  const body = generator.generate();
  return rustfmt(body);
}

async function main() {
  const { values } = parseArgs({
    options: { check: { type: 'boolean' }, fetch: { type: 'boolean' } },
  });
  if (values.fetch) for (const api of APIS) await fetchSpec(api);
  const stale = [];
  for (const api of APIS) {
    const code = await generate(api);
    const file = outFile(api);
    const current = await fs.readFile(file, 'utf8').catch(() => null);
    if (current === code) continue;
    if (values.check) {
      stale.push(path.relative(ROOT, file));
    } else {
      await fs.writeFile(file, code);
      console.log(`wrote ${path.relative(ROOT, file)}`);
    }
  }
  if (stale.length) {
    throw new Error(`Generated API clients are out of date (run pnpm openapi): ${stale}`);
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
