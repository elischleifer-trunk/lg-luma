import { SERVICES, change, checkKeys, describeKey, loadKeys, post } from './keys.js';

const element = (id) => document.getElementById(id);
const form = element('keys');
const status = element('status');
const token = location.pathname.split('/')[1];
const removing = new Set();
let busy = false;

function message(text, error = false) {
  status.textContent = text;
  status.classList.toggle('error', error);
}

function render(keys) {
  for (const service of SERVICES) {
    element(`${service}-state`).textContent = describeKey(keys[service]);
    const remove = element(`${service}-remove`);
    remove.hidden = !keys[service].set;
    remove.textContent = remove.dataset.label ??= remove.textContent.trim();
    element(service).value = '';
  }
  removing.clear();
  element('save').disabled = false;
}

function controls(disabled) {
  for (const control of form.querySelectorAll('input, button')) control.disabled = disabled;
}

async function save(body, working) {
  if (busy) return;
  busy = true;
  controls(true);
  message(working);
  const controller = new AbortController();
  // Each key is checked against its service before saving, which can take a few seconds.
  const timeout = setTimeout(() => controller.abort(), 35000);
  try {
    const result = await post(fetch, token, body, controller.signal);
    render(checkKeys(result));
    message(result.warnings?.length ? result.warnings.join(' ') : 'Saved to your TV.');
  } catch (error) {
    message(
      error.name === 'AbortError'
        ? 'The TV did not respond in time. Keep the TV dialog open and try again.'
        : error instanceof TypeError
          ? 'The connection was lost. Reopen the dialog on your TV and scan the new code.'
          : error.message,
      true,
    );
  } finally {
    clearTimeout(timeout);
    busy = false;
    controls(false);
  }
}

form.addEventListener('submit', (event) => {
  event.preventDefault();
  const body = change({ motn: element('motn').value, tmdb: element('tmdb').value });
  if (!Object.keys(body).length) {
    message('Paste a key first.', true);
    return;
  }
  save(body, 'Checking with each service, then saving to your TV…');
});

for (const service of SERVICES) {
  element(`${service}-remove`).addEventListener('click', (event) => {
    // A second press confirms, so a stray tap cannot remove a key.
    if (!removing.has(service)) {
      removing.add(service);
      event.currentTarget.textContent = 'Press again to remove';
      return;
    }
    save({ [service]: '' }, 'Removing…');
  });
}

loadKeys(fetch)
  .then(render)
  .catch(() => {
    message('Could not read the keys on your TV. Close and reopen the dialog.', true);
  });
