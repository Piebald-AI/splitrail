import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import {
  OTHER, metricValue, periodKey, selectRange, inRange, matchesFilters, granularityOptions, bucketKeys,
  sumRows, groupRows, totalTokens, cacheShare, colorSlots, stackedSeries, niceStep,
} from './data.mjs';

const row = (overrides = {}) => ({
  date: '2025-12-31', hour: 23, model: 'a', project: 'p', tool: 't', session: 's',
  inputTokens: 10, outputTokens: 3, cachedTokens: 2, reasoningTokens: 4, cost: 0.1, messages: 1, ...overrides,
});
const filters = (overrides = {}) => ({ model: new Set(), project: new Set(), tool: new Set(), ...overrides });
const now = new Date(2026, 0, 2, 12, 30);

test('week and month keys cross year boundaries', () => {
  assert.equal(periodKey('2025-12-31', 23, 'week'), '2025-12-29');
  assert.equal(periodKey('2026-01-04', 0, 'week'), '2025-12-29');
  assert.equal(periodKey('2026-01-05', 0, 'week'), '2026-01-05');
  assert.equal(periodKey('2025-12-31', 23, 'month'), '2025-12');
  assert.equal(periodKey('2025-12-31', 7, 'hour'), '2025-12-31T07');
});

test('ranges include their first date and compare with the period before', () => {
  assert.deepEqual(selectRange('7', null, now), {
    first: '2025-12-27', last: '2026-01-02', days: 7, prev: { first: '2025-12-20', last: '2025-12-26' },
  });
  const today = selectRange('today', null, now);
  assert.deepEqual(today.prev, { first: '2026-01-01', last: '2026-01-01', maxHour: 12 });
  assert.ok(inRange(row({ date: '2026-01-01', hour: 12 }), today.prev));
  assert.ok(!inRange(row({ date: '2026-01-01', hour: 13 }), today.prev));
  assert.deepEqual(selectRange('30', '2025-12-31', now), {
    first: '2025-12-31', last: '2025-12-31', days: 1, prev: { first: '2025-12-30', last: '2025-12-30' },
  });
  assert.deepEqual(selectRange('all', null, now, '2025-12-01'), { first: '2025-12-01', last: '2026-01-02', days: 33, prev: null });
});

test('buckets keep gaps, stop at the current hour, and match the interval options', () => {
  assert.equal(bucketKeys(selectRange('today', null, now), 'hour', now).length, 13);
  assert.equal(bucketKeys(selectRange('7', null, now), 'day', now).length, 7);
  assert.deepEqual(bucketKeys(selectRange('7', null, now), 'week', now), ['2025-12-22', '2025-12-29']);
  assert.deepEqual(granularityOptions(1), ['hour']);
  assert.deepEqual(granularityOptions(30), ['day', 'hour', 'week', 'month']);
  assert.equal(granularityOptions(400)[0], 'week');
  assert.equal(granularityOptions(400).length, 4);
  assert.equal(bucketKeys(selectRange('7', null, now), 'hour', now).length, 6 * 24 + 13);
});

test('filters union within a dimension and intersect across dimensions', () => {
  const rows = [row(), row({ model: 'b' }), row({ model: 'c' }), row({ project: 'other' }), row({ tool: 'other' })];
  const selected = filters({ model: new Set(['a', 'b']), project: new Set(['p']), tool: new Set(['t']) });
  const matched = rows.filter(item => matchesFilters(item, selected));
  assert.equal(matched.length, 2);
  assert.equal(matched.reduce((sum, item) => sum + metricValue(item, 'tokens'), 0), 30);
});

test('stacked series preserve totals and keep entity colors when filtered', () => {
  const rows = Array.from({ length: 7 }, (_, index) => row({ model: `m${index}`, date: '2026-01-01', cost: 7 - index }));
  const slots = colorSlots(rows, 'model');
  assert.deepEqual([...slots], [['m0', 0], ['m1', 1], ['m2', 2], ['m3', 3], ['m4', 4]]);
  const keys = bucketKeys(selectRange('7', null, now), 'day', now);
  const series = stackedSeries(rows, keys, 'day', 'model', 'cost', slots);
  assert.equal(series.at(-1).id, OTHER);
  assert.equal(series.at(-1).values[keys.indexOf('2026-01-01')], 3);
  assert.equal(series.reduce((sum, item) => sum + item.values.reduce((a, b) => a + b, 0), 0), 28);
  const filtered = stackedSeries(rows.slice(1), keys, 'day', 'model', 'cost', slots);
  assert.deepEqual(filtered.map(item => item.id), ['m1', 'm2', 'm3', 'm4', OTHER]);
});

test('totals, grouping, cache share, and tool-specific session counts', () => {
  const rows = [row({ session: 'x' }), row({ session: 'y', cost: 0.3, cachedTokens: 30 })];
  const totals = sumRows(rows);
  assert.equal(totals.sessions.size, 2);
  assert.equal(cacheShare(totals), 32 / 52);
  assert.equal(groupRows(rows, item => item.session).get('y').cost, 0.3);
  assert.equal(sumRows([row(), row({ tool: 'other' })]).sessions.size, 2);
  assert.equal(metricValue(row(), 'tokens'), 15);
  assert.equal(cacheShare(sumRows([])), 0);
});

test('axis steps cover the maximum, including empty data', () => {
  assert.equal(niceStep(87), 25);
  assert.equal(niceStep(4e8), 1e8);
  assert.equal(niceStep(0), 1);
});

test('model rankings show no price when cost has no counted tokens', () => {
  const source = readFileSync(new URL('./app.js', import.meta.url), 'utf8');
  const rankSource = source.slice(source.indexOf('function rankSub('), source.indexOf('function rankCard('));
  const rankSub = runInNewContext(`${rankSource}\nrankSub`, {
    totalTokens, cacheShare,
    labels: { cacheShort: share => `cache ${share}`, perMillion: price => `${price}/M`, noPrice: 'No price' },
    fmt: {
      cost: value => new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD' }).format(value),
      percent: (value, digits) => `${(value * 100).toFixed(digits)}%`,
    },
  });
  const costOnly = row({ cost: 1, inputTokens: 0, outputTokens: 0, cachedTokens: 0, reasoningTokens: 0 });
  assert.equal(rankSub('model', sumRows([costOnly])), 'No price · cache 0%');
  assert.equal(rankSub('model', sumRows([{ ...costOnly, reasoningTokens: 100 }])), 'No price · cache 0%');
  assert.equal(rankSub('model', sumRows([{ ...costOnly, inputTokens: 1e6 }])), '$1.00/M · cache 0%');
  assert.equal(rankSub('model', sumRows([row({ cost: 0 })])), 'No price · cache 17%');
});
