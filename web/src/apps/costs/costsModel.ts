/*
 * Regras puras da janela Custos (specs 031/039/092): texto
 * do custo, barras por papel e por tarefa, o ritmo do provedor (limitador da 089) e o resumo.
 * Spec 102: textos pelo dicionário no idioma ativo; números e US$ pelos formatadores da 101
 * (`usageFormat`). O aviso do limitador vem do servidor e aparece como veio.
 */
import type { NamedUsage, SpecBudget, UsageTotals } from "../../api/types";
import { t } from "../../i18n";
import { formatTokens, formatUsd } from "../../usageFormat";
import { tailLine } from "../activity/activityModel";
import { isPacing, stepText } from "../shared/activityText";
import type { ActivityEntry } from "../../store";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Custo de uma linha: "US$ 0,04" (com "~" quando o preço é estimado) ou "sem preço". */
export function costText(u: UsageTotals): string {
  if (u.cost_usd === null) return t("costs.noPrice");
  return `${formatUsd(u.cost_usd)}${u.cost_estimated === true ? " ~" : ""}`;
}

/** Custo total em destaque: "US$ 0,13" (ou "sem preço"). */
export function totalCostText(u: UsageTotals): string {
  if (u.cost_usd === null) return t("costs.noPrice");
  return formatUsd(u.cost_usd);
}

/** Tokens de uma linha: "820 mil entrada, 410 mil saída". */
export function tokensText(u: Pick<UsageTotals, "input" | "output">): string {
  return t("costs.tokensLine", { input: formatTokens(u.input), output: formatTokens(u.output) });
}

/** "1,2 mil tokens" no idioma ativo. */
export function tokensValue(n: number): string {
  return t("costs.tokens", { value: formatTokens(n) });
}

export interface CostBar {
  id: string;
  label: string;
  /** Texto do valor (custo, ou tokens sem preço). */
  value: string;
  /** Largura da barra em % do maior valor (0 a 100). */
  pct: number;
}

/**
 * Barras de um conjunto de linhas: pelo custo quando alguma tem preço; senão, pelos tokens
 * (entrada + saída). A maior ocupa 100%.
 */
export function costBars(rows: readonly NamedUsage[], label: (id: string) => string): CostBar[] {
  const priced = rows.some((r) => r.cost_usd !== null && r.cost_usd > 0);
  const amount = (r: NamedUsage): number => (priced ? (r.cost_usd ?? 0) : r.input + r.output);
  const max = rows.reduce((m, r) => Math.max(m, amount(r)), 0);
  return rows.map((r) => ({
    id: r.id,
    label: label(r.id),
    value: priced ? costText(r) : tokensValue(r.input + r.output),
    pct: max > 0 ? Math.round((amount(r) / max) * 100) : 0,
  }));
}

interface TopItem {
  id: string;
  label: string;
  /** Texto do valor ("US$ 0,24", ou tokens quando não há preço). */
  value: string;
}

/** Id do item "Imagens (N)" entre os maiores. */
const IMAGES_ITEM = "images";

/**
 * Os três maiores itens do custo (095/T05, como no mockup): os papéis e as imagens. O custo das
 * imagens já está no papel que as gerou; aqui ele sai do papel e vira o item "Imagens (N)", e a
 * soma continua igual ao total. Sem preço em nenhum item, ordena pelos tokens.
 */
export function topCostItems(totals: UsageTotals, byAgent: readonly NamedUsage[], label: (id: string) => string, limit = 3): TopItem[] {
  const priced = byAgent.some((r) => r.cost_usd !== null && r.cost_usd > 0) || (totals.images_usd ?? 0) > 0;
  const rows: (TopItem & { amount: number })[] = byAgent.map((r) => {
    if (!priced) {
      const tokens = r.input + r.output;
      return { id: r.id, label: label(r.id), amount: tokens, value: tokensValue(tokens) };
    }
    const usd = r.cost_usd === null ? 0 : Math.max(0, r.cost_usd - (r.images_usd ?? 0));
    return { id: r.id, label: label(r.id), amount: usd, value: r.cost_usd === null ? t("costs.noPrice") : `${formatUsd(usd)}${r.cost_estimated === true ? " ~" : ""}` };
  });
  const images = totals.images ?? 0;
  if (priced && images > 0) {
    const usd = totals.images_usd ?? 0;
    rows.push({ id: IMAGES_ITEM, label: t("costs.images", { count: images }), amount: usd, value: `${formatUsd(usd)}${totals.images_estimated === true ? " ~" : ""}` });
  }
  return rows
    .filter((r) => r.amount > 0)
    .sort((a, b) => b.amount - a.amount)
    .slice(0, limit)
    .map(({ id, label: name, value }) => ({ id, label: name, value }));
}

export interface BudgetProgress {
  /** O usado: "US$ 0,10" (ou em tokens, sem limite em US$). */
  used: string;
  /** O "de US$ X" ao lado do usado. */
  limit: string;
  /** Uso em % do limite, de 0 a 100 (a barra). */
  pct: number;
  /** Passou do limite. */
  over: boolean;
}

/**
 * Progresso do orçamento da spec, com a mesma conta do servidor (`budget_status`): a execução para
 * quando QUALQUER limite é atingido (`>=`). Com os dois limites (US$, se o custo da spec tem preço,
 * e tokens de entrada + saída), vale a maior razão, e o "de X" segue esse limite. Sem limite, `null`.
 */
export function budgetProgress(spec: UsageTotals | null, budget: SpecBudget | null): BudgetProgress | null {
  if (spec === null || budget === null) return null;
  const candidates: { used: number; max: number; fmt: (n: number) => string }[] = [];
  if (budget.max_usd !== undefined && spec.cost_usd !== null) candidates.push({ used: spec.cost_usd, max: budget.max_usd, fmt: formatUsd });
  if (budget.max_tokens !== undefined) candidates.push({ used: spec.input + spec.output, max: budget.max_tokens, fmt: tokensValue });
  if (candidates.length === 0) return null;
  const ratioOf = (c: { used: number; max: number }): number => (c.max > 0 ? c.used / c.max : Infinity);
  const over = candidates.some((c) => c.used >= c.max);
  const pick = candidates.reduce((best, c) => (ratioOf(c) > ratioOf(best) ? c : best));
  return { used: pick.fmt(pick.used), limit: t("costs.budget.of", { limit: pick.fmt(pick.max) }), pct: Math.min(100, Math.round(ratioOf(pick) * 100)), over };
}

/** Aviso atual do limitador do provedor, no idioma ativo (pelo `code` do servidor), ou `null`. */
export function pacingNotice(activities: readonly ActivityEntry[] | undefined): string | null {
  const hit = (activities ?? []).find((a) => isPacing(a));
  return hit === undefined ? null : tailLine(stepText(hit));
}

/** "Ritmo do provedor: …" a partir do aviso. */
export function pacingText(notice: string | null): string {
  return notice === null ? t("costs.pacingIdle") : t("costs.pacing", { notice });
}


/** Houve consumo (alguma chamada, token, busca ou imagem). */
/** Houve consumo cobrado em dólar (chave de API): chamadas, tokens, imagens ou buscas. */
export function hasApiConsumption(t: UsageTotals): boolean {
  return t.calls > 0 || t.input > 0 || t.output > 0 || (t.images ?? 0) > 0 || (t.web_search_calls ?? 0) > 0;
}

/** Chamadas pelo plano ChatGPT (spec 116), que ficam fora das contagens em dólar. */
function planCalls(t: UsageTotals): number {
  return t.chatgpt_plan?.calls ?? 0;
}

/** Houve algum consumo: em dólar ou pelo plano ChatGPT. */
export function hasConsumption(t: UsageTotals): boolean {
  return hasApiConsumption(t) || planCalls(t) > 0;
}

export function costsSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  if (space.running) return { subtitle: t("costs.subtitle.live") };
  return { subtitle: t("costs.subtitle.idle") };
}
