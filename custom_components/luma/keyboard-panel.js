// The page a tapped "Type on your phone" notification opens in the Home Assistant app: one text
// box, focused so the phone's keyboard comes up, whose text goes into the TV's open text field.
// Home Assistant's frontend loads this as a custom panel and hands it `hass` (see keyboard.py).

const REFRESH_MS = 3000;

const STYLE = `
  :host {
    display: block;
    min-height: 100vh;
    background: var(--primary-background-color, #fafafa);
    color: var(--primary-text-color, #212121);
    font-family: var(--paper-font-body1_-_font-family, system-ui, sans-serif);
  }
  header {
    display: flex;
    align-items: center;
    gap: 4px;
    height: var(--header-height, 56px);
    padding: 0 12px;
    padding-top: env(safe-area-inset-top, 0px);
    background: var(--app-header-background-color, var(--primary-color, #03a9f4));
    color: var(--app-header-text-color, #fff);
    font-size: 20px;
  }
  main {
    max-width: 560px;
    margin: 0 auto;
    padding: 24px 16px;
  }
  select,
  input {
    box-sizing: border-box;
    width: 100%;
    padding: 14px 16px;
    border: 1px solid var(--divider-color, #8888);
    border-radius: 12px;
    background: var(--card-background-color, #fff);
    color: inherit;
    /* 16px or more: iOS zooms the page when a smaller field is focused. */
    font: inherit;
    font-size: 18px;
  }
  select {
    margin-bottom: 16px;
  }
  input:focus {
    outline: 2px solid var(--primary-color, #03a9f4);
    outline-offset: 1px;
  }
  form {
    display: flex;
    gap: 8px;
  }
  button {
    flex: none;
    padding: 0 20px;
    border: 0;
    border-radius: 12px;
    background: var(--primary-color, #03a9f4);
    color: var(--text-primary-color, #fff);
    font: inherit;
    font-size: 16px;
    font-weight: 500;
  }
  button:disabled {
    opacity: 0.5;
  }
  p {
    margin: 12px 4px 0;
    color: var(--secondary-text-color, #727272);
    line-height: 1.4;
  }
  p.error {
    color: var(--error-color, #db4437);
  }
`;

class LumaKeyboardPanel extends HTMLElement {
  constructor() {
    super();
    this._hass = null;
    this._tvs = [];
    // The TV the notification was for; the page also works opened by hand, for any TV.
    this._entry = new URLSearchParams(window.location.search).get('entry');
    this._sending = false;
    this._timer = null;
    this._refresh = () => {
      if (document.visibilityState === 'visible') this._load();
    };

    const root = this.attachShadow({ mode: 'open' });
    const style = document.createElement('style');
    style.textContent = STYLE;
    const header = document.createElement('header');
    this._menu = document.createElement('ha-menu-button');
    const title = document.createElement('span');
    title.textContent = 'Type on the TV';
    header.append(this._menu, title);

    this._select = document.createElement('select');
    this._select.setAttribute('aria-label', 'TV');
    this._select.hidden = true;
    this._select.addEventListener('change', () => {
      this._entry = this._select.value;
      this._render();
      this._input.focus();
    });

    this._form = document.createElement('form');
    this._input = document.createElement('input');
    this._input.type = 'text';
    this._input.maxLength = 120;
    this._input.autocomplete = 'off';
    this._input.autofocus = true;
    this._input.setAttribute('enterkeyhint', 'send');
    this._input.setAttribute('aria-label', 'Text for the TV');
    this._input.addEventListener('input', () => this._render());
    this._button = document.createElement('button');
    this._button.type = 'submit';
    this._button.textContent = 'Send';
    this._form.append(this._input, this._button);
    this._form.addEventListener('submit', (event) => {
      event.preventDefault();
      this._send();
    });

    this._note = document.createElement('p');
    this._note.setAttribute('role', 'status');
    const main = document.createElement('main');
    main.append(this._select, this._form, this._note);
    root.append(style, header, main);
    this._render();
  }

  set hass(hass) {
    const first = this._hass === null;
    this._hass = hass;
    this._menu.hass = hass;
    if (first) this._load();
  }

  set narrow(narrow) {
    this._menu.narrow = narrow;
  }

  connectedCallback() {
    document.addEventListener('visibilitychange', this._refresh);
    this._timer = setInterval(this._refresh, REFRESH_MS);
    // After the frontend has put the panel on screen, so the phone's keyboard comes up.
    requestAnimationFrame(() => this._input.focus());
  }

  disconnectedCallback() {
    document.removeEventListener('visibilitychange', this._refresh);
    clearInterval(this._timer);
  }

  _tv() {
    return this._tvs.find((tv) => tv.entry_id === this._entry) ?? null;
  }

  async _load() {
    if (!this._hass) return;
    try {
      const { tvs } = await this._hass.callWS({ type: 'luma/keyboard/tvs' });
      this._tvs = tvs;
    } catch {
      return; // Home Assistant is reconnecting; the next refresh asks again.
    }
    if (!this._tv()) {
      // Opened without a TV: the one waiting for text, else the first.
      const waiting = this._tvs.find((tv) => tv.prompt) ?? this._tvs[0];
      this._entry = waiting ? waiting.entry_id : null;
    }
    this._select.replaceChildren(
      ...this._tvs.map((tv) => {
        const option = document.createElement('option');
        option.value = tv.entry_id;
        option.textContent = tv.title;
        return option;
      }),
    );
    if (this._entry) this._select.value = this._entry;
    this._select.hidden = this._tvs.length < 2;
    this._render();
  }

  _render(message, error = false) {
    const tv = this._tv();
    this._input.placeholder = tv?.prompt ?? 'Text';
    this._button.disabled = this._sending || !tv || !this._input.value.trim();
    if (message === undefined && this._note.dataset.kept) return;
    this._note.classList.toggle('error', error);
    if (message !== undefined) {
      this._note.textContent = message;
      this._note.dataset.kept = 'yes';
    } else if (!tv) {
      this._note.textContent = this._tvs.length ? '' : 'No Luma TV is set up or switched on.';
    } else {
      this._note.textContent = `What you send goes into the text field that is open on ${tv.title}.`;
    }
  }

  async _send() {
    const tv = this._tv();
    const text = this._input.value.trim();
    if (!tv || !text || this._sending || !this._hass) return;
    this._sending = true;
    delete this._note.dataset.kept;
    this._render();
    try {
      await this._hass.callWS({ type: 'luma/keyboard/type', entry_id: tv.entry_id, text });
      this._input.value = '';
      this._sending = false;
      this._render(`Sent to ${tv.title}.`);
    } catch (err) {
      this._sending = false;
      const reason =
        err?.code === 'no_field'
          ? `No text field is open on ${tv.title}. Open one, such as search, then send again.`
          : (err?.message ?? 'Could not reach the TV.');
      this._render(reason, true);
    }
    this._input.focus();
    this._load();
  }
}

customElements.define('luma-keyboard-panel', LumaKeyboardPanel);
