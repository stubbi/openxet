import assert from 'node:assert/strict';
import test from 'node:test';
import { describeUpload } from '../website/assets/file-example.mjs';

const stored = ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H'];

test('a small edit reuses seven parts and sends the new part', () => {
  const result = describeUpload(stored, ['A', 'B', 'I', 'D', 'E', 'F', 'G', 'H']);
  assert.equal(result.reusedCount, 7);
  assert.equal(result.newCount, 1);
  assert.deepEqual(result.parts.filter(part => !part.reused), [{ label: 'I', reused: false }]);
  assert.deepEqual(result.uploadParts, ['I']);
});

test('the same file needs no new parts', () => {
  const result = describeUpload(stored, stored);
  assert.equal(result.reusedCount, 8);
  assert.equal(result.newCount, 0);
  assert.ok(result.parts.every(part => part.reused));
  assert.deepEqual(result.uploadParts, []);
});

test('an entirely new file has no parts to reuse', () => {
  const result = describeUpload(stored, ['I', 'J', 'K', 'L', 'M', 'N', 'O', 'P']);
  assert.equal(result.reusedCount, 0);
  assert.equal(result.newCount, 8);
  assert.ok(result.parts.every(part => !part.reused));
  assert.deepEqual(result.uploadParts, ['I', 'J', 'K', 'L', 'M', 'N', 'O', 'P']);
});

test('reuse follows content, not a part’s position in the file', () => {
  const next = [...stored].reverse();
  const result = describeUpload(stored, next);
  assert.equal(result.reusedCount, 8);
  assert.equal(result.newCount, 0);
  assert.deepEqual(result.parts.map(part => part.label), next);
  assert.deepEqual(stored, ['A', 'B', 'C', 'D', 'E', 'F', 'G', 'H']);
});
