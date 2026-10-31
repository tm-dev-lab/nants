/*
 * Modelo puro da janela do Nanty (spec 138 T04 e T10): o selo, o subtítulo vivo (o estado, os
 * projetos observados, o gasto do dia e a saúde do Telegram), as abas, as seções do feed e os
 * botões de cada cartão (os mesmos do Telegram).
 *
 * Selo sem duplicar o sino (C-07): conta só os cartões abertos que o "Precisa de você"
 * (`/api/attention`) não mostra. O cartão diz o tipo no grupo (`<projeto>:<tipo>`); o sino cobre o
 * plano esperando aprovação (`plan_awaiting_approval`) e a execução pausada ou parada
 * (`paused_or_stalled`) do mesmo projeto. Custo, disco, Docker e os marcos nunca estão no sino.
 */
import { formatNumber, formatUsd, localized, t, tp, type MessageKey } from "../../i18n";
import type { Attention } from "../../desktop/api";
import type { AppBadge } from "../../desktop/appState";
import type { WindowKind } from "../../desktop/windows";
import type { NantyMood } from "./NantyAvatar";
import type { NantyAnswer, NantyCard, NantyDone, NantyFeed, NantyHealth, NantyPlanned, NantyStatus } from "./nantyApi";

export const NANTY_TABS = ["feed", "rules", "settings"] as const;
export type NantyTab = (typeof NANTY_TABS)[number];

export const NANTY_TAB_LABELS: Readonly<Record<NantyTab, string>> = localized({
  feed: "nanty.tab.feed",
  rules: "nanty.tab.rules",
  settings: "nanty.tab.settings",
});

/** Nome do estado do avatar (subtítulo e nome acessível), no idioma ativo. */
export const NANTY_MOOD_LABELS: Readonly<Record<NantyMood, string>> = localized({
  attentive: "nanty.mood.attentive",
  speaking: "nanty.mood.speaking",
  thinking: "nanty.mood.thinking",
  sleeping: "nanty.mood.sleeping",
  waiting: "nanty.mood.waiting",
});

/** Tipos de aviso que o sino já mostra quando o projeto tem a pausa ou a parada. */
const PAUSE_KINDS: readonly string[] = ["no_credit", "provider_stopped", "run_blocked", "budget", "review_loop"];

/** Tipo do cartão: o que vem depois do último `:` do grupo. */
function cardKind(card: NantyCard): string {
  const at = card.group.lastIndexOf(":");
  return at === -1 ? card.group : card.group.slice(at + 1);
}

/** O sino ("Precisa de você") já mostra este cartão? */
export function coveredByAttention(card: NantyCard, attention: Attention | null): boolean {
  if (attention === null || card.project_id === null) return false;
  const project = attention.projects.find((p) => p.project_id === card.project_id);
  if (project === undefined) return false;
  const kind = cardKind(card);
  if (kind === "plan_waiting") return project.plan_awaiting_approval;
  if (PAUSE_KINDS.includes(kind)) return project.paused_or_stalled !== null;
  return false;
}

/** Cartões que só o Nanty mostra (o selo do menu do N). */
export function nantyBadgeCount(feed: NantyFeed | null, attention: Attention | null): number {
  if (feed === null) return 0;
  return feed.waiting.filter((card) => !coveredByAttention(card, attention)).length;
}

/** Estado do avatar: o do servidor; sem leitura ainda, atento. */
export function nantyMood(status: NantyStatus | null): NantyMood {
  return status?.avatar ?? "attentive";
}

interface NantySummaryInput {
  status: NantyStatus | null;
  feed: NantyFeed | null;
  attention: Attention | null;
}

const HEALTH_KEYS: Readonly<Record<NantyHealth, MessageKey>> = {
  off: "nanty.health.off",
  not_configured: "nanty.health.not_configured",
  connecting: "nanty.health.connecting",
  connected: "nanty.health.connected",
  invalid_token: "nanty.health.invalid_token",
  conflict: "nanty.health.conflict",
  network_error: "nanty.health.network_error",
  rate_limited: "nanty.health.rate_limited",
};

/** Saúde do Telegram em palavras; conectado sem dono pareado diz que falta o pareamento. */
function nantyHealthText(status: NantyStatus | null): string | null {
  if (status === null || status.telegram === undefined) return null;
  if (!status.telegram_configured) return t("nanty.health.not_configured");
  if (status.telegram === "connected" && !status.telegram_paired) return t("nanty.health.unpaired");
  return t(HEALTH_KEYS[status.telegram]);
}

/** O gasto do dia: em US$ com preço, senão em tokens. Sem modelo e sem gasto, nada. */
function nantySpendText(status: NantyStatus | null, feed: NantyFeed | null): string | null {
  const budget = status?.budget;
  if (budget !== undefined) {
    const spent = budget.mode === "usd" ? budget.spent_usd > 0 : budget.tokens > 0;
    if (status?.model_configured !== true && !spent) return null;
    return budget.mode === "usd"
      ? t("nanty.window.spend", { spent: formatUsd(budget.spent_usd), budget: formatUsd(budget.budget_usd) })
      : t("nanty.window.spendTokens", { spent: formatNumber(budget.tokens), budget: formatNumber(budget.budget_tokens) });
  }
  if (status?.model_configured === true && feed !== null && feed.today.budget_usd > 0) {
    return t("nanty.window.spend", { spent: formatUsd(feed.today.spend_usd), budget: formatUsd(feed.today.budget_usd) });
  }
  return null;
}

/** Subtítulo vivo e selo da janela (menu do N, lançador, alternador do celular e a barra de título). */
export function nantySummary({ status, feed, attention }: NantySummaryInput): { subtitle: string; badge?: AppBadge } {
  const parts = [status?.instance === "other_instance" ? t("nanty.window.otherInstance") : NANTY_MOOD_LABELS[nantyMood(status)]];
  if (status?.projects_observed !== undefined) parts.push(tp("nanty.window.projects", status.projects_observed));
  const spend = nantySpendText(status, feed);
  if (spend !== null) parts.push(spend);
  const health = nantyHealthText(status);
  if (health !== null) parts.push(health);
  const count = nantyBadgeCount(feed, attention);
  return {
    subtitle: parts.join(" · "),
    ...(count > 0 ? { badge: { count, label: tp("nanty.badge", count) } } : {}),
  };
}

/** Próxima aba pelas setas, Home e End (com volta), ou `null` para outra tecla. */
export function nantyTabByKey(current: NantyTab, key: string): NantyTab | null {
  const index = NANTY_TABS.indexOf(current);
  const at = (i: number): NantyTab => NANTY_TABS[(i + NANTY_TABS.length) % NANTY_TABS.length] ?? current;
  switch (key) {
    case "ArrowRight":
      return at(index + 1);
    case "ArrowLeft":
      return at(index - 1);
    case "Home":
      return at(0);
    case "End":
      return at(NANTY_TABS.length - 1);
    default:
      return null;
  }
}

// --- Feed ---

interface FeedSections {
  waiting: NantyCard[];
  /** O que o Nanty fez sozinho hoje, ou que ainda pode ser desfeito. */
  auto: NantyDone[];
  planned: NantyPlanned[];
  /** O resto do dia: as decisões da pessoa e os avisos resolvidos. */
  today: NantyDone[];
}

/** Início do dia local de `nowMs`. */
function dayStart(nowMs: number): number {
  const d = new Date(nowMs);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** O desfazer ainda vale? */
export function canUndo(done: NantyDone, nowMs: number): boolean {
  return done.undo !== null && done.undo.expires_at_ms > nowMs;
}

/** As seções do feed (`reference/Main.dc.html`): esperando você, feito sozinho, previsto e hoje. */
export function feedSections(feed: NantyFeed | null, nowMs: number): FeedSections {
  if (feed === null) return { waiting: [], auto: [], planned: [], today: [] };
  const start = dayStart(nowMs);
  const done = feed.done ?? [];
  const auto = done.filter((d) => d.decided_by === "auto" && (d.at_ms >= start || canUndo(d, nowMs)));
  const today = done.filter((d) => !auto.includes(d) && d.at_ms >= start);
  const planned = [...(feed.planned ?? [])].sort((a, b) => a.at_ms - b.at_ms);
  return { waiting: feed.waiting, auto, planned, today };
}

/** Ações que o Nanty executa (o botão da ação, como no Telegram). */
const EXECUTES: readonly string[] = ["approve_plan", "resume_run", "retry_provider", "swap_reserve", "clean_orphans", "pause_run"];
/** Ações que só abrem o Nants: na janela, abrem a janela do projeto. */
const LINKS: readonly string[] = ["open_nants", "raise_limit"];

export type CardChoice = NantyAnswer | "open";

interface CardButton {
  choice: CardChoice;
  label: string;
}

const ACTION_KEYS: Readonly<Record<string, MessageKey>> = {
  approve_plan: "nanty.action.approve_plan",
  resume_run: "nanty.action.resume_run",
  retry_provider: "nanty.action.retry_provider",
  swap_reserve: "nanty.action.swap_reserve",
  clean_orphans: "nanty.action.clean_orphans",
  pause_run: "nanty.action.pause_run",
  remind_later: "nanty.action.remind_later",
  open_nants: "nanty.action.open_nants",
  raise_limit: "nanty.action.raise_limit",
  allow_command_always: "nanty.action.allow_command_always",
  change_channel: "nanty.action.change_channel",
  undo: "nanty.action.undo",
};

/** Nome da ação do catálogo, pelo dicionário (`nanty.action.*`). */
function actionLabel(action: string): string {
  const key = ACTION_KEYS[action];
  return key === undefined ? t("nanty.title.unknown") : t(key);
}

/**
 * Os botões do cartão, os mesmos do Telegram: a ação oferecida (se a pessoa pode fazê-la por aqui),
 * "não" (com ação) e "lembrar depois". Sem token, nenhum. O link "abrir no Nants" abre a janela do
 * projeto; sem projeto, ele não aparece.
 */
export function cardButtons(card: NantyCard): CardButton[] {
  if (card.token === undefined) return [];
  const out: CardButton[] = [];
  const action = card.action;
  if (action !== undefined) {
    if (EXECUTES.includes(action)) out.push({ choice: "yes", label: actionLabel(action) });
    else if (LINKS.includes(action) && card.project_id !== null) out.push({ choice: "open", label: actionLabel(action) });
    out.push({ choice: "no", label: t("nanty.feed.no") });
  }
  out.push({ choice: "later", label: t("nanty.action.remind_later") });
  return out;
}

/** Janela que o link "abrir no Nants" abre, pelo tipo do aviso. */
export function linkWindow(card: NantyCard): WindowKind {
  const kind = cardKind(card);
  if (kind === "plan_waiting") return "plan";
  if (kind === "cost_alert" || kind === "budget" || card.action === "raise_limit") return "costs";
  return "execution";
}

const RESULT_KEYS: Readonly<Record<string, MessageKey>> = {
  ok: "nanty.result.ok",
  failed: "nanty.result.failed",
  expired: "nanty.result.expired",
  resolved: "nanty.result.resolved",
  undone: "nanty.result.undone",
  config_changed: "nanty.result.config_changed",
};

/** O resultado de um "feito" e quem decidiu ("Feito, pelo Nanty, sozinho"). */
export function doneWho(done: NantyDone): string {
  const result = t(RESULT_KEYS[done.result] ?? "nanty.result.other");
  if (done.decided_by === null) return result;
  const by = t(done.decided_by === "auto" ? "nanty.by.auto" : done.decided_by === "web" ? "nanty.by.web" : "nanty.by.telegram");
  return t("nanty.done.who", { result, by });
}

/**
 * Os fatos do cartão ou do "feito" (C-18: só contagens e códigos, nunca texto de projeto): a spec,
 * a tarefa, as tentativas, o papel, o código da parada, o espaço livre, o que a limpeza tirou e a
 * idade. Cada fato é um texto curto do dicionário.
 */
export function factTexts(params: Readonly<Record<string, string | number>>, roleLabel: (role: string) => string): string[] {
  const out: string[] = [];
  const text = (name: string): string | null => {
    const v = params[name];
    return typeof v === "string" && v !== "" ? v : null;
  };
  const n = (name: string): number | null => {
    const v = params[name];
    return typeof v === "number" && Number.isFinite(v) ? v : null;
  };
  const spec = text("spec_id");
  if (spec !== null) out.push(t("nanty.fact.spec", { spec }));
  const task = text("task_id");
  if (task !== null) out.push(t("nanty.fact.task", { task }));
  const attempts = n("attempts");
  if (attempts !== null && attempts > 0) out.push(tp("nanty.fact.attempts", attempts));
  const attempt = n("attempt");
  if (attempt !== null) out.push(t("nanty.fact.attempt", { attempt: formatNumber(attempt) }));
  const role = text("role");
  if (role !== null) out.push(t("nanty.fact.role", { role: roleLabel(role) }));
  const stop = text("stop");
  if (stop !== null) out.push(t("nanty.fact.stop", { code: stop }));
  const reason = text("reason");
  if (reason !== null) out.push(t("nanty.fact.reason", { code: reason }));
  const free = n("free_gb");
  if (free !== null) out.push(t("nanty.fact.freeGb", { gb: formatNumber(free) }));
  const projects = n("projects");
  if (projects !== null && projects > 0) out.push(tp("nanty.fact.projects", projects));
  const networks = n("networks");
  if (networks !== null && networks > 0) out.push(tp("nanty.fact.networks", networks));
  const age = n("age_min");
  if (age !== null && age > 0) {
    out.push(age < 60 ? t("nanty.fact.ageMin", { n: formatNumber(age) }) : t("nanty.fact.ageHours", { n: formatNumber(Math.floor(age / 60)) }));
  }
  return out;
}
