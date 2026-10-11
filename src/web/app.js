import {
  OTHER, shiftDate, periodKey, selectRange, inRange, matchesFilters,
  granularityOptions, bucketKeys, sumRows, groupRows, totalTokens, cacheShare, colorSlots, stackedSeries,
  niceStep,
} from './data.mjs';

const labels = {
  summary: 'Key figures', rankings: 'Rankings', sessions: 'Sessions',
  loading: 'Loading local usage…', scanning: 'Scanning…', loadError: detail => `Could not load data: ${detail}`, noData: 'No local usage records found.',
  scanned: (time, tools, sessions) => `Local only · scanned ${time} · ${tools} tools · ${sessions} sessions`,
  range: 'Time range', today: 'Today', last7: '7 days', last30: '30 days', last90: '90 days', all: 'All',
  lastDays: days => `Last ${days} days`, allDays: days => `All time (${days} days)`,
  vsPrevious: days => `vs previous ${days} days`, vsYesterday: 'vs yesterday so far', vsPreviousDay: 'vs previous day',
  hourly: 'hourly', clearDrill: 'Leave day view',
  tool: 'Tool', model: 'Model', project: 'Project',
  searchIn: name => `Filter ${name.toLowerCase()}s`, noMatches: 'No matches', clearSelection: 'Clear selection',
  removeFilter: name => `Remove filter: ${name}`, clearFilters: 'Clear filters',
  metric: 'Metric', cost: 'Cost', tokens: 'Tokens',
  estimatedCost: 'Estimated cost (API prices)', totalTokens: 'Total tokens', cacheShare: 'Cache share',
  noComparison: 'All history, nothing to compare', previousZero: 'previous period was 0', unchanged: 'unchanged', points: 'pts',
  dailyAverage: (cost, active, days) => `${cost} per day · active ${active} of ${days} days`, asOf: time => `As of ${time}`, wholeDay: 'Whole day',
  replies: count => `${count} replies`, cacheSub: 'Share of prompt tokens served from cache',
  cached: 'Cached', input: 'Input', output: 'Output',
  costMayBeLow: 'Cost may be understated', unpriced: (models, tokens) => `${tokens} tokens from ${models} are priced at $0, likely missing prices.`,
  every: { hour: 'Hourly', day: 'Daily', week: 'Weekly', month: 'Monthly' },
  trendTitle: (every, metric) => `${every} ${metric === 'cost' ? 'cost' : 'tokens'}`,
  stackedBy: (dimension, range) => `Stacked by ${dimension.toLowerCase()} · ${range}`,
  group: 'Group', interval: 'Interval', intervals: { hour: 'Hour', day: 'Day', week: 'Week', month: 'Month' },
  drillHint: 'Click a bar to drill into that day by hour.', emptyRange: 'No usage for this range and filters',
  chartLabel: title => `${title}. Use the arrow keys to read each period and Enter to drill in.`,
  other: 'Other', unknownModel: 'Unknown model', unknownProject: 'Unknown project',
  clickToFilter: 'Click a row to filter',
  perMillion: price => `${price}/M`, noPrice: 'No price', cacheShort: share => `cache ${share}`,
  sessionCount: count => `${count} sessions`, lastActive: date => `last ${date}`, noUsage: 'No usage',
  metricNote: 'Total tokens = input + output + cached, matching the TUI. Reasoning tokens are not added again. Costs are estimated from public API prices and may differ from subscription spending.',
};

const integer = new Intl.NumberFormat('en-US', { maximumFractionDigits: 0 });
const money = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 2 });
const monthDay = new Intl.DateTimeFormat('en-US', { month: 'numeric', day: 'numeric' });
const dayLong = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', weekday: 'short' });
const monthLong = new Intl.DateTimeFormat('en-US', { year: 'numeric', month: 'short' });
const clock = new Intl.DateTimeFormat('en-US', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const fmt = {
  int: value => integer.format(value),
  cost: value => money.format(value),
  compact,
  percent: (value, digits = 1) => `${(value * 100).toFixed(digits)}%`,
  md: key => monthDay.format(noon(key)),
  day: key => dayLong.format(noon(key)),
  month: key => monthLong.format(noon(`${key}-01`)),
  clock: date => clock.format(date),
};

const DIMENSIONS = ['tool', 'model', 'project'];
const RANGES = ['today', '7', '30', '90', 'all'];
const RANGE_LABELS = { today: 'today', 7: 'last7', 30: 'last30', 90: 'last90', all: 'all' };
const METRICS = ['cost', 'tokens'];
const GRANULARITIES = ['hour', 'day', 'week', 'month'];
const PLOT_HEIGHT = 240;
// Narrowest bar slot in px; charts with more buckets scroll sideways.
const MIN_SLOT = 4;
const SVG_NS = 'http://www.w3.org/2000/svg';

const elements = Object.fromEntries(['status', 'refresh', 'filters', 'view']
  .map(id => [id, document.getElementById(id)]));
const state = defaultState();
let snapshot = null;
let loading = false;
let loadError = null;
let openMenu = null;
let menuQuery = '';
// Color slots per dimension for the selected time range; see colorSlots.
let slots = {};

// DOM helpers. Text always goes through text nodes, never HTML parsing.
function setProps(element, props) {
  for (const [key, value] of Object.entries(props ?? {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key.startsWith('on')) element.addEventListener(key.slice(2), value);
    else if (key === 'value') element.value = value;
    else element.setAttribute(key, value === true ? '' : String(value));
  }
}

function h(tag, props, ...children) {
  const element = document.createElement(tag);
  setProps(element, props);
  element.append(...children.flat(Infinity).filter(child => child !== null && child !== undefined && child !== false));
  return element;
}

function svg(tag, props, ...children) {
  const element = document.createElementNS(SVG_NS, tag);
  setProps(element, props);
  element.append(...children.flat(Infinity).filter(Boolean));
  return element;
}

function icon(paths, size = 12) {
  return svg('svg', { class: 'icon', viewBox: '0 0 16 16', width: size, height: size, 'aria-hidden': 'true' }, paths.map(d => svg('path', { d })));
}

const closeIcon = () => icon(['M5 5l6 6M11 5l-6 6']);
const chevronIcon = () => icon(['M4.5 6.5 8 10l3.5-3.5']);

function noon(key) {
  return new Date(`${key}T12:00:00`);
}

function compact(value) {
  const trim = (number, digits) => String(Number(number.toFixed(digits)));
  if (value >= 1e9) return `${trim(value / 1e9, 2)}B`;
  if (value >= 1e6) return `${trim(value / 1e6, 1)}M`;
  if (value >= 1e3) return `${trim(value / 1e3, 1)}K`;
  return trim(value, value < 10 ? 2 : 0);
}

function defaultState() {
  return {
    range: '30', day: null, metric: 'cost', group: 'model', granularity: null,
    filters: { tool: new Set(), model: new Set(), project: new Set() },
  };
}

function update(patch) {
  Object.assign(state, patch);
  render();
}

function toggleFilter(key, value) {
  const next = new Set(state.filters[key]);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  update({ filters: { ...state.filters, [key]: next } });
}

async function load() {
  loading = true;
  loadError = null;
  elements.refresh.disabled = true;
  renderStatus();
  try {
    const response = await fetch('/api/usage', { cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    snapshot = prepare(await response.json());
  } catch (error) {
    loadError = error.message;
  } finally {
    loading = false;
    elements.refresh.disabled = false;
    render();
  }
}

function prepare(data) {
  const projects = [...new Set(data.rows.map(row => row.project))];
  return {
    rows: data.rows,
    sessionCount: new Set(data.rows.map(row => `${row.tool}:${row.session}`)).size,
    labels: projectLabels(projects),
    tools: new Set(data.rows.map(row => row.tool)).size,
    firstDate: data.rows.reduce((first, row) => (!first || row.date < first ? row.date : first), ''),
    scannedAt: new Date(data.scannedAt),
  };
}

// The last two path segments name a project unless another project shares
// them; then more of the path is shown.
function projectLabels(ids) {
  const labels = new Map();
  const segments = id => id.split(/[\\/]/).filter(Boolean);
  for (const depth of [2, 3, Infinity]) {
    const pending = ids.filter(id => !labels.has(id));
    const name = id => (depth === Infinity ? id : segments(id).slice(-depth).join('/') || id);
    const counts = new Map();
    for (const id of pending) counts.set(name(id), (counts.get(name(id)) ?? 0) + 1);
    for (const id of pending) if (depth === Infinity || counts.get(name(id)) === 1) labels.set(id, name(id));
  }
  return labels;
}

function label(key, value) {
  if (value === OTHER) return labels.other;
  if (key === 'model' && value === 'Unknown model') return labels.unknownModel;
  if (key !== 'project') return value;
  return snapshot.labels.get(value) || labels.unknownProject;
}

function seriesColor(dimension, value) {
  const slot = slots[dimension].get(value);
  return slot === undefined ? 'var(--other)' : `var(--series-${slot})`;
}

const amount = totals => (state.metric === 'cost' ? totals.cost : totalTokens(totals));
const formatAmount = value => (state.metric === 'cost' ? fmt.cost(value) : fmt.compact(value));
const formatAxis = value => (state.metric === 'cost' ? `$${fmt.compact(value)}` : fmt.compact(value));

function compute() {
  const now = new Date();
  const range = selectRange(state.range, state.day, now, snapshot.firstDate);
  const granularities = granularityOptions(range.days);
  const granularity = granularities.includes(state.granularity) ? state.granularity : granularities[0];
  const scoped = snapshot.rows.filter(row => inRange(row, range));
  const rows = scoped.filter(row => matchesFilters(row, state.filters));
  const prev = range.prev
    ? sumRows(snapshot.rows.filter(row => inRange(row, range.prev) && matchesFilters(row, state.filters)))
    : null;
  return { now, range, granularity, granularities, keys: bucketKeys(range, granularity, now), scoped, rows, totals: sumRows(rows), prev };
}

function rangeLabel(range) {
  if (state.day) return fmt.day(state.day);
  if (state.range === 'today') return labels.today;
  if (state.range === 'all') return labels.allDays(fmt.int(range.days));
  return labels.lastDays(range.days);
}

function comparisonLabel() {
  if (state.day) return labels.vsPreviousDay;
  if (state.range === 'today') return labels.vsYesterday;
  return labels.vsPrevious(state.range);
}

function delta(current, previous, comparison) {
  if (previous === null || previous === undefined) return labels.noComparison;
  if (previous === 0) return `${current > 0 ? labels.previousZero : labels.unchanged} · ${comparison}`;
  const change = (current - previous) / previous;
  return `${change >= 0 ? '▲' : '▼'} ${fmt.percent(Math.abs(change))} · ${comparison}`;
}

function points(current, previous, comparison) {
  const change = (current - previous) * 100;
  return `${change >= 0 ? '▲' : '▼'} ${Math.abs(change).toFixed(1)} ${labels.points} · ${comparison}`;
}

function periodLabel(key, granularity, short, range) {
  if (granularity === 'hour') {
    const [date, hour] = key.split('T');
    if (short) return range.days === 1 ? `${hour}:00` : hour === '00' ? fmt.md(date) : `${fmt.md(date)} ${hour}:00`;
    return `${fmt.day(date)} ${hour}:00–${String(Number(hour) + 1).padStart(2, '0')}:00`;
  }
  if (granularity === 'day') return short ? fmt.md(key) : fmt.day(key);
  if (granularity === 'week') return short ? fmt.md(key) : `${fmt.md(key)} – ${fmt.md(shiftDate(key, 6))}`;
  return fmt.month(key);
}

function segmented(name, items, onPick) {
  return h('div', { class: 'seg', role: 'group', 'aria-label': name },
    items.map(item => h('button', {
      type: 'button', 'aria-pressed': String(item.pressed), 'data-focus': `${name}:${item.value}`,
      onclick: () => onPick(item.value),
    }, item.label)));
}

function sectionHead(number, title, sub, ...actions) {
  return h('div', { class: 'section-head' },
    h('div', { class: 'section-title' },
      number ? h('span', { class: 'section-no' }, number) : null,
      h('h2', {}, title),
      sub ? h('span', { class: 'small muted' }, sub) : null),
    h('div', { class: 'section-actions' }, actions));
}

function swatch(color) {
  return h('span', { class: 'swatch', style: `background:${color}` });
}

function render() {
  const active = document.activeElement;
  const focus = active?.dataset?.focus;
  const selection = focus && typeof active.selectionStart === 'number' ? [active.selectionStart, active.selectionEnd] : null;
  renderStatus();
  if (!snapshot) {
    elements.filters.replaceChildren();
    elements.view.replaceChildren(h('p', { class: 'empty' }, loadError ? labels.loadError(loadError) : labels.loading));
    return;
  }
  const view = compute();
  slots = Object.fromEntries(DIMENSIONS.map(key => [key, colorSlots(view.scoped, key)]));
  renderFilters(view);
  const content = !snapshot.rows.length
    ? [h('p', { class: 'empty' }, labels.noData)]
    : overview(view);
  elements.view.replaceChildren(...content.filter(Boolean));
  if (!focus) return;
  const target = document.querySelector(`[data-focus="${CSS.escape(focus)}"]`);
  target?.focus({ preventScroll: true });
  if (selection && target?.setSelectionRange) target.setSelectionRange(...selection);
}

function renderStatus() {
  if (loading) elements.status.textContent = snapshot ? labels.scanning : labels.loading;
  else if (loadError) elements.status.textContent = labels.loadError(loadError);
  else if (snapshot) elements.status.textContent = labels.scanned(fmt.clock(snapshot.scannedAt), fmt.int(snapshot.tools), fmt.int(snapshot.sessionCount));
  else elements.status.textContent = '';
}

function renderFilters(view) {
  const chips = DIMENSIONS.flatMap(key => [...state.filters[key]].map(value => h('button', {
    type: 'button', class: 'chip', 'aria-label': labels.removeFilter(label(key, value)), 'data-focus': `chip:${key}:${value}`,
    title: key === 'project' ? value : null, onclick: () => toggleFilter(key, value),
  }, h('span', {}, `${labels[key]}: ${label(key, value)}`), closeIcon())));
  elements.filters.replaceChildren(...[
    segmented(labels.range, RANGES.map(range => ({ value: range, label: labels[RANGE_LABELS[range]], pressed: !state.day && state.range === range })),
      range => update({ range, day: null })),
    state.day ? h('button', { type: 'button', class: 'chip', 'aria-label': labels.clearDrill, 'data-focus': 'drill', onclick: () => update({ day: null }) },
      h('span', {}, `${fmt.day(state.day)} · ${labels.hourly}`), closeIcon()) : null,
    h('span', { class: 'divider hide-sm', 'aria-hidden': 'true' }),
    ...DIMENSIONS.map(key => filterMenu(key, view)),
    ...chips,
    chips.length || state.day
      ? h('button', { type: 'button', class: 'link', 'data-focus': 'clear', onclick: () => update({ filters: defaultState().filters, day: null }) }, labels.clearFilters)
      : null,
    h('div', { class: 'metric-group' },
      h('span', { class: 'small muted' }, labels.metric),
      segmented(labels.metric, METRICS.map(metric => ({ value: metric, label: labels[metric], pressed: state.metric === metric })), metric => update({ metric }))),
  ].filter(Boolean));
}

function filterMenu(key, view) {
  const open = openMenu === key;
  const selected = state.filters[key];
  const wrap = h('div', { class: 'menu-wrap', onclick: event => event.stopPropagation() },
    h('button', {
      type: 'button', class: 'btn', 'aria-haspopup': 'true', 'aria-expanded': String(open), 'data-focus': `menu:${key}`,
      onclick: () => {
        openMenu = open ? null : key;
        menuQuery = '';
        render();
        if (!open) document.querySelector(`[data-focus="menu-search:${key}"]`)?.focus();
      },
    }, labels[key], selected.size ? h('span', { class: 'count' }, String(selected.size)) : null, chevronIcon()));
  if (!open) return wrap;
  const totals = groupRows(view.scoped, row => row[key]);
  for (const value of selected) if (!totals.has(value)) totals.set(value, sumRows([]));
  const query = menuQuery.trim().toLocaleLowerCase();
  const options = [...totals]
    .filter(([value]) => !query || `${label(key, value)} ${value}`.toLocaleLowerCase().includes(query))
    .sort((a, b) => amount(b[1]) - amount(a[1]));
  wrap.append(h('div', { class: 'menu', role: 'group', 'aria-label': labels[key] },
    h('input', {
      type: 'search', class: 'menu-search', value: menuQuery, placeholder: labels.searchIn(labels[key]), 'aria-label': labels.searchIn(labels[key]),
      'data-focus': `menu-search:${key}`, oninput: event => { menuQuery = event.target.value; render(); },
    }),
    options.length
      ? h('div', { class: 'menu-list' }, options.map(([value, item]) => h('button', {
        type: 'button', class: 'menu-item', 'aria-pressed': String(selected.has(value)), 'data-focus': `option:${key}:${value}`,
        title: key === 'project' ? value : null, onclick: () => toggleFilter(key, value),
      }, h('span', { class: 'check', 'aria-hidden': 'true' }, selected.has(value) ? '✓' : ''),
      h('span', { class: 'ellipsis grow' }, label(key, value)),
      h('span', { class: 'num small muted' }, amount(item) ? formatAmount(amount(item)) : '—'))))
      : h('p', { class: 'menu-empty' }, labels.noMatches),
    selected.size
      ? h('button', { type: 'button', class: 'link menu-clear', onclick: () => update({ filters: { ...state.filters, [key]: new Set() } }) }, labels.clearSelection)
      : null));
  return wrap;
}

function overview(view) {
  return [
    kpiSection(view),
    qualityNote(view),
    trendSection(view),
    h('section', { class: 'three', 'aria-label': labels.rankings },
      [['model', '02'], ['tool', '03'], ['project', '04']].map(([key, number]) => rankCard(view, key, number))),
    h('p', { class: 'note' }, labels.metricNote),
  ];
}

function sparkline(values) {
  const max = Math.max(0, ...values);
  const coordinates = values.length > 1 && max > 0
    ? values.map((value, index) => `${((index / (values.length - 1)) * 200).toFixed(1)},${(46 - (value / max) * 40).toFixed(1)}`)
    : ['0,46', '200,46'];
  const line = `M${coordinates.join(' L')}`;
  return svg('svg', { class: 'spark', viewBox: '0 0 200 48', preserveAspectRatio: 'none', 'aria-hidden': 'true' },
    svg('path', { class: 'spark-area', d: `${line} L200,48 L0,48 Z` }),
    svg('path', { class: 'spark-line', d: line }));
}

// Token mix stays grayscale (cached, input, output) so hue always names an entity.
function composition(totals, className) {
  const parts = [totals.cachedTokens, totals.inputTokens, totals.outputTokens];
  const sum = parts.reduce((total, value) => total + value, 0);
  return h('span', { class: className, 'aria-hidden': 'true' },
    parts.map((value, index) => h('span', { style: `flex:${sum ? value / sum : 1} 1 0;background:var(--comp-${index})` })));
}

function compositionLegend(totals) {
  const parts = [['cached', totals.cachedTokens], ['input', totals.inputTokens], ['output', totals.outputTokens]];
  const sum = parts.reduce((total, [, value]) => total + value, 0);
  return h('div', { class: 'legend' }, parts.map(([key, value], index) => h('span', { class: 'legend-item' },
    swatch(`var(--comp-${index})`), labels[key], ' ',
    h('span', { class: 'num' }, sum ? fmt.percent(value / sum, value / sum < 0.01 ? 2 : 1) : '0%'))));
}

function kpiSection(view) {
  const { totals, prev, range, keys } = view;
  const comparison = comparisonLabel();
  const index = new Map(keys.map((key, position) => [key, position]));
  const costs = new Array(keys.length).fill(0);
  for (const row of view.rows) {
    const position = index.get(periodKey(row.date, row.hour, view.granularity));
    if (position !== undefined) costs[position] += row.cost;
  }
  const average = range.days === 1
    ? (state.day ? labels.wholeDay : labels.asOf(fmt.clock(view.now)))
    : labels.dailyAverage(fmt.cost(totals.cost / range.days), fmt.int(totals.dates.size), fmt.int(range.days));
  const card = (title, value, change, ...notes) => h('div', { class: 'card kpi' },
    h('h2', { class: 'kpi-label' }, title), h('p', { class: 'kpi-value' }, value), h('p', { class: 'delta' }, change), notes);
  return h('section', { class: 'kpis', 'aria-label': labels.summary },
    h('div', { class: 'card kpi hero' },
      h('div', { class: 'kpi-head' }, h('h2', { class: 'kpi-label' }, labels.estimatedCost), h('span', { class: 'small muted' }, rangeLabel(range))),
      h('div', { class: 'hero-body' },
        h('div', {}, h('p', { class: 'hero-value' }, fmt.cost(totals.cost)), h('p', { class: 'delta' }, delta(totals.cost, prev?.cost, comparison))),
        sparkline(costs)),
      h('p', { class: 'small muted' }, average)),
    card(labels.totalTokens, fmt.compact(totalTokens(totals)), delta(totalTokens(totals), prev && totalTokens(prev), comparison),
      composition(totals, 'comp'), compositionLegend(totals)),
    card(labels.sessions, fmt.int(totals.sessions.size), delta(totals.sessions.size, prev?.sessions.size, comparison),
      h('p', { class: 'small muted' }, labels.replies(fmt.int(totals.messages)))),
    card(labels.cacheShare, fmt.percent(cacheShare(totals)), prev ? points(cacheShare(totals), cacheShare(prev), comparison) : labels.noComparison,
      h('p', { class: 'small muted' }, labels.cacheSub)));
}

function qualityNote(view) {
  const unpriced = [...groupRows(view.rows, row => row.model)].filter(([, totals]) => totals.cost === 0 && totalTokens(totals) > 0);
  if (!unpriced.length) return null;
  const tokens = unpriced.reduce((sum, [, totals]) => sum + totalTokens(totals), 0);
  const names = unpriced.map(([model]) => label('model', model)).join(', ');
  return h('div', { class: 'quality', role: 'note' },
    svg('svg', { viewBox: '0 0 16 16', width: 16, height: 16, 'aria-hidden': 'true' },
      svg('path', { d: 'M8 1.5 15 14H1z', style: 'fill:var(--warn)' }),
      svg('path', { d: 'M8 6v4', style: 'stroke:#1b1a18;stroke-width:1.6;stroke-linecap:round' }),
      svg('circle', { cx: 8, cy: 12, r: 0.9, style: 'fill:#1b1a18' })),
    h('p', {}, h('strong', {}, labels.costMayBeLow), ` · ${labels.unpriced(names, fmt.compact(tokens))}`));
}

function granularityControl(view) {
  if (view.granularities.length < 2) return null;
  return [
    h('span', { class: 'small muted' }, labels.interval),
    segmented(labels.interval, GRANULARITIES.filter(item => view.granularities.includes(item))
      .map(item => ({ value: item, label: labels.intervals[item], pressed: item === view.granularity })), granularity => update({ granularity })),
  ];
}

function stackBar(series, index, total, height, dimension) {
  const parts = series.filter(item => item.values[index] > 0);
  const gap = parts.length > 1 && height >= parts.length * 6 ? 2 : 0;
  const available = Math.max(0, height - gap * (parts.length - 1));
  const radius = height >= 4 ? 4 : 1;
  return h('div', { class: 'bar', 'data-index': index },
    h('div', { class: 'stack', style: `height:${height}px;gap:${gap}px` },
      parts.map((item, position) => h('div', {
        class: 'segment',
        style: `height:${(item.values[index] / total) * available}px;background:${seriesColor(dimension, item.id)}${position === parts.length - 1 ? `;border-radius:${radius}px ${radius}px 0 0` : ''}`,
      }))));
}

function trendSection(view) {
  const series = stackedSeries(view.rows, view.keys, view.granularity, state.group, state.metric, slots[state.group]);
  const title = labels.trendTitle(labels.every[view.granularity], state.metric);
  return h('section', { class: 'card trend' },
    sectionHead('01', title, labels.stackedBy(labels[state.group], rangeLabel(view.range)),
      h('span', { class: 'small muted' }, labels.group),
      segmented(labels.group, ['model', 'tool', 'project'].map(key => ({ value: key, label: labels[key], pressed: state.group === key })), group => update({ group })),
      granularityControl(view)),
    h('div', { class: 'legend' }, series.map(item => h('span', { class: 'legend-item' }, swatch(seriesColor(state.group, item.id)), label(state.group, item.id)))),
    trendChart(series, view, title),
    view.granularity === 'day' ? h('p', { class: 'hint' }, labels.drillHint) : null);
}

function trendChart(series, view, title) {
  const { keys, granularity, range } = view;
  const totals = keys.map((_, index) => series.reduce((sum, item) => sum + item.values[index], 0));
  const max = totals.reduce((maximum, total) => Math.max(maximum, total), 0);
  const step = niceStep(max);
  const drillable = granularity === 'day';
  const bars = h('div', { class: drillable ? 'bars drillable' : 'bars' },
    keys.map((_, index) => stackBar(series, index, totals[index], (totals[index] / (step * 4)) * PLOT_HEIGHT, state.group)));
  const tip = h('div', { class: 'tip', hidden: true });
  const live = h('p', { class: 'sr-only', 'aria-live': 'polite' });
  const ticks = [0, 1, 2, 3, 4];
  const plot = h('div', { class: 'plot', tabindex: '0', 'aria-label': labels.chartLabel(title) },
    ticks.map(tick => h('div', { class: tick ? 'tick' : 'tick base', style: `bottom:${tick * 25}%` })),
    bars, tip, max ? null : h('p', { class: 'plot-empty' }, labels.emptyRange));
  // The value axis stays outside the scrolling area so wide charts keep it in view.
  const valueAxis = h('div', { class: 'y-axis', 'aria-hidden': 'true' },
    ticks.map(tick => h('span', { class: 'num', style: `bottom:${tick * 25}%` }, formatAxis(step * tick))));
  let active = null;
  const show = index => {
    if (active !== null) bars.children[active].classList.remove('active');
    active = index;
    bars.children[index].classList.add('active');
    const heading = periodLabel(keys[index], granularity, false, range);
    const parts = series.filter(item => item.values[index] > 0).sort((a, b) => b.values[index] - a.values[index]);
    tip.replaceChildren(
      h('p', { class: 'small muted' }, heading),
      h('p', { class: 'tip-value num' }, formatAmount(totals[index])),
      h('div', { class: 'tip-rows' }, parts.map(item => h('div', { class: 'tip-row' },
        h('span', { class: 'tip-key', style: `background:${seriesColor(state.group, item.id)}` }),
        h('span', { class: 'ellipsis' }, label(state.group, item.id)),
        h('span', { class: 'num' }, formatAmount(item.values[index]))))));
    const position = (index + 0.5) / keys.length;
    tip.style.left = `${position * 100}%`;
    tip.style.transform = `translateX(${position < 0.2 ? -12 : position > 0.8 ? -88 : -50}%)`;
    tip.hidden = false;
    live.textContent = `${heading}: ${formatAmount(totals[index])}`;
  };
  const hide = () => {
    if (active !== null) bars.children[active].classList.remove('active');
    active = null;
    tip.hidden = true;
  };
  const drill = index => {
    if (drillable) update({ day: keys[index] });
  };
  const indexOf = event => event.target.closest('.bar')?.dataset.index;
  bars.addEventListener('pointermove', event => {
    const index = indexOf(event);
    if (index !== undefined && Number(index) !== active) show(Number(index));
  });
  bars.addEventListener('pointerleave', hide);
  bars.addEventListener('click', event => {
    const index = indexOf(event);
    if (index !== undefined) drill(Number(index));
  });
  plot.addEventListener('focus', () => show(active ?? Math.max(0, totals.findLastIndex(total => total > 0))));
  plot.addEventListener('blur', hide);
  plot.addEventListener('keydown', event => {
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault();
      show(Math.min(keys.length - 1, Math.max(0, (active ?? 0) + (event.key === 'ArrowRight' ? 1 : -1))));
    } else if (event.key === 'Enter' && active !== null) {
      drill(active);
    }
  });
  // Labels sit about 90px apart on a desktop-wide chart; multi-day hourly charts label each midnight.
  const slot = Math.max(MIN_SLOT, 1200 / keys.length);
  let every = granularity === 'hour' && range.days === 1 ? 3 : Math.max(1, Math.ceil(90 / slot));
  if (granularity === 'hour' && range.days > 1) every = 24 * Math.ceil(every / 24);
  const axis = h('div', { class: 'x-axis', 'aria-hidden': 'true' }, keys.map((key, index) => (index % every ? null
    : h('span', { class: 'num', style: `left:${((index + 0.5) / keys.length) * 100}%` }, periodLabel(key, granularity, true, range)))));
  const scroller = h('div', { class: 'chart-scroll' },
    h('div', { class: 'chart-inner', style: `min-width:${Math.max(544, keys.length * MIN_SLOT)}px` }, plot, axis, live));
  // A chart wider than the page opens at the latest period.
  if (keys.length * MIN_SLOT > 1200) requestAnimationFrame(() => { scroller.scrollLeft = scroller.scrollWidth; });
  return h('div', { class: 'chart' }, valueAxis, scroller);
}

function rankSub(key, totals) {
  const cache = labels.cacheShort(fmt.percent(cacheShare(totals), 0));
  if (key === 'model') {
    const tokens = totalTokens(totals);
    const price = totals.cost > 0 && tokens > 0 ? labels.perMillion(fmt.cost((totals.cost / tokens) * 1e6)) : labels.noPrice;
    return `${price} · ${cache}`;
  }
  if (key === 'tool') return `${labels.sessionCount(fmt.int(totals.sessions.size))} · ${cache}`;
  return [
    labels.sessionCount(fmt.int(totals.sessions.size)),
    labels.lastActive(fmt.md([...totals.dates].sort().at(-1))),
  ].filter(Boolean).join(' · ');
}

function rankCard(view, key, number) {
  const groups = [...groupRows(view.rows, row => row[key])].sort((a, b) => amount(b[1]) - amount(a[1]));
  const max = groups.length ? amount(groups[0][1]) : 0;
  return h('div', { class: 'card rank' },
    sectionHead(number, labels[key], null, h('span', { class: 'small muted' }, labels.clickToFilter)),
    groups.slice(0, 6).map(([value, totals]) => h('button', {
      type: 'button', class: 'rank-row', 'aria-pressed': String(state.filters[key].has(value)), 'data-focus': `rank:${key}:${value}`,
      title: key === 'project' ? value : label(key, value), onclick: () => toggleFilter(key, value),
    },
    h('span', { class: 'rank-name' }, key === state.group ? swatch(seriesColor(key, value)) : null, h('span', { class: 'ellipsis' }, label(key, value))),
    h('span', { class: 'rank-value num' }, formatAmount(amount(totals))),
    h('span', { class: 'rank-meter' },
      h('span', { class: 'track', 'aria-hidden': 'true' }, h('span', { class: 'fill', style: `width:${max > 0 ? (amount(totals) / max) * 100 : 0}%` })),
      h('span', { class: 'rank-sub num' }, rankSub(key, totals))))),
    groups.length ? null : h('p', { class: 'menu-empty' }, labels.noUsage));
}

elements.refresh.addEventListener('click', () => load());
document.addEventListener('click', () => {
  if (!openMenu) return;
  openMenu = null;
  render();
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || !openMenu) return;
  const key = openMenu;
  openMenu = null;
  render();
  document.querySelector(`[data-focus="menu:${key}"]`)?.focus();
});
load();
