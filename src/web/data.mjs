// Pure aggregation over hourly usage rows (one per local date, hour, tool,
// model, project, and session). Everything the dashboard shows derives here.

export const OTHER = '\u0000other';
export const SESSION_PAGE_SIZE = 50;

// Quote every field and keep labels from being interpreted as spreadsheet formulas.
export function csv(rows) {
  return rows.map(row => row.map(value => {
    let text = String(value ?? '');
    if (typeof value === 'string' && /^(?:[\t\r]|\s*[=+\-@])/.test(text)) text = `'${text}`;
    return `"${text.replaceAll('"', '""')}"`;
  }).join(',')).join('\r\n') + '\r\n';
}

export function rowTokens(row) {
  return row.inputTokens + row.outputTokens + row.cachedTokens;
}

export function metricValue(row, metric) {
  if (metric === 'cost') return row.cost;
  if (metric === 'requests') return row.messages;
  return rowTokens(row);
}

export function localDateKey(date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}

export function shiftDate(key, days) {
  const date = new Date(`${key}T12:00:00`);
  date.setDate(date.getDate() + days);
  return localDateKey(date);
}

export function periodKey(date, hour, granularity) {
  if (granularity === 'hour') return `${date}T${String(hour).padStart(2, '0')}`;
  if (granularity === 'day') return date;
  if (granularity === 'month') return date.slice(0, 7);
  const monday = new Date(`${date}T12:00:00`);
  monday.setDate(monday.getDate() - (monday.getDay() + 6) % 7);
  return localDateKey(monday);
}

// Inclusive local dates for a preset ('today', '7', '30', '90', 'all') or a
// drilled-into day, plus the equal-length period before it. "Today" compares
// with yesterday up to the current hour.
export function selectRange(range, day, now, firstDate) {
  const today = localDateKey(now);
  if (day) return { first: day, last: day, days: 1, prev: { first: shiftDate(day, -1), last: shiftDate(day, -1) } };
  if (range === 'today') {
    const yesterday = shiftDate(today, -1);
    return { first: today, last: today, days: 1, prev: { first: yesterday, last: yesterday, maxHour: now.getHours() } };
  }
  if (range === 'all') {
    const first = firstDate && firstDate < today ? firstDate : today;
    const days = Math.round((new Date(`${today}T12:00:00`) - new Date(`${first}T12:00:00`)) / 86400000) + 1;
    return { first, last: today, days, prev: null };
  }
  const days = Number(range);
  const first = shiftDate(today, 1 - days);
  return { first, last: today, days, prev: { first: shiftDate(first, -days), last: shiftDate(first, -1) } };
}

export function inRange(row, range) {
  return row.date >= range.first && row.date <= range.last && (range.maxHour === undefined || row.hour <= range.maxHour);
}

// Selections within a dimension combine; dimensions narrow each other.
export function matchesFilters(row, filters) {
  return ['tool', 'model', 'project'].every(key => !filters[key].size || filters[key].has(row[key]));
}

// Intervals offered for a range; the first is the default. A single day only
// has hours, and every longer range offers all four.
export function granularityOptions(days) {
  if (days <= 1) return ['hour'];
  if (days <= 120) return ['day', 'hour', 'week', 'month'];
  if (days <= 730) return ['week', 'hour', 'day', 'month'];
  return ['month', 'hour', 'day', 'week'];
}

// Every bucket in the range so gaps stay visible; hours stop at `now`.
export function bucketKeys(range, granularity, now) {
  const today = localDateKey(now);
  const keys = [];
  for (let date = range.first; date <= range.last; date = shiftDate(date, 1)) {
    const lastHour = granularity !== 'hour' ? 0 : date === today ? now.getHours() : 23;
    for (let hour = 0; hour <= lastHour; hour++) {
      const key = periodKey(date, hour, granularity);
      if (keys.at(-1) !== key) keys.push(key);
    }
  }
  return keys;
}

function emptyTotals() {
  return { cost: 0, inputTokens: 0, outputTokens: 0, cachedTokens: 0, reasoningTokens: 0, messages: 0, toolCalls: 0, sessions: new Set(), dates: new Set() };
}

const SUMMED = ['cost', 'inputTokens', 'outputTokens', 'cachedTokens', 'reasoningTokens', 'messages', 'toolCalls'];

function addRow(totals, row) {
  for (const field of SUMMED) totals[field] += row[field];
  totals.sessions.add(row.session);
  totals.dates.add(row.date);
  return totals;
}

export function sumRows(rows) {
  const totals = emptyTotals();
  for (const row of rows) addRow(totals, row);
  return totals;
}

export function groupRows(rows, keyOf) {
  const groups = new Map();
  for (const row of rows) {
    const key = keyOf(row);
    if (!groups.has(key)) groups.set(key, emptyTotals());
    addRow(groups.get(key), row);
  }
  return groups;
}

export function totalTokens(totals) {
  return totals.inputTokens + totals.outputTokens + totals.cachedTokens;
}

// Share of prompt tokens served from cache.
export function cacheShare(totals) {
  const prompt = totals.inputTokens + totals.cachedTokens;
  return prompt > 0 ? totals.cachedTokens / prompt : 0;
}

// Colors follow entities: given the rows of the selected time range, the top
// `count` by cost (then tokens) own a slot, so tool, model, and project
// filters never repaint a series.
export function colorSlots(rows, dimension, count = 5) {
  const ranked = [...groupRows(rows, row => row[dimension])]
    .sort((a, b) => b[1].cost - a[1].cost || totalTokens(b[1]) - totalTokens(a[1]) || String(a[0]).localeCompare(String(b[0])));
  return new Map(ranked.slice(0, count).map(([key], slot) => [key, slot]));
}

// One value array per series over `keys`, slotted entities first and the
// remainder folded into OTHER last.
export function stackedSeries(rows, keys, granularity, dimension, metric, slots) {
  const index = new Map(keys.map((key, position) => [key, position]));
  const series = new Map();
  for (const row of rows) {
    const position = index.get(periodKey(row.date, row.hour, granularity));
    if (position === undefined) continue;
    const id = slots.has(row[dimension]) ? row[dimension] : OTHER;
    if (!series.has(id)) series.set(id, new Array(keys.length).fill(0));
    series.get(id)[position] += metricValue(row, metric);
  }
  const order = id => (slots.has(id) ? slots.get(id) : slots.size);
  return [...series].map(([id, values]) => ({ id, values })).sort((a, b) => order(a.id) - order(b.id));
}

// Monday-first weekday rows × 24 local hours.
export function weekdayHourGrid(rows, metric) {
  const weekdays = new Map();
  const grid = Array.from({ length: 7 }, () => new Array(24).fill(0));
  for (const row of rows) {
    if (!weekdays.has(row.date)) weekdays.set(row.date, (new Date(`${row.date}T12:00:00`).getDay() + 6) % 7);
    grid[weekdays.get(row.date)][row.hour] += metricValue(row, metric);
  }
  return grid;
}

// A round axis step such that four steps cover `max`.
export function niceStep(max) {
  if (!(max > 0)) return 1;
  const raw = max / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map(multiple => multiple * magnitude).find(step => step >= raw);
}

// How much of the total the largest tenth of the values hold.
export function topTenthShare(values) {
  if (!values.length) return { count: 0, share: 0 };
  const sorted = [...values].sort((a, b) => b - a);
  const count = Math.max(1, Math.round(sorted.length / 10));
  const total = sorted.reduce((sum, value) => sum + value, 0);
  return { count, share: total > 0 ? sorted.slice(0, count).reduce((sum, value) => sum + value, 0) / total : 0 };
}

export function median(values) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

// Value at `fraction` of the sorted values; 0.9 is the p90.
export function percentile(values, fraction) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1))];
}

// Session shape bands. The last band is open-ended, and `duration` is minutes.
export const SESSION_SHAPES = {
  messages: [[1, 5], [6, 15], [16, 30], [31, 60], [61, 120], [121, Infinity]],
  duration: [[0, 10], [11, 30], [31, 60], [61, 120], [121, 360], [361, Infinity]],
  toolCalls: [[0, 0], [1, 5], [6, 20], [21, 50], [51, 100], [101, Infinity]],
};

// How many values land in each [min, max] band, in band order.
export function histogram(values, buckets) {
  const counts = new Array(buckets.length).fill(0);
  for (const value of values) {
    const index = buckets.findIndex(([min, max]) => value >= min && value <= max);
    if (index !== -1) counts[index] += 1;
  }
  return counts;
}
