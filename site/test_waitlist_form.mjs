import test from 'node:test';
import assert from 'node:assert/strict';
import { requestWaitlist, removalLink } from '../website/assets/waitlist.mjs';

const receipt = 'a'.repeat(43);
const saved = () => new Response(JSON.stringify({schema: 'openxet.waitlist.v1', code: 'request_saved', receipt}), {status: 201});

test('accepts only a persisted response with a valid private receipt', async () => {
  let request;
  const result = await requestWaitlist('reader@example.com', '', async (url, options) => {
    request = {url, ...options};
    return saved();
  });
  assert.equal(result, receipt);
  assert.equal(request.url, 'https://openxet-waitlist.jannes73978.chatgpt.site/api/waitlist');
  assert.equal(request.method, 'POST');
  assert.equal(request.credentials, 'omit');
  assert.deepEqual(JSON.parse(request.body), {email: 'reader@example.com', website: ''});
  assert.equal(removalLink(result), `https://openxet-waitlist.jannes73978.chatgpt.site/cancel#${receipt}`);
});

test('server failure and malformed success never become a saved request', async () => {
  for (const response of [
    new Response('{}', {status: 500}),
    new Response('not JSON', {status: 201}),
    new Response(JSON.stringify({schema: 'openxet.waitlist.v1', code: 'request_saved'}), {status: 201}),
    new Response(JSON.stringify({schema: 'openxet.waitlist.v1', code: 'request_saved', receipt}), {status: 200}),
    new Response(JSON.stringify({schema: 'other', code: 'request_saved', receipt}), {status: 201}),
  ]) {
    await assert.rejects(requestWaitlist('reader@example.com', '', async () => response), /try again/i);
  }
});

test('network failure gives a recoverable message without echoing input', async () => {
  await assert.rejects(requestWaitlist('reader@example.com', '', async () => { throw new Error('network'); }),
    (error) => /try again/i.test(error.message) && !error.message.includes('reader@example.com'));
});

test('rate limits produce a wait instruction and no receipt', async () => {
  await assert.rejects(requestWaitlist('reader@example.com', '', async () => new Response('{}', {status: 429})), /wait/i);
});

test('a rate limit tells the visitor how long to wait using bounded server retry time', async () => {
  await assert.rejects(requestWaitlist('reader@example.com', '', async () => new Response('{}', {
    status: 429, headers: {'Retry-After': '125'},
  })), /3 minutes/);
  await assert.rejects(requestWaitlist('reader@example.com', '', async () => new Response('{}', {
    status: 429, headers: {'Retry-After': 'invalid'},
  })), /Please wait/);
});

test('invalid email points to the field without echoing the server message', async () => {
  await assert.rejects(requestWaitlist('a@b', '', async () => new Response(JSON.stringify({
    schema: 'openxet.waitlist.v1', code: 'invalid_email', message: 'untrusted details',
  }), {status: 400})), error => error.code === 'invalid_email' && /email address/.test(error.message) && !error.message.includes('untrusted'));
});

test('a removal link never accepts arbitrary content or an address', () => {
  for (const value of ['', 'reader@example.com', 'https://elsewhere.test', 'a'.repeat(42), '<'.repeat(43)]) {
    assert.throws(() => removalLink(value), /receipt/i);
  }
});
