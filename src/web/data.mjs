// Pure aggregation over hourly usage rows (one per local date, hour, tool,
// model, project, and session). Everything the dashboard shows derives here.

export const OTHER = '\u0000other';

export function rowTokens(row) {
  return row.inputTokens + row.outputTokens + row.cachedTokens;
}

export function metricValue(row, metric) {
  return metric === 'cost' ? row.cost : rowTokens(row);
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
  return { cost: 0, inputTokens: 0, outputTokens: 0, cachedTokens: 0, reasoningTokens: 0, messages: 0, sessions: new Set(), dates: new Set() };
}

const SUMMED = ['cost', 'inputTokens', 'outputTokens', 'cachedTokens', 'reasoningTokens', 'messages'];

function addRow(totals, row) {
  for (const field of SUMMED) totals[field] += row[field];
  totals.sessions.add(`${row.tool}:${row.session}`);
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

// A round axis step such that four steps cover `max`.
export function niceStep(max) {
  if (!(max > 0)) return 1;
  const raw = max / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map(multiple => multiple * magnitude).find(step => step >= raw);
}
