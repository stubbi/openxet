const escape = value => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");

export function resultPage(body, operation) {
  const saved = body.code === "request_saved";
  const removed = body.code === "request_removed";
  const title = saved ? "Your request is saved" : removed ? "Request removed" : operation === "cancel" ? "Request not removed" : "Request not saved";
  const privateLink = saved ? `<p><a class="button secondary" href="/cancel#${escape(body.receipt)}">Save your private removal link</a></p><p class="small">This link can remove your request. Keep it private.</p>` : "";
  const back = operation === "cancel" ? "/cancel" : "/";
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="referrer" content="strict-origin"><meta name="robots" content="noindex, nofollow"><title>${title} · openxet</title><link rel="stylesheet" href="/waitlist.css"><link rel="icon" href="/favicon.svg"></head><body><main class="panel"><a class="brand" href="https://openxet.com/">openxet</a><p class="eyebrow">Hosted storage waitlist</p><h1>${title}.</h1><p>${escape(body.message)}</p><p>${saved ? "We’ll use your email to contact you about hosted access. No account has been created, and no email has been sent yet." : escape(body.hint)}</p>${privateLink}<div class="actions"><a href="https://openxet.com/">Back to openxet</a>${!saved && !removed ? `<a href="${back}">Return to the form</a>` : ""}</div><p class="small"><a href="https://openxet.com/privacy/">How we use your email</a></p></main></body></html>`;
}
