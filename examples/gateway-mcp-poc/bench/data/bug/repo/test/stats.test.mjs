import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mean, median } from '../src/stats.mjs';

test('mean', () => assert.equal(mean([1, 2, 3, 4]), 2.5));
test('median of sorted odd list', () => assert.equal(median([1, 2, 3]), 2));
test('median of unsorted list', () => assert.equal(median([9, 1, 5]), 5));
test('median of unsorted even list', () => assert.equal(median([8, 2, 6, 4]), 5));
test('median does not change its input', () => {
  const xs = [3, 1, 2];
  median(xs);
  assert.deepEqual(xs, [3, 1, 2]);
});
