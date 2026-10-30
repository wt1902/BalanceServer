"use strict";

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

const COLORS = [
  "red", "orange", "yellow", "green", "mint", "teal", "cyan", "blue",
  "indigo", "purple", "pink", "gray", "coral", "peach", "gold", "lime",
  "forest", "turquoise", "sky", "navy", "lavender", "magenta", "brown", "slate",
];

const ICONS = [
  ["star.fill", "★"], ["heart.fill", "♥"], ["sparkles", "✦"], ["bolt.fill", "ϟ"], ["flame.fill", "♨"],
  ["drop.fill", "●"], ["cart.fill", "🛒"], ["bag.fill", "▣"], ["creditcard.fill", "▤"], ["banknote.fill", "₽"],
  ["gift.fill", "🎁"], ["tag.fill", "◇"], ["cup.and.saucer.fill", "☕"], ["fork.knife", "♨"], ["takeoutbag.and.cup.and.straw.fill", "🥡"],
  ["birthday.cake.fill", "🎂"], ["car.fill", "🚗"], ["bus.fill", "🚌"], ["tram.fill", "🚊"], ["bicycle", "🚲"],
  ["airplane", "✈"], ["fuelpump.fill", "⛽"], ["house.fill", "⌂"], ["bed.double.fill", "▰"], ["lightbulb.fill", "☀"],
  ["wifi", "⌁"], ["phone.fill", "▯"], ["printer.fill", "▤"], ["pawprint.fill", "🐾"], ["leaf.fill", "♧"],
  ["figure.walk", "♙"], ["figure.run", "➜"], ["dumbbell.fill", "↔"], ["cross.case.fill", "+"], ["pills.fill", "◉"],
  ["stethoscope", "∿"], ["book.fill", "▥"], ["graduationcap.fill", "⌃"], ["laptopcomputer", "▰"], ["briefcase.fill", "▣"],
  ["gamecontroller.fill", "⌘"], ["music.note", "♫"], ["film.fill", "▦"], ["camera.fill", "◉"], ["paintpalette.fill", "◒"],
  ["tshirt.fill", "♜"], ["hammer.fill", "⚒"], ["wrench.and.screwdriver.fill", "⚙"], ["shippingbox.fill", "⬡"], ["globe.europe.africa.fill", "◎"],
];

const EMOJIS = [
  "🍔", "☕️", "🛒", "🚕", "✈️", "🏠", "💡", "📱", "💻", "🎮",
  "🎬", "🎵", "📚", "🎓", "💊", "🏋️", "🐶", "🐱", "👕", "🎁",
  "💰", "💳", "📈", "🔧", "🌿", "❤️", "⭐️", "🎯", "🧾", "🏖️",
];

const BUILT_IN = {
  expense: [
    ["Продукты", "cart.fill", "orange"], ["Транспорт", "car.fill", "blue"],
    ["Дом", "house.fill", "indigo"], ["Развлечения", "gamecontroller.fill", "pink"],
    ["Здоровье", "cross.case.fill", "red"], ["Покупки", "bag.fill", "purple"],
    ["Подписки", "repeat", "cyan"], ["Образование", "book.fill", "mint"],
    ["Другое", "ellipsis.circle.fill", "gray"],
  ],
  income: [
    ["Зарплата", "banknote.fill", "green"], ["Подработка", "laptopcomputer", "teal"],
    ["Инвестиции", "chart.line.uptrend.xyaxis", "blue"], ["Подарок", "gift.fill", "pink"],
    ["Другое", "plus.circle.fill", "gray"],
  ],
};

const ICON_GLYPHS = new Map(ICONS);
const EXTRA_GLYPHS = {
  "repeat": "↻", "ellipsis.circle.fill": "•••", "plus.circle.fill": "+",
  "laptopcomputer": "▰", "chart.line.uptrend.xyaxis": "↗", "tune": "≡",
};

const state = {
  user: null,
  route: "dashboard",
  transactions: new Map(),
  categories: new Map(),
  budgets: new Map(),
  cursor: 0,
  syncing: false,
  analyticsMonths: 1,
  transactionQuery: "",
  transactionKind: "all",
  authMode: "login",
  lastSyncAt: null,
};

const preferences = loadPreferences();
const mediaDark = window.matchMedia("(prefers-color-scheme: dark)");

class APIError extends Error {
  constructor(message, code, status) {
    super(message);
    this.name = "APIError";
    this.code = code;
    this.status = status;
  }
}

function loadPreferences() {
  try {
    const value = JSON.parse(localStorage.getItem("balance.web.preferences") || "{}");
    return {
      currency: ["RUB", "EUR", "USD", "SEK"].includes(value.currency) ? value.currency : "RUB",
      theme: ["system", "light", "dark"].includes(value.theme) ? value.theme : "system",
    };
  } catch {
    return { currency: "RUB", theme: "system" };
  }
}

function savePreferences() {
  localStorage.setItem("balance.web.preferences", JSON.stringify(preferences));
}

function applyTheme() {
  const dark = preferences.theme === "dark" || (preferences.theme === "system" && mediaDark.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  $("meta[name='theme-color']")?.setAttribute("content", dark ? "#1b1b21" : "#5b5bd6");
}

function escapeHTML(value) {
  return String(value ?? "").replace(/[&<>"']/g, character => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;",
  })[character]);
}

function safeColor(value) {
  return COLORS.includes(value) ? value : "indigo";
}

function iconGlyph(value) {
  return ICON_GLYPHS.get(value) || EXTRA_GLYPHS[value] || "◆";
}

function categoryBadge(item, size = "") {
  const visual = item.emoji ? escapeHTML(item.emoji) : `<span class="symbol">${escapeHTML(iconGlyph(item.icon))}</span>`;
  return `<span class="category-badge color-${safeColor(item.colorName)} ${size}">${visual}</span>`;
}

function formatMoney(value, options = {}) {
  const amount = Number.isFinite(Number(value)) ? Number(value) : 0;
  return new Intl.NumberFormat("ru-RU", {
    style: "currency",
    currency: preferences.currency,
    maximumFractionDigits: Math.abs(amount % 1) < 0.0001 ? 0 : 2,
    ...options,
  }).format(amount);
}

function formatDate(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return new Intl.DateTimeFormat("ru-RU", { day: "numeric", month: "short", year: "numeric" }).format(date);
}

function formatDateTime(value) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "никогда";
  return new Intl.DateTimeFormat("ru-RU", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }).format(date);
}

function localDateInput(value = new Date()) {
  const date = new Date(value);
  const offset = date.getTimezoneOffset() * 60_000;
  return new Date(date.getTime() - offset).toISOString().slice(0, 10);
}

function dateInputISO(value) {
  const [year, month, day] = value.split("-").map(Number);
  return new Date(year, month - 1, day, 0, 0, 0, 0).toISOString();
}

function randomID() {
  return crypto.randomUUID ? crypto.randomUUID() : `${Date.now().toString(36)}-${crypto.getRandomValues(new Uint32Array(2)).join("-")}`;
}

function firstGrapheme(value) {
  const trimmed = String(value || "").trim();
  if (!trimmed) return "";
  if (Intl.Segmenter) return [...new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(trimmed)][0]?.segment || "";
  return Array.from(trimmed)[0] || "";
}

function allTransactions() {
  return [...state.transactions.values()];
}

function allCategories() {
  return [...state.categories.values()];
}

function allBudgets() {
  return [...state.budgets.values()];
}

function categoriesFor(kind) {
  const builtIn = BUILT_IN[kind].map(([name, icon, colorName]) => ({ name, icon, emoji: "", colorName, kindRawValue: kind, builtIn: true }));
  const custom = allCategories()
    .filter(category => category.kindRawValue === kind)
    .sort((a, b) => a.name.localeCompare(b.name, "ru"));
  return [...builtIn, ...custom];
}

function monthBounds(months = 1) {
  const now = new Date();
  const end = new Date(now.getFullYear(), now.getMonth() + 1, 1);
  const start = new Date(now.getFullYear(), now.getMonth() - (months - 1), 1);
  return [start.getTime(), end.getTime()];
}

function inPeriod(transaction, start, end) {
  const time = new Date(transaction.date).getTime();
  return Number.isFinite(time) && time >= start && time < end;
}

function totalBalance() {
  return allTransactions().reduce((sum, transaction) => sum + (transaction.kindRawValue === "income" ? transaction.amount : -transaction.amount), 0);
}

function summaryFor(months = 1) {
  const [start, end] = monthBounds(months);
  const items = allTransactions().filter(item => !item.isBalanceAdjustment && inPeriod(item, start, end));
  return {
    income: items.filter(item => item.kindRawValue === "income").reduce((sum, item) => sum + item.amount, 0),
    expenses: items.filter(item => item.kindRawValue === "expense").reduce((sum, item) => sum + item.amount, 0),
    items,
    start,
    end,
  };
}

function spendingBreakdown(months = 1) {
  const { items } = summaryFor(months);
  const groups = new Map();
  for (const item of items.filter(value => value.kindRawValue === "expense")) {
    const group = groups.get(item.categoryName) || {
      name: item.categoryName, icon: item.categoryIcon, emoji: item.categoryEmoji,
      colorName: item.categoryColorName, amount: 0,
    };
    group.amount += Number(item.amount) || 0;
    groups.set(item.categoryName, group);
  }
  return [...groups.values()].sort((a, b) => b.amount - a.amount);
}

function currentBudget(categoryName) {
  const now = new Date();
  return allBudgets()
    .filter(item => item.categoryName === categoryName)
    .filter(item => {
      const date = new Date(item.monthStart);
      return date.getFullYear() === now.getFullYear() && date.getMonth() === now.getMonth();
    })
    .sort((a, b) => new Date(b._updatedAt) - new Date(a._updatedAt))[0] || null;
}

function currentMonthStartISO() {
  const now = new Date();
  return new Date(now.getFullYear(), now.getMonth(), 1).toISOString();
}

async function apiFetch(path, options = {}, retry = true) {
  const headers = new Headers(options.headers || {});
  if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
  const response = await fetch(path, { ...options, headers, credentials: "same-origin" });
  if (response.status === 401 && retry && path !== "/v1/auth/refresh") {
    const refreshed = await fetch("/v1/auth/refresh", {
      method: "POST", credentials: "same-origin", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ refreshToken: "" }),
    });
    if (refreshed.ok) return apiFetch(path, options, false);
  }
  let payload = null;
  if (response.status !== 204) {
    const type = response.headers.get("Content-Type") || "";
    payload = type.includes("application/json") ? await response.json().catch(() => null) : await response.text().catch(() => "");
  }
  if (!response.ok) {
    const code = payload?.error?.code || "request_failed";
    throw new APIError(localizedError(code, payload?.error?.message), code, response.status);
  }
  return payload;
}

function localizedError(code, fallback) {
  const messages = {
    invalid_credentials: "Неверный email или пароль.",
    invalid_email: "Введите корректный email.",
    invalid_password: "Пароль должен содержать не менее 8 символов.",
    email_exists: "Аккаунт с таким email уже существует.",
    registration_disabled: "Регистрация на этом сервере отключена.",
    rate_limited: "Слишком много попыток. Повторите через минуту.",
    authorization_required: "Необходимо войти в аккаунт.",
    invalid_token: "Сессия истекла. Войдите снова.",
    invalid_refresh_token: "Сессия истекла. Войдите снова.",
    sync_failed: "Не удалось синхронизировать данные.",
  };
  return messages[code] || fallback || "Не удалось выполнить запрос.";
}

function applyChanges(changes) {
  for (const change of [...(changes || [])].sort((a, b) => (a.sequence || 0) - (b.sequence || 0))) {
    const target = change.entity === "transaction" ? state.transactions : change.entity === "category" ? state.categories : change.entity === "budget" ? state.budgets : null;
    if (!target) continue;
    if (change.deleted) target.delete(change.id);
    else if (change.payload) target.set(change.id, { id: change.id, ...change.payload, _updatedAt: change.updatedAt });
  }
}

async function fullSync() {
  if (!state.user || state.syncing) return;
  state.transactions.clear();
  state.categories.clear();
  state.budgets.clear();
  state.cursor = 0;
  await pullChanges(true);
}

async function pullChanges(silent = false) {
  if (!state.user || state.syncing) return;
  state.syncing = true;
  setSyncStatus("syncing", "Синхронизация…");
  try {
    let hasMore = true;
    let guard = 0;
    while (hasMore) {
      const previous = state.cursor;
      const response = await apiFetch("/v1/sync", {
        method: "POST",
        body: JSON.stringify({ cursor: state.cursor, deviceId: webDeviceID(), changes: [], pull: true }),
      });
      applyChanges(response.changes);
      state.cursor = response.cursor;
      hasMore = Boolean(response.hasMore);
      if (hasMore && state.cursor <= previous) throw new Error("Сервер не смог продолжить синхронизацию.");
      if (++guard > 10000) throw new Error("Слишком много страниц синхронизации.");
    }
    state.lastSyncAt = new Date();
    setSyncStatus("ok", "Синхронизировано");
    render();
    if (!silent) toast("Данные синхронизированы");
  } catch (error) {
    handleSyncError(error, silent);
    throw error;
  } finally {
    state.syncing = false;
  }
}

async function pushChanges(changes) {
  if (!state.user || !changes.length) return;
  if (state.syncing) throw new Error("Дождитесь завершения синхронизации.");
  state.syncing = true;
  setSyncStatus("syncing", "Сохранение…");
  try {
    for (let offset = 0; offset < changes.length; offset += 100) {
      const batch = changes.slice(offset, offset + 100);
      let response = await apiFetch("/v1/sync", {
        method: "POST",
        body: JSON.stringify({ cursor: state.cursor, deviceId: webDeviceID(), changes: batch, pull: true }),
      });
      applyChanges(response.changes);
      state.cursor = response.cursor;
      while (response.hasMore) {
        const previous = state.cursor;
        response = await apiFetch("/v1/sync", {
          method: "POST",
          body: JSON.stringify({ cursor: state.cursor, deviceId: webDeviceID(), changes: [], pull: true }),
        });
        applyChanges(response.changes);
        state.cursor = response.cursor;
        if (response.hasMore && state.cursor <= previous) throw new Error("Сервер не смог продолжить синхронизацию.");
      }
    }
    state.lastSyncAt = new Date();
    setSyncStatus("ok", "Синхронизировано");
    render();
  } catch (error) {
    handleSyncError(error, false);
    throw error;
  } finally {
    state.syncing = false;
  }
}

function handleSyncError(error, silent) {
  setSyncStatus("error", "Ошибка синхронизации");
  if (error instanceof APIError && error.status === 401) showAuth();
  if (!silent) toast(error.message || "Ошибка синхронизации", true);
}

function webDeviceID() {
  let value = localStorage.getItem("balance.web.device");
  if (!value) {
    value = `web-${randomID()}`;
    localStorage.setItem("balance.web.device", value);
  }
  return value;
}

function changeFor(entity, id, payload, deleted = false, updatedAt = new Date().toISOString()) {
  return { entity, id, deleted, updatedAt, ...(deleted ? {} : { payload }) };
}

function transactionPayload(value) {
  return {
    amount: Number(value.amount), date: value.date, note: value.note || "",
    categoryName: value.categoryName, categoryIcon: value.categoryIcon || "circle",
    categoryEmoji: value.categoryEmoji || "", categoryColorName: value.categoryColorName || "indigo",
    isBalanceAdjustment: Boolean(value.isBalanceAdjustment), kindRawValue: value.kindRawValue === "income" ? "income" : "expense",
  };
}

function categoryPayload(value) {
  return {
    name: value.name, icon: value.icon || "star", emoji: value.emoji || "", colorName: safeColor(value.colorName),
    kindRawValue: value.kindRawValue === "income" ? "income" : "expense", createdAt: value.createdAt,
  };
}

function budgetPayload(value) {
  return {
    categoryName: value.categoryName, categoryIcon: value.categoryIcon || "circle",
    categoryEmoji: value.categoryEmoji || "", limit: Number(value.limit), monthStart: value.monthStart,
  };
}

function pageHeader(title, subtitle, actions = "") {
  return `<header class="page-header"><div><p class="eyebrow">BALANCE</p><h1>${escapeHTML(title)}</h1><p>${escapeHTML(subtitle)}</p></div>${actions ? `<div class="page-actions">${actions}</div>` : ""}</header>`;
}

function emptyState(icon, title, text, action = "") {
  return `<div class="empty-state"><div><span class="empty-icon">${escapeHTML(icon)}</span><h3>${escapeHTML(title)}</h3><p>${escapeHTML(text)}</p>${action}</div></div>`;
}

function render() {
  if (!state.user) return;
  const renderers = {
    dashboard: renderDashboard,
    transactions: renderTransactions,
    analytics: renderAnalytics,
    budgets: renderBudgets,
    categories: renderCategories,
    settings: renderSettings,
  };
  $("#content").innerHTML = (renderers[state.route] || renderDashboard)();
  $$('[data-route]').forEach(button => button.classList.toggle("is-active", button.dataset.route === state.route));
  document.title = `${routeTitle(state.route)} — Balance`;
}

function routeTitle(route) {
  return ({ dashboard: "Обзор", transactions: "Операции", analytics: "Аналитика", budgets: "Бюджеты", categories: "Категории", settings: "Настройки" })[route] || "Balance";
}

function renderDashboard() {
  const summary = summaryFor(1);
  const recent = allTransactions().sort((a, b) => new Date(b.date) - new Date(a.date)).slice(0, 6);
  const savings = summary.income > 0 ? ((summary.income - summary.expenses) / summary.income) * 100 : 0;
  return `${pageHeader("Обзор", "Главное о ваших финансах за текущий месяц", `
    <button class="button" type="button" data-action="sync">↻ Синхронизировать</button>
    <button class="button button-primary" type="button" data-action="transaction-new">＋ Добавить операцию</button>`)}
    <div class="grid dashboard-grid">
      <section class="card hero-balance">
        <p class="hero-label">Общий баланс</p>
        <div class="hero-value">${formatMoney(totalBalance())}</div>
        <div class="hero-meta">
          <div><span>Доходы за месяц</span><strong>+ ${formatMoney(summary.income)}</strong></div>
          <div><span>Расходы за месяц</span><strong>− ${formatMoney(summary.expenses)}</strong></div>
        </div>
        <button class="button" type="button" data-action="balance-adjust">≡ Скорректировать баланс</button>
      </section>
      <div class="grid grid-2 metric-grid-mobile">
        <section class="card metric-card is-income"><span>Доходы</span><strong>${formatMoney(summary.income)}</strong><div class="metric-foot">Текущий месяц</div></section>
        <section class="card metric-card is-expense"><span>Расходы</span><strong>${formatMoney(summary.expenses)}</strong><div class="metric-foot">Без корректировок</div></section>
        <section class="card metric-card is-primary"><span>Сбережения</span><strong>${formatMoney(summary.income - summary.expenses)}</strong><div class="metric-foot">Доходы минус расходы</div></section>
        <section class="card metric-card"><span>Норма сбережений</span><strong>${Number.isFinite(savings) ? `${Math.round(savings)}%` : "—"}</strong><div class="metric-foot">За текущий месяц</div></section>
      </div>
      <section class="card span-2">
        <div class="card-header"><h2>Последние операции</h2><button class="button button-ghost" type="button" data-route="transactions">Все операции →</button></div>
        <div class="card-body">${recent.length ? `<div class="list">${recent.map(transactionRow).join("")}</div>` : emptyState("↕", "Операций пока нет", "Добавьте первый доход или расход, и он появится на всех ваших устройствах.", `<button class="button button-primary" type="button" data-action="transaction-new">Добавить операцию</button>`)}</div>
      </section>
    </div>`;
}

function transactionRow(transaction) {
  const income = transaction.kindRawValue === "income";
  const item = {
    emoji: transaction.categoryEmoji, icon: transaction.categoryIcon,
    colorName: transaction.categoryColorName,
  };
  const category = transaction.isBalanceAdjustment ? "Корректировка" : transaction.categoryName;
  const note = transaction.note ? ` · ${escapeHTML(transaction.note)}` : "";
  return `<div class="list-row">
    ${categoryBadge(item)}
    <div class="row-main"><strong>${escapeHTML(category)}</strong><span>${formatDate(transaction.date)}${note}</span></div>
    <div class="row-value"><strong class="${income ? "amount-income" : "amount-expense"}">${income ? "+" : "−"} ${formatMoney(transaction.amount)}</strong><span>${income ? "Доход" : "Расход"}</span></div>
    <div class="row-actions">
      <button class="icon-button" type="button" data-action="transaction-edit" data-id="${escapeHTML(transaction.id)}" aria-label="Редактировать">✎</button>
      <button class="icon-button is-danger" type="button" data-action="transaction-delete" data-id="${escapeHTML(transaction.id)}" aria-label="Удалить">×</button>
    </div>
  </div>`;
}

function filteredTransactions() {
  const query = state.transactionQuery.trim().toLocaleLowerCase("ru");
  return allTransactions()
    .filter(item => state.transactionKind === "all" || item.kindRawValue === state.transactionKind)
    .filter(item => !query || `${item.categoryName} ${item.note || ""}`.toLocaleLowerCase("ru").includes(query))
    .sort((a, b) => new Date(b.date) - new Date(a.date));
}

function transactionListHTML() {
  const items = filteredTransactions();
  return items.length ? `<div class="list">${items.map(transactionRow).join("")}</div>` : emptyState("⌕", "Ничего не найдено", "Измените поиск или фильтр, либо добавьте новую операцию.");
}

function renderTransactions() {
  return `${pageHeader("Операции", "Доходы, расходы и ручные корректировки баланса", `<button class="button button-primary" type="button" data-action="transaction-new">＋ Добавить операцию</button>`)}
    <div class="toolbar">
      <div class="search-wrap"><input id="transaction-search" class="search-control" type="search" placeholder="Поиск по категории и заметке" value="${escapeHTML(state.transactionQuery)}" aria-label="Поиск операций"></div>
      <select id="transaction-filter" class="select-control" aria-label="Тип операции">
        <option value="all" ${state.transactionKind === "all" ? "selected" : ""}>Все операции</option>
        <option value="expense" ${state.transactionKind === "expense" ? "selected" : ""}>Расходы</option>
        <option value="income" ${state.transactionKind === "income" ? "selected" : ""}>Доходы</option>
      </select>
    </div>
    <section class="card operations-card"><div id="transaction-list" class="card-body">${transactionListHTML()}</div></section>`;
}

function renderAnalytics() {
  const months = state.analyticsMonths;
  const summary = summaryFor(months);
  const breakdown = spendingBreakdown(months);
  const savings = summary.income > 0 ? ((summary.income - summary.expenses) / summary.income) * 100 : 0;
  const max = Math.max(...breakdown.map(item => item.amount), 1);
  const periodName = months === 1 ? "месяц" : `${months} месяца`;
  const insight = summary.income === 0
    ? "Добавьте доходы, чтобы увидеть норму сбережений и персональную подсказку."
    : savings >= 20
      ? `Вы сохраняете ${Math.round(savings)}% доходов — это сильный результат.`
      : savings >= 0
        ? `Вы сохраняете ${Math.round(savings)}% доходов. Попробуйте постепенно приблизиться к 20%.`
        : "Расходы выше доходов за выбранный период. Проверьте самые крупные категории.";
  return `<div class="analytics-page">
    ${pageHeader("Аналитика", "Структура расходов и динамика сбережений", `<button class="button" type="button" data-action="sync">↻ Обновить</button>`)}
      <div class="segmented period-switch">
        <button type="button" data-analytics-months="1" class="${months === 1 ? "is-active" : ""}">Месяц</button>
        <button type="button" data-analytics-months="3" class="${months === 3 ? "is-active" : ""}">3 месяца</button>
        <button type="button" data-analytics-months="12" class="${months === 12 ? "is-active" : ""}">12 месяцев</button>
      </div>
      <div class="grid grid-4 metric-grid-mobile analytics-metrics">
        <section class="card metric-card is-income"><span>Доходы</span><strong>${formatMoney(summary.income)}</strong><div class="metric-foot">За ${periodName}</div></section>
        <section class="card metric-card is-expense"><span>Расходы</span><strong>${formatMoney(summary.expenses)}</strong><div class="metric-foot">За ${periodName}</div></section>
        <section class="card metric-card is-primary"><span>Сбережения</span><strong>${formatMoney(summary.income - summary.expenses)}</strong><div class="metric-foot">Чистый результат</div></section>
        <section class="card metric-card"><span>Норма</span><strong>${summary.income ? `${Math.round(savings)}%` : "—"}</strong><div class="metric-foot">От доходов</div></section>
      </div>
      <div class="grid analytics-layout">
        <section class="card analytics-breakdown-card">
          <div class="card-header"><h2>Расходы по категориям</h2><span class="field-label">${formatMoney(summary.expenses)}</span></div>
          <div class="card-body">${breakdown.length ? `<div class="breakdown">${breakdown.map(item => `
            <div class="breakdown-row">
              <div class="breakdown-name">${categoryBadge(item, "small")}<span>${escapeHTML(item.name)}</span></div>
              <progress class="progress-${safeColor(item.colorName)}" max="${max}" value="${Math.max(0, item.amount)}"></progress>
              <div class="breakdown-value">${formatMoney(item.amount)}</div>
            </div>`).join("")}</div>` : emptyState("◫", "Недостаточно данных", "В выбранном периоде ещё нет расходов.")}</div>
        </section>
        <aside class="card insight-card"><span class="insight-icon">✦</span><h3>Финансовый итог</h3><p>${escapeHTML(insight)}</p></aside>
      </div>
    </div>`;
}

function renderBudgets() {
  const [start, end] = monthBounds(1);
  const spending = new Map();
  for (const transaction of allTransactions().filter(item => item.kindRawValue === "expense" && !item.isBalanceAdjustment && inPeriod(item, start, end))) {
    spending.set(transaction.categoryName, (spending.get(transaction.categoryName) || 0) + Number(transaction.amount || 0));
  }
  const budgets = allBudgets().filter(item => {
    const date = new Date(item.monthStart);
    const now = new Date();
    return date.getFullYear() === now.getFullYear() && date.getMonth() === now.getMonth() && Number(item.limit) > 0;
  }).sort((a, b) => a.categoryName.localeCompare(b.categoryName, "ru"));
  const content = budgets.length ? budgets.map(budget => {
    const value = spending.get(budget.categoryName) || 0;
    const ratio = Math.min(value / Math.max(Number(budget.limit), 1), 1);
    const over = value > Number(budget.limit);
    const item = { emoji: budget.categoryEmoji, icon: budget.categoryIcon, colorName: "indigo" };
    return `<section class="card budget-item ${over ? "is-over" : ""}">
      <div class="budget-top">${categoryBadge(item)}<div class="budget-copy"><strong>${escapeHTML(budget.categoryName)}</strong><span>${over ? "Лимит превышен" : `Осталось ${formatMoney(Math.max(Number(budget.limit) - value, 0))}`}</span></div>
      <div class="budget-values"><strong>${formatMoney(value)}</strong><span>из ${formatMoney(Number(budget.limit))}</span></div></div>
      <progress max="1" value="${ratio}"></progress>
    </section>`;
  }).join("") : emptyState("◎", "Лимиты не настроены", "Задайте месячный лимит для одной или нескольких категорий расходов.", `<button class="button button-primary" type="button" data-action="budgets-edit">Настроить бюджеты</button>`);
  return `${pageHeader("Бюджеты", "Месячные лимиты по категориям расходов", `<button class="button button-primary" type="button" data-action="budgets-edit">✎ Настроить лимиты</button>`)}
    <div class="budget-list">${content}</div>`;
}

function renderCategories() {
  const categories = allCategories().sort((a, b) => a.name.localeCompare(b.name, "ru"));
  return `${pageHeader("Категории", "Собственные категории синхронизируются со всеми устройствами", `<button class="button button-primary" type="button" data-action="category-new">＋ Добавить категорию</button>`)}
    ${categories.length ? `<div class="grid category-grid">${categories.map(category => `<section class="card category-card">
      ${categoryBadge(category)}
      <div class="row-main"><strong>${escapeHTML(category.name)}</strong><span>${category.kindRawValue === "income" ? "доход" : "расход"}</span></div>
      <div class="row-actions">
        <button class="icon-button" type="button" data-action="category-edit" data-id="${escapeHTML(category.id)}" aria-label="Редактировать">✎</button>
        <button class="icon-button is-danger" type="button" data-action="category-delete" data-id="${escapeHTML(category.id)}" aria-label="Удалить">×</button>
      </div>
    </section>`).join("")}</div>` : `<section class="card">${emptyState("◆", "Своих категорий пока нет", "Создайте категорию с одной из 50 иконок или собственным эмодзи.", `<button class="button button-primary" type="button" data-action="category-new">Добавить категорию</button>`)}</section>`}`;
}

function renderSettings() {
  return `${pageHeader("Настройки", "Отображение, синхронизация и аккаунт")}
    <div class="grid settings-layout">
      <div class="grid">
        <section class="card settings-section">
          <h2>Отображение</h2><p>Настройки сохраняются только в этом браузере.</p>
          <div class="settings-row"><div><strong>Валюта</strong><span>Используется для форматирования сумм</span></div>
            <select id="currency-setting" class="select-control" aria-label="Валюта">${["RUB", "EUR", "USD", "SEK"].map(value => `<option ${preferences.currency === value ? "selected" : ""}>${value}</option>`).join("")}</select></div>
          <div class="settings-row"><div><strong>Тема</strong><span>Светлая, тёмная или системная</span></div>
            <select id="theme-setting" class="select-control" aria-label="Тема">
              <option value="system" ${preferences.theme === "system" ? "selected" : ""}>Системная</option>
              <option value="light" ${preferences.theme === "light" ? "selected" : ""}>Светлая</option>
              <option value="dark" ${preferences.theme === "dark" ? "selected" : ""}>Тёмная</option>
            </select></div>
        </section>
        <section class="card settings-section">
          <h2>Данные</h2><p>Все изменения отправляются на ваш Balance Server.</p>
          <div class="settings-row"><div><strong>Пользовательские категории</strong><span>${state.categories.size} сохранено</span></div><button class="button" type="button" data-route="categories">Открыть</button></div>
          <div class="settings-row"><div><strong>Синхронизация</strong><span>Последняя: ${state.lastSyncAt ? formatDateTime(state.lastSyncAt) : "в этой сессии не выполнялась"}</span></div><button class="button" type="button" data-action="sync">Синхронизировать</button></div>
          <div class="settings-row"><div><strong>Записи</strong><span>${state.transactions.size} операций · ${state.budgets.size} бюджетов</span></div></div>
        </section>
      </div>
      <aside class="card account-card">
        <div class="account-cover"></div>
        <div class="account-content">
          <div class="avatar">${escapeHTML((state.user.email || "Б")[0].toUpperCase())}</div>
          <h3>${escapeHTML(state.user.email)}</h3><p>Аккаунт на этом self-hosted сервере</p>
          <div class="status-line"><span></span><span>Соединение с сервером установлено</span></div>
          <button class="button button-danger" type="button" data-action="logout">Выйти из аккаунта</button>
        </div>
      </aside>
    </div>`;
}

function buildCategoryOptions(kind, selected) {
  return categoriesFor(kind).map(category => `<option value="${escapeHTML(category.name)}" ${category.name === selected ? "selected" : ""}>${escapeHTML(category.emoji ? `${category.emoji} ${category.name}` : category.name)}</option>`).join("");
}

function modalShell(title, body, saveLabel = "Сохранить", form = "editor-form") {
  return `<form id="${form}" class="modal-shell">
    <header class="modal-header"><h2>${escapeHTML(title)}</h2><button class="icon-button" type="button" data-action="dialog-close" aria-label="Закрыть">×</button></header>
    <div class="modal-body">${body}</div>
    <footer class="modal-footer"><button class="button" type="button" data-action="dialog-close">Отмена</button><button class="button button-primary" type="submit">${escapeHTML(saveLabel)}</button></footer>
  </form>`;
}

function showDialog(content) {
  $("#dialog-content").innerHTML = content;
  const dialog = $("#editor-dialog");
  if (!dialog.open) dialog.showModal();
}

function closeDialog() {
  const dialog = $("#editor-dialog");
  if (dialog.open) dialog.close();
  $("#dialog-content").innerHTML = "";
}

function openTransactionEditor(id = null) {
  const existing = id ? state.transactions.get(id) : null;
  const kind = existing?.kindRawValue || "expense";
  const first = categoriesFor(kind)[0];
  const selected = existing?.categoryName || first.name;
  const body = `
    <input type="hidden" name="id" value="${escapeHTML(existing?.id || "")}">
    <input id="transaction-kind" type="hidden" name="kind" value="${kind}">
    <div class="segmented kind-switch" aria-label="Тип операции">
      <button type="button" data-transaction-kind="expense" class="${kind === "expense" ? "is-active" : ""}">Расход</button>
      <button type="button" data-transaction-kind="income" class="${kind === "income" ? "is-active" : ""}">Доход</button>
    </div>
    <label class="field"><span>Сумма</span><input name="amount" inputmode="decimal" required autofocus value="${escapeHTML(existing?.amount || "")}" placeholder="0"></label>
    <label class="field"><span>Категория</span><select id="transaction-category" name="category" required>${buildCategoryOptions(kind, selected)}</select></label>
    <label class="field"><span>Дата</span><input name="date" type="date" required value="${localDateInput(existing?.date || new Date())}"></label>
    <label class="field"><span>Заметка</span><textarea name="note" maxlength="500" placeholder="Необязательно">${escapeHTML(existing?.note || "")}</textarea></label>`;
  showDialog(modalShell(existing ? "Редактирование операции" : "Новая операция", body, "Сохранить", "transaction-form"));
}

function iconChoices(selected) {
  return ICONS.map(([value, glyph]) => `<label title="${escapeHTML(value)}"><input type="radio" name="icon" value="${escapeHTML(value)}" ${value === selected ? "checked" : ""}><span class="choice-option">${escapeHTML(glyph)}</span></label>`).join("");
}

function emojiChoices(selected) {
  return EMOJIS.map(value => `<label><input type="radio" name="emoji-choice" value="${escapeHTML(value)}" ${value === selected ? "checked" : ""}><span class="choice-option">${escapeHTML(value)}</span></label>`).join("");
}

function colorChoices(selected) {
  return COLORS.map(value => `<label title="${value}"><input type="radio" name="color" value="${value}" ${value === selected ? "checked" : ""}><span class="color-swatch swatch-${value}"></span></label>`).join("");
}

function openCategoryEditor(id = null) {
  const existing = id ? state.categories.get(id) : null;
  const kind = existing?.kindRawValue || "expense";
  const visual = existing?.emoji ? "emoji" : "icon";
  const body = `
    <input type="hidden" name="id" value="${escapeHTML(existing?.id || "")}">
    <input id="category-visual" type="hidden" name="visual" value="${visual}">
    <label class="field"><span>Название</span><input name="name" required maxlength="80" autofocus value="${escapeHTML(existing?.name || "")}" placeholder="Например, Питомцы"></label>
    <label class="field"><span>Тип</span><select name="kind" ${existing ? "disabled" : ""}>
      <option value="expense" ${kind === "expense" ? "selected" : ""}>Расход</option><option value="income" ${kind === "income" ? "selected" : ""}>Доход</option>
    </select></label>
    ${existing ? `<input type="hidden" name="kind-fixed" value="${kind}">` : ""}
    <div class="segmented kind-switch"><button type="button" data-category-visual="icon" class="${visual === "icon" ? "is-active" : ""}">Иконка</button><button type="button" data-category-visual="emoji" class="${visual === "emoji" ? "is-active" : ""}">Эмодзи</button></div>
    <div id="icon-section" ${visual === "emoji" ? "hidden" : ""}><p class="field-label">Иконка</p><div class="choice-grid">${iconChoices(existing?.icon || "star.fill")}</div></div>
    <div id="emoji-section" ${visual === "icon" ? "hidden" : ""}>
      <label class="field"><span>Собственный эмодзи</span><input id="category-emoji" name="emoji" maxlength="32" value="${escapeHTML(existing?.emoji || "")}" placeholder="Например, 🐕"></label>
      <div class="choice-grid">${emojiChoices(existing?.emoji || "")}</div>
    </div>
    <div><p class="field-label">Цвет</p><div class="color-grid">${colorChoices(existing?.colorName || "indigo")}</div></div>
    <p class="helper-text">При изменении категории её название, иконка и цвет обновятся в связанных операциях и бюджетах.</p>`;
  showDialog(modalShell(existing ? "Редактирование категории" : "Новая категория", body, "Сохранить", "category-form"));
}

function openBudgetEditor() {
  const categories = categoriesFor("expense");
  const rows = categories.map((category, index) => {
    const budget = currentBudget(category.name);
    return `<div class="budget-edit-row">${categoryBadge(category, "small")}<strong>${escapeHTML(category.name)}</strong><label class="field"><input name="budget-${index}" inputmode="decimal" value="${escapeHTML(budget?.limit || "")}" placeholder="0"><input type="hidden" name="category-${index}" value="${escapeHTML(category.name)}"></label></div>`;
  }).join("");
  const body = `<p class="helper-text">Укажите месячный лимит. Пустое поле или ноль удаляет существующий бюджет.</p><div class="budget-edit-list">${rows}</div>`;
  showDialog(modalShell("Месячные лимиты", body, "Сохранить", "budget-form"));
}

function openBalanceAdjustment() {
  const current = totalBalance();
  const body = `<p class="helper-text">Расчётный баланс: <strong>${formatMoney(current)}</strong>. Корректировка не влияет на аналитику.</p>
    <label class="field"><span>Фактический баланс</span><input name="target" inputmode="decimal" required autofocus value="${escapeHTML(String(Math.round(current * 100) / 100))}"></label>
    <label class="field"><span>Комментарий</span><textarea name="note" maxlength="500" placeholder="Ручная корректировка баланса"></textarea></label>`;
  showDialog(modalShell("Корректировка баланса", body, "Скорректировать", "adjustment-form"));
}

async function submitTransaction(form) {
  const data = new FormData(form);
  const amount = Number(String(data.get("amount")).replace(",", "."));
  if (!Number.isFinite(amount) || amount <= 0) throw new Error("Введите сумму больше нуля.");
  const kind = data.get("kind") === "income" ? "income" : "expense";
  const category = categoriesFor(kind).find(item => item.name === data.get("category"));
  if (!category) throw new Error("Выберите категорию.");
  const existing = state.transactions.get(String(data.get("id"))) || null;
  const id = existing?.id || randomID();
  const value = {
    amount, date: dateInputISO(String(data.get("date"))), note: String(data.get("note") || "").trim(),
    categoryName: category.name, categoryIcon: category.icon, categoryEmoji: category.emoji || "",
    categoryColorName: category.colorName, isBalanceAdjustment: false, kindRawValue: kind,
  };
  await pushChanges([changeFor("transaction", id, transactionPayload(value))]);
  closeDialog();
  toast(existing ? "Операция обновлена" : "Операция добавлена");
}

async function submitCategory(form) {
  const data = new FormData(form);
  const existing = state.categories.get(String(data.get("id"))) || null;
  const name = String(data.get("name") || "").trim();
  const kind = String(existing?.kindRawValue || data.get("kind-fixed") || data.get("kind")) === "income" ? "income" : "expense";
  if (!name) throw new Error("Введите название категории.");
  const duplicate = allCategories().some(item => item.id !== existing?.id && item.kindRawValue === kind && item.name.localeCompare(name, "ru", { sensitivity: "accent" }) === 0);
  if (duplicate) throw new Error("Категория с таким названием уже существует.");
  const visual = data.get("visual") === "emoji" ? "emoji" : "icon";
  const emoji = visual === "emoji" ? firstGrapheme(data.get("emoji") || data.get("emoji-choice")) : "";
  if (visual === "emoji" && !emoji) throw new Error("Введите или выберите эмодзи.");
  const now = new Date().toISOString();
  const value = {
    name, icon: String(data.get("icon") || existing?.icon || "star.fill"), emoji,
    colorName: safeColor(String(data.get("color") || "indigo")), kindRawValue: kind,
    createdAt: existing?.createdAt || now,
  };
  const changes = [changeFor("category", existing?.id || randomID(), categoryPayload(value), false, now)];
  if (existing) {
    for (const transaction of allTransactions().filter(item => item.categoryName === existing.name && item.kindRawValue === kind)) {
      changes.push(changeFor("transaction", transaction.id, transactionPayload({
        ...transaction, categoryName: name, categoryIcon: value.icon, categoryEmoji: value.emoji, categoryColorName: value.colorName,
      }), false, now));
    }
    for (const budget of allBudgets().filter(item => item.categoryName === existing.name)) {
      changes.push(changeFor("budget", budget.id, budgetPayload({
        ...budget, categoryName: name, categoryIcon: value.icon, categoryEmoji: value.emoji,
      }), false, now));
    }
  }
  await pushChanges(changes);
  closeDialog();
  toast(existing ? "Категория обновлена" : "Категория добавлена");
}

async function submitBudgets(form) {
  const data = new FormData(form);
  const categories = categoriesFor("expense");
  const now = new Date().toISOString();
  const changes = [];
  categories.forEach((category, index) => {
    const amount = Number(String(data.get(`budget-${index}`) || "0").replace(",", "."));
    const existing = currentBudget(category.name);
    if (Number.isFinite(amount) && amount > 0) {
      changes.push(changeFor("budget", existing?.id || randomID(), budgetPayload({
        categoryName: category.name, categoryIcon: category.icon, categoryEmoji: category.emoji || "",
        limit: amount, monthStart: existing?.monthStart || currentMonthStartISO(),
      }), false, now));
    } else if (existing) {
      changes.push(changeFor("budget", existing.id, null, true, now));
    }
  });
  if (changes.length) await pushChanges(changes);
  closeDialog();
  toast("Бюджеты обновлены");
}

async function submitAdjustment(form) {
  const data = new FormData(form);
  const target = Number(String(data.get("target")).replace(",", "."));
  if (!Number.isFinite(target)) throw new Error("Введите фактический баланс.");
  const difference = target - totalBalance();
  if (Math.abs(difference) < 0.005) throw new Error("Баланс уже соответствует указанной сумме.");
  const value = {
    amount: Math.abs(difference), date: new Date().toISOString(),
    note: String(data.get("note") || "").trim() || "Ручная корректировка баланса",
    categoryName: "Корректировка", categoryIcon: "tune", categoryEmoji: "", categoryColorName: "indigo",
    isBalanceAdjustment: true, kindRawValue: difference > 0 ? "income" : "expense",
  };
  await pushChanges([changeFor("transaction", randomID(), transactionPayload(value))]);
  closeDialog();
  toast("Баланс скорректирован");
}

async function deleteTransaction(id) {
  const value = state.transactions.get(id);
  if (!value || !confirm(`Удалить операцию «${value.categoryName}» на сумму ${formatMoney(value.amount)}?`)) return;
  await pushChanges([changeFor("transaction", id, null, true)]);
  toast("Операция удалена");
}

async function deleteCategory(id) {
  const value = state.categories.get(id);
  if (!value || !confirm(`Удалить категорию «${value.name}»? Существующие операции сохранятся.`)) return;
  await pushChanges([changeFor("category", id, null, true)]);
  toast("Категория удалена");
}

function setRoute(route) {
  if (!["dashboard", "transactions", "analytics", "budgets", "categories", "settings"].includes(route)) return;
  state.route = route;
  render();
  $("#content")?.focus({ preventScroll: true });
  window.scrollTo({ top: 0, behavior: "smooth" });
}

function setSyncStatus(type, text) {
  const pill = $("#sync-pill");
  if (!pill) return;
  pill.classList.toggle("is-syncing", type === "syncing");
  pill.classList.toggle("is-error", type === "error");
  const label = $("span:last-child", pill);
  if (label) label.textContent = text;
}

function toast(message, error = false) {
  const item = document.createElement("div");
  item.className = `toast${error ? " is-error" : ""}`;
  item.textContent = message;
  $("#toast-region").append(item);
  window.setTimeout(() => item.remove(), 4200);
}

function showApp() {
  $("#auth-view").hidden = true;
  $("#app-view").hidden = false;
  $("#account-email").textContent = state.user.email;
  $("#account-avatar").textContent = (state.user.email || "Б")[0].toUpperCase();
  render();
}

function showAuth() {
  state.user = null;
  state.transactions.clear();
  state.categories.clear();
  state.budgets.clear();
  state.cursor = 0;
  $("#app-view").hidden = true;
  $("#auth-view").hidden = false;
  document.title = "Balance — вход";
}

async function authenticate(event) {
  event.preventDefault();
  const form = event.currentTarget;
  const button = $("#auth-submit");
  const error = $("#auth-error");
  error.hidden = true;
  button.disabled = true;
  button.textContent = state.authMode === "login" ? "Входим…" : "Создаём аккаунт…";
  try {
    await apiFetch(`/v1/auth/${state.authMode}`, {
      method: "POST",
      body: JSON.stringify({ email: form.email.value.trim(), password: form.password.value }),
    }, false);
    state.user = await apiFetch("/v1/me");
    showApp();
  } catch (failure) {
    error.textContent = failure.message || "Не удалось войти.";
    error.hidden = false;
    return;
  } finally {
    button.disabled = false;
    button.textContent = state.authMode === "login" ? "Войти" : "Создать аккаунт";
  }
  try {
    await fullSync();
  } catch (failure) {
    toast(failure.message || "Вход выполнен, но синхронизация пока недоступна.", true);
  }
}

function setAuthMode(mode) {
  state.authMode = mode === "register" ? "register" : "login";
  $$('[data-auth-mode]').forEach(button => button.classList.toggle("is-active", button.dataset.authMode === state.authMode));
  $("#auth-submit").textContent = state.authMode === "login" ? "Войти" : "Создать аккаунт";
  $("#auth-password").autocomplete = state.authMode === "login" ? "current-password" : "new-password";
  $("#auth-error").hidden = true;
}

async function logout() {
  if (!confirm("Выйти из аккаунта в этом браузере?")) return;
  try {
    await apiFetch("/v1/auth/logout", { method: "POST", body: JSON.stringify({ refreshToken: "" }) }, false);
  } catch {
    // The local session is cleared even if the server is temporarily unavailable.
  }
  showAuth();
}

async function handleAction(action, id) {
  try {
    if (action === "transaction-new") openTransactionEditor();
    else if (action === "transaction-edit") openTransactionEditor(id);
    else if (action === "transaction-delete") await deleteTransaction(id);
    else if (action === "category-new") openCategoryEditor();
    else if (action === "category-edit") openCategoryEditor(id);
    else if (action === "category-delete") await deleteCategory(id);
    else if (action === "budgets-edit") openBudgetEditor();
    else if (action === "balance-adjust") openBalanceAdjustment();
    else if (action === "sync") await pullChanges(false);
    else if (action === "logout") await logout();
    else if (action === "dialog-close") closeDialog();
  } catch (error) {
    toast(error.message || "Не удалось выполнить действие", true);
  }
}

document.addEventListener("click", event => {
  const route = event.target.closest("[data-route]");
  if (route) {
    event.preventDefault();
    setRoute(route.dataset.route);
    return;
  }
  const authMode = event.target.closest("[data-auth-mode]");
  if (authMode) {
    setAuthMode(authMode.dataset.authMode);
    return;
  }
  const analytics = event.target.closest("[data-analytics-months]");
  if (analytics) {
    state.analyticsMonths = Number(analytics.dataset.analyticsMonths) || 1;
    render();
    return;
  }
  const transactionKind = event.target.closest("[data-transaction-kind]");
  if (transactionKind) {
    const kind = transactionKind.dataset.transactionKind;
    $("#transaction-kind").value = kind;
    $$('[data-transaction-kind]').forEach(button => button.classList.toggle("is-active", button.dataset.transactionKind === kind));
    $("#transaction-category").innerHTML = buildCategoryOptions(kind, categoriesFor(kind)[0]?.name);
    return;
  }
  const categoryVisual = event.target.closest("[data-category-visual]");
  if (categoryVisual) {
    const visual = categoryVisual.dataset.categoryVisual;
    $("#category-visual").value = visual;
    $$('[data-category-visual]').forEach(button => button.classList.toggle("is-active", button.dataset.categoryVisual === visual));
    $("#icon-section").hidden = visual !== "icon";
    $("#emoji-section").hidden = visual !== "emoji";
    return;
  }
  const emojiChoice = event.target.closest('input[name="emoji-choice"]');
  if (emojiChoice && $("#category-emoji")) $("#category-emoji").value = emojiChoice.value;
  const action = event.target.closest("[data-action]");
  if (action) void handleAction(action.dataset.action, action.dataset.id);
});

document.addEventListener("input", event => {
  if (event.target.id === "transaction-search") {
    state.transactionQuery = event.target.value;
    const list = $("#transaction-list");
    if (list) list.innerHTML = transactionListHTML();
  }
  if (event.target.id === "category-emoji") {
    $$('input[name="emoji-choice"]').forEach(input => { input.checked = input.value === event.target.value; });
  }
});

document.addEventListener("change", event => {
  if (event.target.id === "transaction-filter") {
    state.transactionKind = event.target.value;
    const list = $("#transaction-list");
    if (list) list.innerHTML = transactionListHTML();
  } else if (event.target.id === "currency-setting") {
    preferences.currency = event.target.value;
    savePreferences();
    render();
  } else if (event.target.id === "theme-setting") {
    preferences.theme = event.target.value;
    savePreferences();
    applyTheme();
    render();
  } else if (event.target.name === "emoji-choice" && $("#category-emoji")) {
    $("#category-emoji").value = event.target.value;
  }
});

document.addEventListener("submit", event => {
  if (event.target.id === "auth-form") return;
  event.preventDefault();
  const form = event.target;
  const submit = $('button[type="submit"]', form);
  if (submit) submit.disabled = true;
  const actions = {
    "transaction-form": submitTransaction,
    "category-form": submitCategory,
    "budget-form": submitBudgets,
    "adjustment-form": submitAdjustment,
  };
  const action = actions[form.id];
  if (!action) return;
  void action(form).catch(error => {
    toast(error.message || "Проверьте введённые данные", true);
    if (submit) submit.disabled = false;
  });
});

$("#auth-form").addEventListener("submit", authenticate);
$("#editor-dialog").addEventListener("click", event => {
  if (event.target === event.currentTarget) closeDialog();
});
mediaDark.addEventListener("change", () => { if (preferences.theme === "system") applyTheme(); });
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && state.user && !state.syncing) void pullChanges(true).catch(() => {});
});
window.setInterval(() => {
  if (state.user && !document.hidden && !state.syncing) void pullChanges(true).catch(() => {});
}, 30_000);

async function boot() {
  applyTheme();
  if ("serviceWorker" in navigator) navigator.serviceWorker.register("/service-worker.js").catch(() => {});
  try {
    state.user = await apiFetch("/v1/me");
  } catch {
    showAuth();
    return;
  }
  showApp();
  try {
    await fullSync();
  } catch (failure) {
    toast(failure.message || "Не удалось загрузить данные с сервера.", true);
  }
}

void boot();
