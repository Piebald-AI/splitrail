import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { messages } from './locales.mjs';
import {
  OTHER, localDateKey, metricValue, periodKey, selectRange, inRange, matchesFilters, granularityOptions, bucketKeys,
  sumRows, groupRows, totalTokens, cacheShare, colorSlots, stackedSeries, weekdayHourGrid, niceStep, topTenthShare, median, csv,
  percentile, SESSION_SHAPES, histogram,
} from './data.mjs';

const row = (overrides = {}) => ({
  date: '2025-12-31', hour: 23, model: 'a', project: 'p', tool: 't', session: 's',
  inputTokens: 10, outputTokens: 3, cachedTokens: 2, reasoningTokens: 4, cost: 0.1, messages: 1, toolCalls: 0,
  filesRead: 0, filesEdited: 0, filesAdded: 0, terminalCommands: 0, ...overrides,
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

test('totals, grouping, cache share, and concentration', () => {
  const rows = [row({ session: 'x' }), row({ session: 'y', cost: 0.3, cachedTokens: 30, filesRead: 2, filesEdited: 1, terminalCommands: 3 })];
  const totals = sumRows(rows);
  assert.equal(totals.sessions.size, 2);
  assert.equal(cacheShare(totals), 32 / 52);
  assert.deepEqual([totals.filesRead, totals.filesEdited, totals.filesAdded, totals.terminalCommands], [2, 1, 0, 3]);
  assert.equal(groupRows(rows, item => item.session).get('y').cost, 0.3);
  assert.deepEqual(topTenthShare([90, 5, 1, 1, 1, 1, 1, 0, 0, 0]), { count: 1, share: 0.9 });
  assert.equal(median([5, 1, 3]), 3);
  assert.equal(percentile([], 0.9), 0);
  assert.equal(percentile([5, 1, 3], 0.9), 5);
  assert.equal(percentile([1, 2, 3, 4], 0.5), 2);
});

test('weekday grid is Monday-first and axis steps are round', () => {
  const grid = weekdayHourGrid([row({ date: '2026-01-05', hour: 9 }), row({ date: '2026-01-04', hour: 9 })], 'cost');
  assert.equal(grid[0][9], 0.1);
  assert.equal(grid[6][9], 0.1);
  assert.equal(niceStep(87), 25);
  assert.equal(niceStep(4e8), 1e8);
  assert.equal(niceStep(0), 1);
});

test('replies metric counts model responses', () => {
  assert.equal(metricValue(row({ messages: 3 }), 'requests'), 3);
});

test('every locale presents the requests metric as replies', () => {
  for (const locale of Object.values(messages)) assert.equal(locale.requests, locale.replies);
});

test('session shape bands cover every value without gaps or overlap', () => {
  // Bands are ascending and contiguous, with the last one open-ended.
  for (const bands of Object.values(SESSION_SHAPES)) {
    assert.equal(bands.at(-1)[1], Infinity);
    for (let i = 1; i < bands.length; i++) assert.equal(bands[i][0], bands[i - 1][1] + 1);
  }
  // 0 falls in no message band (messages start at 1), so it is dropped.
  assert.deepEqual(histogram([0, 1, 5, 6, 15, 16, 30, 31, 60, 61, 120, 121, 9999], SESSION_SHAPES.messages), [2, 2, 2, 2, 2, 2]);
  assert.deepEqual(histogram([0, 1, 5, 6, 20, 21, 50, 51, 100, 101], SESSION_SHAPES.toolCalls), [1, 2, 2, 2, 2, 1]);
  // Values outside every band are dropped rather than misfiled.
  assert.deepEqual(histogram([-1, 3], [[0, 10]]), [1]);
});

test('model rankings guard cost-only and reasoning-only rows', () => {
  const source = readFileSync(new URL('./app.js', import.meta.url), 'utf8');
  const rankSource = source.slice(source.indexOf('function rankSub('), source.indexOf('function rankCard('));
  const rankSub = runInNewContext(`${rankSource}\nrankSub`, {
    totalTokens, cacheShare, t: key => messages.en[key],
    fmt: { cost: value => `$${value.toFixed(2)}`, percent: (value, digits) => `${(value * 100).toFixed(digits)}%` },
  });
  const costOnly = row({ cost: 1, inputTokens: 0, outputTokens: 0, cachedTokens: 0, reasoningTokens: 0 });
  assert.equal(rankSub('model', 'a', sumRows([costOnly])), 'No price · cache 0%');
  assert.equal(rankSub('model', 'a', sumRows([{ ...costOnly, reasoningTokens: 100 }])), 'No price · cache 0%');
  assert.equal(rankSub('model', 'a', sumRows([{ ...costOnly, inputTokens: 1e6 }])), '$1.00/M · cache 0%');
  assert.equal(rankSub('model', 'a', sumRows([row({ cost: 0 })])), 'No price · cache 17%');
});

test('CSV preserves exact values, escapes fields, and neutralizes formula labels', () => {
  assert.equal(csv([['a,b', 'a"b', 'a\nb', '=1+1', '  @x', -2, 1234567]]),
    '"a,b","a""b","a\nb","\'=1+1","\'  @x","-2","1234567"\r\n');
});

test('CSV neutralizes leading tabs and carriage returns without changing numbers', () => {
  for (const value of ['\tplain', '\rplain', '\t=1+1', '\r@x', '  +1', '-2']) {
    assert.equal(csv([[value]]), `"'${value}"\r\n`);
  }
  assert.equal(csv([[-2, 'plain']]), '"-2","plain"\r\n');
});

test('all locales provide the same labels and interpolation signatures', () => {
  for (const locale of Object.values(messages)) {
    assert.deepEqual(Object.keys(locale).sort(), Object.keys(messages.en).sort());
    for (const [key, value] of Object.entries(messages.en)) {
      assert.equal(typeof locale[key], typeof value, key);
      if (typeof value === 'function') assert.equal(locale[key].length, value.length, key);
      if (typeof value === 'object') assert.deepEqual(Object.keys(locale[key]).sort(), Object.keys(value).sort());
    }
  }
});

test('bookmarked dates must be valid calendar dates', () => {
  const source = readFileSync(new URL('./app.js', import.meta.url), 'utf8');
  const stateSource = source.slice(source.indexOf('function defaultState('), source.indexOf('function writeHash('));
  const location = { hash: '' };
  const readHash = runInNewContext(`${stateSource}\nreadHash`, {
    location, URLSearchParams, localDateKey, noon: key => new Date(`${key}T12:00:00`),
    VIEWS: ['overview', 'sessions', 'breakdown'], RANGES: ['today', '7', '30', '90', 'all'],
    METRICS: ['cost', 'tokens', 'requests'], DIMENSIONS: ['tool', 'model', 'project'],
    GRANULARITIES: ['hour', 'day', 'week', 'month'], SORTS: ['recent', 'cost', 'tokens', 'requests', 'duration'],
    BREAKDOWNS: ['period', 'project', 'model', 'tool'],
    SHAPES: ['messages', 'duration', 'toolCalls'],
  });
  for (const day of ['2026-02-30', '2026-99-01', 'garbage']) {
    location.hash = `#day=${day}`;
    assert.equal(readHash().day, null);
  }
  location.hash = '#view=sessions&day=2024-02-29&tool=one&tool=two';
  const state = readHash();
  assert.equal(state.day, '2024-02-29');
  assert.equal(state.view, 'sessions');
  assert.deepEqual([...state.filters.tool], ['one', 'two']);
});
