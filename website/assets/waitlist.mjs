const service = 'https://openxet-waitlist.jannes73978.chatgpt.site';
const receiptPattern = /^[A-Za-z0-9_-]{43}$/;
const retryMessage = 'We couldn’t save your request. Your address is still here; try again.';

class WaitlistError extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

export function removalLink(receipt) {
  if (typeof receipt !== 'string' || !receiptPattern.test(receipt)) throw new Error('Invalid removal receipt.');
  return `${service}/cancel#${receipt}`;
}

export async function requestWaitlist(email, website, send = fetch) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 15000);
  try {
    const response = await send(`${service}/api/waitlist`, {
      method: 'POST',
      headers: {'Content-Type': 'application/json', Accept: 'application/json'},
      credentials: 'omit',
      referrerPolicy: 'strict-origin',
      signal: controller.signal,
      body: JSON.stringify({email, website}),
    });
    if (response.status === 429) {
      const seconds = Number(response.headers.get('Retry-After'));
      const minutes = Math.ceil(seconds / 60);
      const wait = Number.isFinite(seconds) && seconds > 0 && seconds <= 86400
        ? `Please wait ${minutes} ${minutes === 1 ? 'minute' : 'minutes'} before trying again.`
        : 'Please wait before trying again.';
      throw new WaitlistError('rate_limited', `${wait} Your address is still here.`);
    }
    if (response.status === 400) {
      const body = await response.json();
      if (body.schema === 'openxet.waitlist.v1' && body.code === 'invalid_email') {
        throw new WaitlistError('invalid_email', 'Enter an email address, such as you@example.com.');
      }
    }
    if (response.status !== 201) throw new Error(retryMessage);
    const body = await response.json();
    if (body.schema !== 'openxet.waitlist.v1' || body.code !== 'request_saved' ||
        typeof body.receipt !== 'string' || !receiptPattern.test(body.receipt)) throw new Error(retryMessage);
    return body.receipt;
  } catch (error) {
    if (error instanceof WaitlistError) throw error;
    throw new Error(retryMessage);
  } finally {
    clearTimeout(timeout);
  }
}

if (typeof document !== 'undefined') {
  const copyButton = document.querySelector('#copy-removal-link');
  if (copyButton) {
    copyButton.hidden = false;
    copyButton.addEventListener('click', async () => {
      const link = document.querySelector('#removal-link').href;
      const status = document.querySelector('#copy-status');
      try {
        await navigator.clipboard.writeText(link);
        status.textContent = 'Private link copied. Save it somewhere only you can access.';
      } catch {
        const fallback = document.querySelector('#removal-copy-fallback');
        fallback.hidden = false;
        const input = fallback.querySelector('input');
        input.value = link;
        input.focus();
        input.select();
        status.textContent = 'Copy the selected link and save it privately.';
      }
    });
  }
  const form = document.querySelector('#waitlist-form');
  form?.elements.email.addEventListener('input', () => {
    form.elements.email.removeAttribute('aria-invalid');
    form.elements.email.setAttribute('aria-describedby', 'privacy-note');
  });
  if (form) form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const button = form.querySelector('button[type="submit"]');
    if (button.disabled) return;
    const status = document.querySelector('#form-status');
    const email = form.elements.email;
    status.textContent = 'Saving your request…';
    status.classList.remove('form-error');
    button.disabled = true;
    email.readOnly = true;
    button.textContent = 'Saving…';
    form.setAttribute('aria-busy', 'true');
    try {
      const receipt = await requestWaitlist(email.value, form.elements.website.value);
      document.querySelector('#removal-link').href = removalLink(receipt);
      const confirmation = document.querySelector('#waitlist-confirmation');
      confirmation.hidden = false;
      form.hidden = true;
      status.textContent = '';
      email.value = '';
      confirmation.querySelector('h2').focus();
    } catch (error) {
      status.textContent = error.message;
      status.classList.add('form-error');
      if (error.code === 'invalid_email') {
        email.setAttribute('aria-invalid', 'true');
        email.setAttribute('aria-describedby', 'privacy-note form-status');
        email.focus();
      } else status.focus();
    } finally {
      button.disabled = false;
      email.readOnly = false;
      button.textContent = 'Join the waitlist';
      form.removeAttribute('aria-busy');
    }
  });
}
