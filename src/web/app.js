import {
  OTHER, SESSION_PAGE_SIZE, rowTokens, localDateKey, shiftDate, periodKey, selectRange, inRange, matchesFilters,
  granularityOptions, bucketKeys, sumRows, groupRows, totalTokens, cacheShare, colorSlots, stackedSeries,
  weekdayHourGrid, niceStep, topTenthShare, median, csv,
} from './data.mjs';
import { messages } from './locales.mjs';

const DIMENSIONS = ['tool', 'model', 'project'];
const VIEWS = ['overview', 'sessions', 'breakdown'];
const RANGES = ['today', '7', '30', '90', 'all'];
const RANGE_LABELS = { today: 'today', 7: 'last7', 30: 'last30', 90: 'last90', all: 'all' };
const METRICS = ['cost', 'tokens'];
const SORTS = ['recent', 'cost', 'tokens', 'duration'];
const BREAKDOWNS = ['period', 'project', 'model', 'tool'];
const GRANULARITIES = ['hour', 'day', 'week', 'month'];
const PLOT_HEIGHT = 240;
const TIMELINE_HEIGHT = 88;
// Narrowest bar slot in px; charts with more buckets scroll sideways.
const MIN_SLOT = 4;
const LANGUAGE_KEY = 'splitrail-web-language';
const SVG_NS = 'http://www.w3.org/2000/svg';
const POLL_MS = 60_000;


const elements = Object.fromEntries(['status', 'refresh', 'language', 'tabs-bar', 'tabs', 'filters', 'view']
  .map(id => [id, document.getElementById(id)]));
let language = initialLanguage();
let fmt;
let state = readHash();
let snapshot = null;
let loading = false;
let loadError = null;
let openMenu = null;
let menuQuery = '';
let sessionLimit = SESSION_PAGE_SIZE;
// Color slots per dimension for the selected time range; see colorSlots.
let slots = {};

function t(key) {
  return messages[language][key];
}

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

function initialLanguage() {
  try {
    const saved = localStorage.getItem(LANGUAGE_KEY);
    if (Object.hasOwn(messages, saved)) return saved;
  } catch { /* The default language also works without storage. */ }
  return 'en';
}

function setLanguage(next, persist) {
  language = next;
  const locale = language === 'zh' ? 'zh-CN' : language === 'zh-TW' ? 'zh-TW' : 'en-US';
  const integer = new Intl.NumberFormat(locale, { maximumFractionDigits: 0 });
  const money = new Intl.NumberFormat('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 2 });
  const monthDay = new Intl.DateTimeFormat(locale, { month: 'numeric', day: 'numeric' });
  const dayLong = new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric', weekday: 'short' });
  const monthLong = new Intl.DateTimeFormat(locale, { year: 'numeric', month: 'short' });
  const clock = new Intl.DateTimeFormat(locale, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
  const weekday = new Intl.DateTimeFormat(locale, { weekday: 'short' });
  fmt = {
    int: value => integer.format(value),
    cost: value => money.format(value),
    compact,
    percent: (value, digits = 1) => `${(value * 100).toFixed(digits)}%`,
    md: key => monthDay.format(noon(key)),
    day: key => dayLong.format(noon(key)),
    month: key => monthLong.format(noon(`${key}-01`)),
    clock: date => clock.format(date),
    duration: minutes => (minutes < 60 ? t('minutes')(minutes) : t('hoursMinutes')(Math.floor(minutes / 60), minutes % 60)),
    // 2024-01-01 was a Monday.
    weekdays: Array.from({ length: 7 }, (_, index) => weekday.format(new Date(2024, 0, 1 + index))),
  };
  document.documentElement.lang = language === 'zh' ? 'zh-CN' : language === 'zh-TW' ? 'zh-TW' : 'en';
  elements.language.value = language;
  elements.language.setAttribute('aria-label', t('language'));
  elements['tabs-bar'].setAttribute('aria-label', t('views'));
  for (const element of document.querySelectorAll('[data-i18n]')) element.textContent = t(element.dataset.i18n);
  if (persist) {
    try { localStorage.setItem(LANGUAGE_KEY, language); } catch { /* The page works without storage. */ }
  }
  render();
}

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

// The URL hash holds the view state, so reloads and bookmarks restore it.
function defaultState() {
  return {
    view: 'overview', range: '30', day: null, metric: 'cost', group: 'model', granularity: null,
    sort: 'recent', q: '', session: null, dim: 'project',
    filters: { tool: new Set(), model: new Set(), project: new Set() },
  };
}

function readHash() {
  const params = new URLSearchParams(location.hash.slice(1));
  const next = defaultState();
  const choose = (key, allowed) => {
    if (allowed.includes(params.get(key))) next[key] = params.get(key);
  };
  choose('view', VIEWS);
  choose('range', RANGES);
  choose('metric', METRICS);
  choose('group', DIMENSIONS);
  choose('granularity', GRANULARITIES);
  choose('sort', SORTS);
  choose('dim', BREAKDOWNS);
  const day = params.get('day');
  if (/^\d{4}-\d{2}-\d{2}$/.test(day ?? '') && localDateKey(noon(day)) === day) next.day = day;
  next.q = params.get('q') ?? '';
  next.session = params.get('session');
  for (const key of DIMENSIONS) next.filters[key] = new Set(params.getAll(key));
  return next;
}

function writeHash() {
  const params = new URLSearchParams();
  const defaults = defaultState();
  for (const key of ['view', 'range', 'day', 'metric', 'group', 'granularity', 'sort', 'q', 'session', 'dim']) {
    if (state[key] && state[key] !== defaults[key]) params.set(key, state[key]);
  }
  for (const key of DIMENSIONS) for (const value of state.filters[key]) params.append(key, value);
  const hash = params.toString();
  if (hash !== location.hash.slice(1)) history.replaceState(null, '', hash ? `#${hash}` : `${location.pathname}${location.search}`);
}

function update(patch) {
  if (Object.keys(patch).some(key => key !== 'session')) sessionLimit = SESSION_PAGE_SIZE;
  Object.assign(state, patch);
  writeHash();
  render();
}

function toggleFilter(key, value) {
  const next = new Set(state.filters[key]);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  update({ filters: { ...state.filters, [key]: next }, session: null });
}

async function load(refresh = false, startup = false) {
  loading = true;
  loadError = null;
  elements.refresh.disabled = true;
  renderStatus();
  try {
    const response = await fetch(`/api/usage${refresh ? '?refresh=true' : startup ? '?startup=true' : ''}`, { cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    snapshot = prepare(await response.json());
    if (!refresh && !startup && response.headers.get('x-splitrail-stale') === 'true') {
      render();
      return await load(false, true);
    }
  } catch (error) {
    loadError = error.message;
  } finally {
    loading = false;
    elements.refresh.disabled = false;
    render();
  }
}

function schedulePoll() {
  setTimeout(async () => {
    if (!document.hidden && !loading) await load();
    schedulePoll();
  }, POLL_MS);
}
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && !loading) load();
});

function prepare(data) {
  const projects = new Map(data.projects.map(project => [project.id, project.paths]));
  return {
    rows: data.rows,
    sessions: new Map(data.sessions.map(session => [session.id, { ...session, start: new Date(session.start), end: new Date(session.end) }])),
    projects,
    labels: projectLabels([...projects.keys()], projects),
    tools: new Set(data.rows.map(row => row.tool)).size,
    firstDate: data.rows.reduce((first, row) => (!first || row.date < first ? row.date : first), ''),
    scannedAt: new Date(data.scannedAt),
  };
}

// The last two path segments name a project unless another project shares
// them; then more of the path is shown.
function projectLabels(ids, projects) {
  const labels = new Map();
  const segments = id => id.split(/[\\/]/).filter(Boolean);
  for (const depth of [2, 3, Infinity]) {
    const pending = ids.filter(id => projects.has(id) && !labels.has(id));
    const name = id => (depth === Infinity ? id : segments(id).slice(-depth).join('/') || id);
    const counts = new Map();
    for (const id of pending) counts.set(name(id), (counts.get(name(id)) ?? 0) + 1);
    for (const id of pending) if (depth === Infinity || counts.get(name(id)) === 1) labels.set(id, name(id));
  }
  return labels;
}

function label(key, value) {
  if (value === OTHER) return t('other');
  if (key === 'model' && value === 'Unknown model') return t('unknownModel');
  if (key !== 'project') return value;
  return snapshot.labels.get(value) ?? (value ? `${t('unknownProject')} · ${value.slice(0, 6)}` : t('unknownProject'));
}

function projectTitle(value) {
  const paths = snapshot.projects.get(value);
  return paths ? [value, ...paths.filter(path => path !== value)].join('\n') : label('project', value);
}

function seriesColor(dimension, value) {
  const slot = slots[dimension].get(value);
  return slot === undefined ? 'var(--other)' : `var(--series-${slot})`;
}

function sessionName(meta) {
  return meta.name?.trim() || t('untitled');
}

function sessionMinutes(meta) {
  return Math.max(1, Math.round((meta.end - meta.start) / 60000));
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
  if (state.range === 'today') return t('today');
  if (state.range === 'all') return t('allDays')(fmt.int(range.days));
  return t('lastDays')(range.days);
}

function comparisonLabel() {
  if (state.day) return t('vsPreviousDay');
  if (state.range === 'today') return t('vsYesterday');
  return t('vsPrevious')(state.range);
}

function delta(current, previous, comparison) {
  if (previous === null || previous === undefined) return t('noComparison');
  if (previous === 0) return `${current > 0 ? t('previousZero') : t('unchanged')} · ${comparison}`;
  const change = (current - previous) / previous;
  return `${change >= 0 ? '▲' : '▼'} ${fmt.percent(Math.abs(change))} · ${comparison}`;
}

function points(current, previous, comparison) {
  const change = (current - previous) * 100;
  return `${change >= 0 ? '▲' : '▼'} ${Math.abs(change).toFixed(1)} ${t('points')} · ${comparison}`;
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
  document.title = t('title');
  renderStatus();
  if (!snapshot) {
    elements.tabs.replaceChildren();
    elements.filters.replaceChildren();
    elements.view.replaceChildren(h('p', { class: 'empty' }, loadError ? t('loadError')(loadError) : t('loading')));
    return;
  }
  const view = compute();
  slots = Object.fromEntries(DIMENSIONS.map(key => [key, colorSlots(view.scoped, key)]));
  renderTabs(view);
  renderFilters(view);
  const content = !snapshot.rows.length
    ? [h('p', { class: 'empty' }, t('noData'))]
    : state.view === 'sessions' ? sessionsView(view) : state.view === 'breakdown' ? breakdownView(view) : overview(view);
  elements.view.replaceChildren(...content.filter(Boolean));
  if (!focus) return;
  const target = document.querySelector(`[data-focus="${CSS.escape(focus)}"]`);
  target?.focus({ preventScroll: true });
  if (selection && target?.setSelectionRange) target.setSelectionRange(...selection);
}

function renderStatus() {
  if (loading) elements.status.textContent = snapshot ? t('scanning') : t('loading');
  else if (loadError) elements.status.textContent = t('loadError')(loadError);
  else if (snapshot) elements.status.textContent = t('scanned')(fmt.clock(snapshot.scannedAt), fmt.int(snapshot.tools), fmt.int(snapshot.sessions.size));
  else elements.status.textContent = '';
}

function renderTabs(view) {
  elements.tabs.replaceChildren(...VIEWS.map(id => h('button', {
    type: 'button', class: 'tab', 'aria-pressed': String(state.view === id), 'data-focus': `tab:${id}`,
    onclick: () => update({ view: id }),
  }, t(id), id === 'sessions' ? h('span', { class: 'tab-badge num' }, fmt.int(view.totals.sessions.size)) : null)));
}

function renderFilters(view) {
  const chips = DIMENSIONS.flatMap(key => [...state.filters[key]].map(value => h('button', {
    type: 'button', class: 'chip', 'aria-label': t('removeFilter')(label(key, value)), 'data-focus': `chip:${key}:${value}`,
    title: key === 'project' ? projectTitle(value) : null, onclick: () => toggleFilter(key, value),
  }, h('span', {}, `${t(key)}: ${label(key, value)}`), closeIcon())));
  elements.filters.replaceChildren(...[
    segmented(t('range'), RANGES.map(range => ({ value: range, label: t(RANGE_LABELS[range]), pressed: !state.day && state.range === range })),
      range => update({ range, day: null, session: null })),
    state.day ? h('button', { type: 'button', class: 'chip', 'aria-label': t('clearDrill'), 'data-focus': 'drill', onclick: () => update({ day: null }) },
      h('span', {}, `${fmt.day(state.day)} · ${t('hourly')}`), closeIcon()) : null,
    h('span', { class: 'divider hide-sm', 'aria-hidden': 'true' }),
    ...DIMENSIONS.map(key => filterMenu(key, view)),
    ...chips,
    chips.length || state.day
      ? h('button', { type: 'button', class: 'link', 'data-focus': 'clear', onclick: () => update({ filters: defaultState().filters, day: null, session: null }) }, t('clearFilters'))
      : null,
    h('div', { class: 'metric-group' },
      h('span', { class: 'small muted' }, t('metric')),
      segmented(t('metric'), METRICS.map(metric => ({ value: metric, label: t(metric), pressed: state.metric === metric })), metric => update({ metric }))),
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
    }, t(key), selected.size ? h('span', { class: 'count' }, String(selected.size)) : null, chevronIcon()));
  if (!open) return wrap;
  const totals = groupRows(view.scoped, row => row[key]);
  for (const value of selected) if (!totals.has(value)) totals.set(value, sumRows([]));
  const query = menuQuery.trim().toLocaleLowerCase();
  const options = [...totals]
    .filter(([value]) => !query || `${label(key, value)} ${value}`.toLocaleLowerCase().includes(query))
    .sort((a, b) => amount(b[1]) - amount(a[1]));
  wrap.append(h('div', { class: 'menu', role: 'group', 'aria-label': t(key) },
    h('input', {
      type: 'search', class: 'menu-search', value: menuQuery, placeholder: t('searchIn')(t(key)), 'aria-label': t('searchIn')(t(key)),
      'data-focus': `menu-search:${key}`, oninput: event => { menuQuery = event.target.value; render(); },
    }),
    options.length
      ? h('div', { class: 'menu-list' }, options.map(([value, item]) => h('button', {
        type: 'button', class: 'menu-item', 'aria-pressed': String(selected.has(value)), 'data-focus': `option:${key}:${value}`,
        title: key === 'project' ? projectTitle(value) : null, onclick: () => toggleFilter(key, value),
      }, h('span', { class: 'check', 'aria-hidden': 'true' }, selected.has(value) ? '✓' : ''),
      h('span', { class: 'ellipsis grow' }, label(key, value)),
      h('span', { class: 'num small muted' }, amount(item) ? formatAmount(amount(item)) : '—'))))
      : h('p', { class: 'menu-empty' }, t('noMatches')),
    selected.size
      ? h('button', { type: 'button', class: 'link menu-clear', onclick: () => update({ filters: { ...state.filters, [key]: new Set() } }) }, t('clearSelection'))
      : null));
  return wrap;
}

function overview(view) {
  return [
    kpiSection(view),
    qualityNote(view),
    trendSection(view),
    h('section', { class: 'three', 'aria-label': t('rankings') },
      [['model', '02'], ['tool', '03'], ['project', '04']].map(([key, number]) => rankCard(view, key, number))),
    h('section', { class: 'two' }, heatSection(view), topSessionsSection(view)),
    h('p', { class: 'note' }, t('metricNote')),
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
    swatch(`var(--comp-${index})`), t(key), ' ',
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
  const durations = [...totals.sessions].map(id => snapshot.sessions.get(id)).filter(Boolean).map(sessionMinutes);
  const average = range.days === 1
    ? (state.day ? t('wholeDay') : t('asOf')(fmt.clock(view.now)))
    : t('dailyAverage')(fmt.cost(totals.cost / range.days), fmt.int(totals.dates.size), fmt.int(range.days));
  const card = (title, value, change, ...notes) => h('div', { class: 'card kpi' },
    h('h2', { class: 'kpi-label' }, title), h('p', { class: 'kpi-value' }, value), h('p', { class: 'delta' }, change), notes);
  return h('section', { class: 'kpis', 'aria-label': t('summary') },
    h('div', { class: 'card kpi hero' },
      h('div', { class: 'kpi-head' }, h('h2', { class: 'kpi-label' }, t('estimatedCost')), h('span', { class: 'small muted' }, rangeLabel(range))),
      h('div', { class: 'hero-body' },
        h('div', {}, h('p', { class: 'hero-value' }, fmt.cost(totals.cost)), h('p', { class: 'delta' }, delta(totals.cost, prev?.cost, comparison))),
        sparkline(costs)),
      h('p', { class: 'small muted' }, average)),
    card(t('totalTokens'), fmt.compact(totalTokens(totals)), delta(totalTokens(totals), prev && totalTokens(prev), comparison),
      composition(totals, 'comp'), compositionLegend(totals)),
    card(t('sessions'), fmt.int(totals.sessions.size), delta(totals.sessions.size, prev?.sessions.size, comparison),
      h('p', { class: 'small muted' }, durations.length ? t('sessionsSub')(fmt.duration(median(durations)), fmt.int(totals.messages)) : t('noSessions'))),
    card(t('cacheShare'), fmt.percent(cacheShare(totals)), prev ? points(cacheShare(totals), cacheShare(prev), comparison) : t('noComparison'),
      h('p', { class: 'small muted' }, t('cacheSub'))));
}

function qualityNote(view) {
  const unpriced = [...groupRows(view.rows, row => row.model)].filter(([, totals]) => totals.cost === 0 && totalTokens(totals) > 0);
  if (!unpriced.length) return null;
  const tokens = unpriced.reduce((sum, [, totals]) => sum + totalTokens(totals), 0);
  const names = unpriced.map(([model]) => label('model', model)).join(t('listSeparator'));
  return h('div', { class: 'quality', role: 'note' },
    svg('svg', { viewBox: '0 0 16 16', width: 16, height: 16, 'aria-hidden': 'true' },
      svg('path', { d: 'M8 1.5 15 14H1z', style: 'fill:var(--warn)' }),
      svg('path', { d: 'M8 6v4', style: 'stroke:#1b1a18;stroke-width:1.6;stroke-linecap:round' }),
      svg('circle', { cx: 8, cy: 12, r: 0.9, style: 'fill:#1b1a18' })),
    h('p', {}, h('strong', {}, t('costMayBeLow')), ` · ${t('unpriced')(names, fmt.compact(tokens))}`),
    h('button', { type: 'button', class: 'link', onclick: () => update({ view: 'breakdown', dim: 'model' }) }, t('viewInBreakdown')));
}

function granularityControl(view) {
  if (view.granularities.length < 2) return null;
  return [
    h('span', { class: 'small muted' }, t('interval')),
    segmented(t('interval'), GRANULARITIES.filter(item => view.granularities.includes(item))
      .map(item => ({ value: item, label: t('intervals')[item], pressed: item === view.granularity })), granularity => update({ granularity })),
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
  const title = t('trendTitle')(t('every')[view.granularity], state.metric);
  return h('section', { class: 'card trend' },
    sectionHead('01', title, t('stackedBy')(t(state.group), rangeLabel(view.range)),
      h('span', { class: 'small muted' }, t('group')),
      segmented(t('group'), ['model', 'tool', 'project'].map(key => ({ value: key, label: t(key), pressed: state.group === key })), group => update({ group })),
      granularityControl(view),
      h('button', { type: 'button', class: 'link', onclick: () => update({ view: 'breakdown', dim: 'period' }) }, t('viewTable'))),
    h('div', { class: 'legend' }, series.map(item => h('span', { class: 'legend-item' }, swatch(seriesColor(state.group, item.id)), label(state.group, item.id)))),
    trendChart(series, view, title),
    view.granularity === 'day' ? h('p', { class: 'hint' }, t('drillHint')) : null);
}

function trendChart(series, view, title) {
  const { keys, granularity, range } = view;
  const totals = keys.map((_, index) => series.reduce((sum, item) => sum + item.values[index], 0));
  const max = Math.max(0, ...totals);
  const step = niceStep(max);
  const drillable = granularity === 'day';
  const bars = h('div', { class: drillable ? 'bars drillable' : 'bars' },
    keys.map((_, index) => stackBar(series, index, totals[index], (totals[index] / (step * 4)) * PLOT_HEIGHT, state.group)));
  const tip = h('div', { class: 'tip', hidden: true });
  const live = h('p', { class: 'sr-only', 'aria-live': 'polite' });
  const ticks = [0, 1, 2, 3, 4];
  const plot = h('div', { class: 'plot', tabindex: '0', 'aria-label': t('chartLabel')(title) },
    ticks.map(tick => h('div', { class: tick ? 'tick' : 'tick base', style: `bottom:${tick * 25}%` })),
    bars, tip, max ? null : h('p', { class: 'plot-empty' }, t('emptyRange')));
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
    if (drillable) update({ day: keys[index], session: null });
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

function rankSub(key, value, totals) {
  const cache = t('cacheShort')(fmt.percent(cacheShare(totals), 0));
  if (key === 'model') {
    const price = totals.cost > 0 && totalTokens(totals) > 0 ? t('perMillion')(fmt.cost((totals.cost / totalTokens(totals)) * 1e6)) : t('noPrice');
    return `${price} · ${cache}`;
  }
  if (key === 'tool') return `${t('sessionCount')(fmt.int(totals.sessions.size))} · ${cache}`;
  const merged = snapshot.projects.get(value)?.length ?? 0;
  return [
    t('sessionCount')(fmt.int(totals.sessions.size)),
    t('lastActive')(fmt.md([...totals.dates].sort().at(-1))),
    merged > 1 ? t('mergedPaths')(merged) : null,
  ].filter(Boolean).join(' · ');
}

function rankCard(view, key, number) {
  const groups = [...groupRows(view.rows, row => row[key])].sort((a, b) => amount(b[1]) - amount(a[1]));
  const max = groups.length ? amount(groups[0][1]) : 0;
  return h('div', { class: 'card rank' },
    sectionHead(number, t(key), null, h('span', { class: 'small muted' }, t('clickToFilter'))),
    groups.slice(0, 6).map(([value, totals]) => h('button', {
      type: 'button', class: 'rank-row', 'aria-pressed': String(state.filters[key].has(value)), 'data-focus': `rank:${key}:${value}`,
      title: key === 'project' ? projectTitle(value) : label(key, value), onclick: () => toggleFilter(key, value),
    },
    h('span', { class: 'rank-name' }, key === state.group ? swatch(seriesColor(key, value)) : null, h('span', { class: 'ellipsis' }, label(key, value))),
    h('span', { class: 'rank-value num' }, formatAmount(amount(totals))),
    h('span', { class: 'rank-meter' },
      h('span', { class: 'track', 'aria-hidden': 'true' }, h('span', { class: 'fill', style: `width:${max > 0 ? (amount(totals) / max) * 100 : 0}%` })),
      h('span', { class: 'rank-sub num' }, rankSub(key, value, totals))))),
    groups.length ? null : h('p', { class: 'menu-empty' }, t('noUsage')),
    h('button', { type: 'button', class: 'link more', onclick: () => update({ view: 'breakdown', dim: key }) }, t('allOf')(fmt.int(groups.length), t(key))));
}

function heatSection(view) {
  const grid = weekdayHourGrid(view.rows, state.metric);
  const other = state.metric === 'cost' ? 'tokens' : 'cost';
  const otherGrid = weekdayHourGrid(view.rows, other);
  const total = grid.flat().reduce((sum, value) => sum + value, 0);
  let max = 0;
  let peak = null;
  grid.forEach((line, day) => line.forEach((value, hour) => {
    if (value > max) {
      max = value;
      peak = [day, hour];
    }
  }));
  const level = value => (value > 0 ? 1 + Math.min(5, Math.floor((value / max) * 6)) : 0);
  const hour = value => String(value).padStart(2, '0');
  const cells = h('div', { class: 'heat-grid', tabindex: '0', 'aria-label': t('rhythmLabel') },
    h('span', { class: 'heat-day' }),
    Array.from({ length: 24 }, (_, value) => h('span', { class: 'heat-hour num' }, value % 3 ? '' : String(value))),
    grid.map((line, day) => [
      h('span', { class: 'heat-day' }, fmt.weekdays[day]),
      line.map((value, index) => h('span', { class: 'heat-cell', 'data-day': day, 'data-hour': index, style: `background:var(--heat-${level(value)})` })),
    ]));
  const tip = h('div', { class: 'tip', hidden: true });
  const live = h('p', { class: 'sr-only', 'aria-live': 'polite' });
  const card = h('section', { class: 'card heat' },
    sectionHead('05', t('rhythm'), null,
      h('span', { class: 'small muted' }, peak ? t('peak')(fmt.weekdays[peak[0]], hour(peak[1])) : t('noUsage'))),
    h('div', { class: 'chart-scroll' }, cells),
    h('div', { class: 'heat-legend', 'aria-hidden': 'true' },
      h('span', {}, t('less')), [1, 2, 3, 4, 5, 6].map(step => swatch(`var(--heat-${step})`)), h('span', {}, t('more')), h('span', {}, t('rhythmNote'))),
    tip, live);
  let active = null;
  // The tip lives on the card, outside the scrolling grid, so the top row's tip is not clipped.
  // Keyboard moves reveal cells that narrow screens have scrolled out of view.
  const show = (day, index, reveal = false) => {
    cells.querySelector('.heat-cell.active')?.classList.remove('active');
    const cell = cells.querySelector(`[data-day="${day}"][data-hour="${index}"]`);
    cell.classList.add('active');
    if (reveal) cell.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    active = [day, index];
    const value = grid[day][index];
    const heading = `${fmt.weekdays[day]} ${hour(index)}:00–${hour(index + 1)}:00`;
    const otherValue = other === 'cost' ? fmt.cost(otherGrid[day][index]) : t('tokenCount')(fmt.compact(otherGrid[day][index]));
    tip.replaceChildren(
      h('p', { class: 'small muted' }, heading),
      h('p', { class: 'tip-value num' }, formatAmount(value)),
      h('p', { class: 'small muted num' }, `${t('shareOfRange')(fmt.percent(total > 0 ? value / total : 0))} · ${otherValue}`));
    const cardBox = card.getBoundingClientRect();
    const cellBox = cell.getBoundingClientRect();
    const left = cellBox.left - cardBox.left + cellBox.width / 2;
    tip.style.left = `${left}px`;
    tip.style.top = `${cellBox.top - cardBox.top - 6}px`;
    tip.style.transform = `translate(${left < 130 ? -12 : left > cardBox.width - 130 ? -88 : -50}%, -100%)`;
    tip.hidden = false;
    live.textContent = `${heading}: ${formatAmount(value)}`;
  };
  const hide = () => {
    cells.querySelector('.heat-cell.active')?.classList.remove('active');
    active = null;
    tip.hidden = true;
  };
  cells.addEventListener('pointermove', event => {
    const cell = event.target.closest('.heat-cell');
    if (!cell) return;
    const [day, index] = [Number(cell.dataset.day), Number(cell.dataset.hour)];
    if (active?.[0] !== day || active?.[1] !== index) show(day, index);
  });
  cells.addEventListener('pointerleave', hide);
  cells.addEventListener('focus', () => show(...(active ?? peak ?? [0, 0]), true));
  cells.addEventListener('blur', hide);
  cells.addEventListener('keydown', event => {
    const move = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[event.key];
    if (!move) return;
    event.preventDefault();
    const [day, index] = active ?? [0, 0];
    show(Math.min(6, Math.max(0, day + move[0])), Math.min(23, Math.max(0, index + move[1])), true);
  });
  return card;
}

function topSessionsSection(view) {
  const sessions = [...groupRows(view.rows, row => row.session)].filter(([id]) => snapshot.sessions.has(id));
  const ranked = sessions.sort((a, b) => amount(b[1]) - amount(a[1]));
  const { count, share } = topTenthShare(sessions.map(([, totals]) => totals.cost));
  const sort = state.metric === 'cost' ? 'cost' : 'tokens';
  return h('section', { class: 'card top' },
    sectionHead('06', state.metric === 'cost' ? t('topByCost') : t('topByTokens'), null,
      sessions.length ? h('span', { class: 'small muted' }, t('concentration')(fmt.int(count), fmt.percent(share, 0))) : null),
    ranked.slice(0, 5).map(([id, totals], index) => {
      const meta = snapshot.sessions.get(id);
      return h('button', { type: 'button', class: 'top-row', 'data-focus': `top:${id}`, onclick: () => update({ view: 'sessions', sort, session: id, q: '' }) },
        h('span', { class: 'mono small muted' }, String(index + 1)),
        h('span', { class: 'ellipsis' },
          h('span', { class: 'block ellipsis' }, sessionName(meta)),
          h('span', { class: 'block ellipsis small muted' }, `${label('project', meta.project)} · ${meta.tool} · ${fmt.md(localDateKey(meta.start))}`)),
        h('span', { class: 'right' },
          h('span', { class: 'block num', style: 'font-weight:600' }, formatAmount(amount(totals))),
          h('span', { class: 'block num small muted' }, fmt.duration(sessionMinutes(meta)))));
    }),
    sessions.length ? null : h('p', { class: 'menu-empty' }, t('noUsage')),
    h('button', { type: 'button', class: 'link more', onclick: () => update({ view: 'sessions', sort, session: null }) }, t('viewAllSessions')));
}

function sessionsView(view) {
  const modelTokens = new Map();
  for (const row of view.rows) {
    if (!modelTokens.has(row.session)) modelTokens.set(row.session, new Map());
    const models = modelTokens.get(row.session);
    models.set(row.model, (models.get(row.model) ?? 0) + rowTokens(row));
  }
  const query = state.q.trim().toLocaleLowerCase();
  const sorters = {
    recent: (a, b) => b.meta.start - a.meta.start,
    cost: (a, b) => b.totals.cost - a.totals.cost,
    tokens: (a, b) => totalTokens(b.totals) - totalTokens(a.totals),
    duration: (a, b) => sessionMinutes(b.meta) - sessionMinutes(a.meta),
  };
  const list = [...groupRows(view.rows, row => row.session)]
    .map(([id, totals]) => ({ id, totals, meta: snapshot.sessions.get(id) }))
    .filter(item => item.meta && (!query
      || `${sessionName(item.meta)} ${label('project', item.meta.project)} ${item.meta.project} ${item.meta.tool}`.toLocaleLowerCase().includes(query)))
    .sort(sorters[state.sort]);
  const selected = list.find(item => item.id === state.session) ?? list[0];
  const shown = list.slice(0, sessionLimit);
  const listCard = h('section', { class: 'card sess-list', 'aria-label': t('sessions') },
    h('div', { class: 'toolbar' },
      h('label', { class: 'search' },
        h('span', { class: 'sr-only' }, t('searchSessions')),
        svg('svg', { class: 'icon', viewBox: '0 0 16 16', width: 14, height: 14, 'aria-hidden': 'true' }, svg('circle', { cx: 7, cy: 7, r: 4.5 }), svg('path', { d: 'm10.5 10.5 3 3' })),
        h('input', { type: 'search', value: state.q, placeholder: t('searchSessions'), 'data-focus': 'session-search', oninput: event => update({ q: event.target.value }) })),
      segmented(t('sort'), SORTS.map(sort => ({ value: sort, label: sort === 'duration' ? t('durationLabel') : t(sort), pressed: state.sort === sort })), sort => update({ sort })),
      h('span', { class: 'small muted count-label' }, t('sessionCountIn')(fmt.int(list.length), rangeLabel(view.range)))),
    h('div', { class: 'sess-grid sess-head', 'aria-hidden': 'true' },
      h('span', {}, t('session')), h('span', { class: 'hide-sm' }, t('started')), h('span', { class: 'hide-sm' }, t('durationLabel')),
      h('span', { class: 'hide-sm right' }, t('replies')), h('span', { class: 'hide-sm' }, t('models')),
      h('span', { class: 'hide-sm right' }, t(state.metric === 'cost' ? 'tokens' : 'cost')),
      h('span', { class: 'hide-sm right' }, t('cached')), h('span', { class: 'right' }, t(state.metric))),
    shown.map(item => sessionRow(item, modelTokens.get(item.id), item === selected)),
    list.length ? null : h('p', { class: 'empty' }, t('noMatchingSessions')),
    h('div', { class: 'list-footer' },
      h('span', {}, t('showing')(fmt.int(shown.length), fmt.int(list.length))),
      list.length > shown.length
        ? h('button', { type: 'button', class: 'link', 'data-focus': 'load-more', onclick: () => { sessionLimit += SESSION_PAGE_SIZE; render(); } }, t('loadMore'))
        : null));
  return [h('div', { class: 'sess-layout' }, listCard, sessionDetail(selected))];
}

function sessionRow(item, modelTokens, selected) {
  const { meta, totals } = item;
  const models = [...(modelTokens ?? [])].sort((a, b) => b[1] - a[1]);
  const tokens = className => h('span', { class: `tokens-cell ${className}` },
    h('span', { class: 'num' }, fmt.compact(totalTokens(totals))), composition(totals, 'mini-comp'));
  const cost = className => h('span', { class: `num right ${className}` }, fmt.cost(totals.cost));
  // The selected metric is the emphasized last column, the only figure left on phones.
  const [secondary, primary] = state.metric === 'cost' ? [tokens, cost] : [cost, tokens];
  return h('button', {
    type: 'button', class: 'sess-grid sess-row', 'aria-pressed': String(selected), 'data-focus': `session:${item.id}`,
    onclick: () => {
      update({ session: item.id });
      // Below 1100px the details stack under the list.
      if (matchMedia('(max-width: 1100px)').matches) document.querySelector('.detail')?.scrollIntoView({ block: 'start' });
    },
  },
  h('span', { class: 'ellipsis' },
    h('span', { class: 'sess-name ellipsis' }, sessionName(meta)),
    h('span', { class: 'block ellipsis muted' }, `${label('project', meta.project)} · ${meta.tool}`)),
  h('span', { class: 'hide-sm num' }, `${fmt.md(localDateKey(meta.start))} ${fmt.clock(meta.start)}`),
  h('span', { class: 'hide-sm' }, fmt.duration(sessionMinutes(meta))),
  h('span', { class: 'hide-sm num right' }, fmt.int(totals.messages)),
  h('span', { class: 'hide-sm models' },
    models.slice(0, 2).map(([model]) => h('span', { class: 'model-chip' }, swatch(seriesColor('model', model)), h('span', { class: 'ellipsis' }, label('model', model)))),
    models.length > 2 ? h('span', { class: 'muted' }, `+${models.length - 2}`) : null),
  secondary('hide-sm'),
  h('span', { class: 'hide-sm num right' }, fmt.percent(cacheShare(totals), 0)),
  primary('value-cell'));
}

function sessionDetail(item) {
  if (!item) return h('aside', { class: 'card detail' }, h('p', { class: 'muted' }, t('selectSession')));
  const { meta } = item;
  const rows = snapshot.rows.filter(row => row.session === meta.id);
  const totals = sumRows(rows);
  const granularity = meta.end - meta.start > 48 * 3600000 ? 'day' : 'hour';
  const first = periodKey(localDateKey(meta.start), meta.start.getHours(), granularity);
  const last = periodKey(localDateKey(meta.end), meta.end.getHours(), granularity);
  const keys = bucketKeys({ first: localDateKey(meta.start), last: localDateKey(meta.end) }, granularity, new Date())
    .filter(key => key >= first && key <= last);
  const series = stackedSeries(rows, keys, granularity, 'model', 'tokens', slots.model);
  const sums = keys.map((_, index) => series.reduce((sum, entry) => sum + entry.values[index], 0));
  const max = Math.max(0, ...sums);
  const every = Math.max(1, Math.ceil(keys.length / 8));
  const models = [...groupRows(rows, row => row.model)].sort((a, b) => totalTokens(b[1]) - totalTokens(a[1]));
  const sameDay = localDateKey(meta.start) === localDateKey(meta.end);
  const end = sameDay ? fmt.clock(meta.end) : `${fmt.md(localDateKey(meta.end))} ${fmt.clock(meta.end)}`;
  const copy = h('button', {
    type: 'button', class: 'btn',
    onclick: async () => {
      try {
        await navigator.clipboard.writeText(meta.id);
        copy.textContent = t('copied');
      } catch {
        copy.textContent = meta.id;
      }
    },
  }, t('copy'));
  return h('aside', { class: 'card detail', 'aria-label': t('sessionDetails') },
    h('div', {},
      h('p', { class: 'section-no' }, t('sessionDetails')),
      h('h2', {}, sessionName(meta)),
      h('p', { class: 'small', style: 'margin-top:6px;color:var(--ink-2)' }, `${meta.tool} · ${fmt.md(localDateKey(meta.start))} ${fmt.clock(meta.start)} – ${end} · ${fmt.duration(sessionMinutes(meta))}`),
      h('p', { class: 'mono small muted ellipsis', title: projectTitle(meta.project) }, snapshot.projects.has(meta.project) ? meta.project : label('project', meta.project))),
    h('div', { class: 'detail-stats' },
      [[t('cost'), fmt.cost(totals.cost)], [t('tokens'), fmt.compact(totalTokens(totals))], [t('replies'), fmt.int(totals.messages)], [t('toolCalls'), fmt.int(totals.toolCalls)]]
        .map(([name, value]) => h('div', { class: 'stat' }, h('p', { class: 'small muted' }, name), h('p', {}, value)))),
    h('div', {},
      h('div', { class: 'section-head' }, h('h3', {}, granularity === 'hour' ? t('tokensPerHour') : t('tokensPerDay')), h('span', { class: 'small muted' }, t('stackedByModel'))),
      h('div', { class: 'timeline' }, keys.map((key, index) => {
        const bar = stackBar(series, index, sums[index], max ? (sums[index] / max) * TIMELINE_HEIGHT : 0, 'model');
        bar.title = `${periodLabel(key, granularity, false, { days: 2 })} · ${fmt.compact(sums[index])}`;
        return bar;
      })),
      h('div', { class: 'timeline-labels', 'aria-hidden': 'true' },
        keys.map((key, index) => h('span', {}, index % every ? '' : periodLabel(key, granularity, true, { days: 1 }))))),
    h('div', {},
      h('h3', {}, t('models')),
      models.map(([model, modelTotals]) => h('div', { class: 'detail-model' },
        h('span', { class: 'rank-name' }, swatch(seriesColor('model', model)), h('span', { class: 'ellipsis' }, label('model', model))),
        h('span', { class: 'num muted' }, `${t('replyCount')(fmt.int(modelTotals.messages))} · ${fmt.compact(totalTokens(modelTotals))}`),
        h('span', { class: 'num right', style: 'font-weight:600' }, modelTotals.cost > 0 ? fmt.cost(modelTotals.cost) : t('noPrice'))))),
    h('div', {}, h('h3', {}, t('tokenMix')), composition(totals, 'comp'), compositionLegend(totals)),
    h('div', { class: 'detail-id' }, h('span', { class: 'mono small muted ellipsis', title: meta.id }, meta.id), copy),
    h('button', {
      type: 'button', class: 'btn',
      onclick: () => update({ view: 'overview', day: null, filters: { tool: new Set(), model: new Set(), project: new Set([meta.project]) } }),
    }, t('onlyThisProject')));
}

function breakdownView(view) {
  const dim = state.dim;
  const keyOf = dim === 'period' ? row => periodKey(row.date, row.hour, view.granularity) : row => row[dim];
  const groups = [...groupRows(view.rows, keyOf)].sort(dim === 'period'
    ? (a, b) => (a[0] < b[0] ? 1 : a[0] > b[0] ? -1 : 0)
    : (a, b) => amount(b[1]) - amount(a[1]) || b[1].cost - a[1].cost || totalTokens(b[1]) - totalTokens(a[1]));
  // Token counts are compact so every column fits; the exact count is the cell's title.
  const tokens = value => (value ? h('td', { title: fmt.int(value) }, fmt.compact(value)) : h('td', {}, '—'));
  const share = totals => h('td', {}, amount(view.totals) > 0 ? fmt.percent(amount(totals) / amount(view.totals)) : '—');
  const costColumn = ['cost', totals => h('td', {}, fmt.cost(totals.cost))];
  const tokensColumn = ['totalTokens', totals => tokens(totalTokens(totals))];
  // The selected metric leads the table and its share follows it.
  const columns = [
    ...(state.metric === 'cost'
      ? [costColumn, ['shareOfCost', share], tokensColumn]
      : [tokensColumn, ['shareOfTokens', share], costColumn]),
    ['input', totals => tokens(totals.inputTokens)],
    ['cached', totals => tokens(totals.cachedTokens)],
    ['output', totals => tokens(totals.outputTokens)],
    ['reasoning', totals => tokens(totals.reasoningTokens)],
    ['cacheShare', totals => h('td', {}, fmt.percent(cacheShare(totals)))],
    ['averagePrice', totals => h('td', {}, totals.cost > 0 && totalTokens(totals) > 0 ? fmt.cost((totals.cost / totalTokens(totals)) * 1e6) : '—')],
    ['sessions', totals => h('td', {}, fmt.int(totals.sessions.size))],
    ['replies', totals => h('td', {}, fmt.int(totals.messages))],
  ];
  const cells = totals => columns.map(([, cell]) => cell(totals));
  const exportValues = totals => {
    const values = {
      cost: totals.cost, totalTokens: totalTokens(totals), input: totals.inputTokens,
      cached: totals.cachedTokens, output: totals.outputTokens, reasoning: totals.reasoningTokens,
      cacheShare: cacheShare(totals), sessions: totals.sessions.size, replies: totals.messages,
      shareOfCost: amount(view.totals) > 0 ? amount(totals) / amount(view.totals) : '',
      shareOfTokens: amount(view.totals) > 0 ? amount(totals) / amount(view.totals) : '',
      averagePrice: totals.cost > 0 && totalTokens(totals) > 0 ? totals.cost / totalTokens(totals) * 1e6 : '',
    };
    return columns.map(([name]) => values[name]);
  };
  const exportCsv = () => {
    const rows = [
      [t(dim), ...columns.map(([name]) => t(name))],
      ...groups.map(([key, totals]) => [dim === 'period' ? key : label(dim, key), ...exportValues(totals)]),
      [t('total'), ...exportValues(view.totals)],
    ];
    const url = URL.createObjectURL(new Blob(['\uFEFF', csv(rows)], { type: 'text/csv;charset=utf-8' }));
    const link = h('a', { href: url, download: `splitrail-${dim}.csv` });
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
  };
  const nameCell = key => {
    if (dim !== 'period') {
      return h('button', {
        type: 'button', class: 'row-link', 'aria-pressed': String(state.filters[dim].has(key)), 'data-focus': `breakdown:${key}`,
        title: dim === 'project' ? projectTitle(key) : null, onclick: () => toggleFilter(dim, key),
      }, label(dim, key));
    }
    const text = periodLabel(key, view.granularity, false, view.range);
    return view.granularity === 'day'
      ? h('button', { type: 'button', class: 'row-link', 'data-focus': `breakdown:${key}`, onclick: () => update({ view: 'overview', day: key, session: null }) }, text)
      : text;
  };
  return [h('section', { class: 'card table-card', 'aria-label': t('breakdown') },
    h('div', { class: 'toolbar' },
      h('span', { class: 'small muted' }, t('by')),
      segmented(t('by'), BREAKDOWNS.map(key => ({ value: key, label: t(key), pressed: key === dim })), value => update({ dim: value })),
      dim === 'period' ? granularityControl(view) : null,
      h('button', { type: 'button', class: 'btn', onclick: exportCsv }, t('exportCsv')),
      h('span', { class: 'small muted' }, [
        t('rowsIn')(fmt.int(groups.length), rangeLabel(view.range)),
        ...(dim === 'period' ? [t('onlyActivePeriods')] : [t('sortedBy')[state.metric], t('clickToFilter')]),
      ].join(' · '))),
    h('div', { class: 'table-scroll' }, h('table', {},
      h('thead', {}, h('tr', {},
        h('th', { scope: 'col' }, dim === 'period' ? t('periodBy')(t('intervals')[view.granularity]) : t(dim)),
        columns.map(([name]) => h('th', { scope: 'col' }, t(name))))),
      h('tbody', {}, groups.map(([key, totals]) => h('tr', {}, h('td', { class: 'name-cell' }, nameCell(key)), cells(totals)))),
      h('tfoot', {}, h('tr', {}, h('td', {}, t('total')), cells(view.totals))))))];
}

elements.refresh.addEventListener('click', () => load(true));
elements.language.addEventListener('change', event => setLanguage(event.target.value, true));
window.addEventListener('hashchange', () => {
  state = readHash();
  sessionLimit = SESSION_PAGE_SIZE;
  render();
});
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
setLanguage(language, false);
load();
schedulePoll();
