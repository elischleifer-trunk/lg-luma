// Logic for the streaming key page, kept free of the DOM so it can be tested with node:test.

export const SERVICES = ['motn', 'tmdb'];

export async function loadKeys(fetcher) {
  const response = await fetcher('keys.json', { cache: 'no-store' });
  if (!response.ok) throw new Error('Keys unavailable');
  return checkKeys(await response.json());
}

export function checkKeys(keys) {
  for (const service of SERVICES) {
    const entry = keys?.[service];
    if (typeof entry?.set !== 'boolean' || typeof entry.hint !== 'string') {
      throw new Error('Invalid key status');
    }
  }
  return keys;
}

/** How the page describes a stored key; the TV only ever sends its last four characters. */
export function describeKey(entry) {
  return entry.set ? `Saved key ends ${entry.hint}.` : 'Not set yet.';
}

/**
 * The change to send: only fields the owner typed in. Blank fields keep the stored key, so
 * opening the page and pressing save cannot erase anything.
 */
export function change(values) {
  const result = {};
  for (const service of SERVICES) {
    const value = (values[service] ?? '').trim();
    if (value) result[service] = value;
  }
  return result;
}

// The token in the address is the session's capability; writes repeat it as a header.
export async function post(fetcher, token, body, signal) {
  const response = await fetcher('save', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-Luma-Token': token },
    body: JSON.stringify(body),
    signal,
  });
  const result = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(result.error || 'The TV could not save these keys.');
  return result;
}
