/*
 * Regras puras da janela Configurações (spec 093, T08): abas, o que falta por aba (prontidão), o
 * resumo da janela e as regras do editor de preços (com o "US$ por imagem" da 092).
 */
import { formatNumber, localized, t, tp } from "../../i18n";
import type { Agents, ConnectionView, ModelPricing } from "../../api/types";
import { isNativeKind, NATIVE_PROVIDERS, ROLE_INFO, ROLE_ORDER, wireApiCardLabel, wireApiOf } from "../../settings/rules";
import type { IconName } from "../../desktop/Icon";
import { requiredMissing, SETTINGS_WINDOW_TABS, type Readiness, type ReadinessItem, type SettingsWindowTab } from "../../desktop/api";
import type { Tone } from "../../desktop/theme/tokens";
import type { AppSummary, AppSummaryInput } from "../registry";

export { SETTINGS_WINDOW_TABS };

export const SETTINGS_TAB_LABELS: Readonly<Record<SettingsWindowTab, string>> = localized({
  connections: "settings.tab.connections",
  agents: "settings.tab.agents",
  pricing: "settings.tab.pricing",
  machine: "settings.tab.machine",
  appearance: "settings.tab.appearance",
  projects: "settings.tab.projects",
});

export const SETTINGS_TAB_ICONS: Readonly<Record<SettingsWindowTab, IconName>> = {
  connections: "key",
  agents: "users",
  pricing: "tag",
  machine: "cpu",
  appearance: "palette",
  projects: "folder",
};

export const DEFAULT_SETTINGS_TAB: SettingsWindowTab = "connections";

/** Aba vizinha pelo teclado da lista de abas (setas circulam; Home/End vão às pontas), ou `null`. */
export function tabByKey(current: SettingsWindowTab, key: string): SettingsWindowTab | null {
  const tabs = SETTINGS_WINDOW_TABS;
  const index = tabs.indexOf(current);
  const at = (i: number): SettingsWindowTab => tabs[(i + tabs.length) % tabs.length] ?? current;
  switch (key) {
    case "ArrowDown":
    case "ArrowRight":
      return at(index + 1);
    case "ArrowUp":
    case "ArrowLeft":
      return at(index - 1);
    case "Home":
      return at(0);
    case "End":
      return at(tabs.length - 1);
    default:
      return null;
  }
}

/** Pendências da prontidão (obrigatórias e recomendadas, sem repetição) que levam a esta aba. */
export function tabMissing(readiness: Readiness | null, tab: SettingsWindowTab): ReadinessItem[] {
  if (readiness === null) return [];
  const seen = new Set<string>();
  const out: ReadinessItem[] = [];
  for (const item of [...requiredMissing(readiness), ...readiness.recommended.missing]) {
    if (seen.has(item.id) || item.settings_tab !== tab) continue;
    seen.add(item.id);
    out.push(item);
  }
  return out;
}

export function settingsSummary({ readiness }: AppSummaryInput): AppSummary {
  if (readiness === null) return {};
  const required = requiredMissing(readiness).length;
  if (required > 0) return { subtitle: tp("settings.summary.pending", required) };
  if (readiness.recommended.missing.length > 0) return { subtitle: t("settings.summary.readyPricing") };
  return { subtitle: t("settings.summary.ready") };
}

// --- Conexões (095/T06: lista com ponto, nome, detalhe e "usada por") ---

/** Papéis que usam a conexão, na ordem Arquiteto, Desenvolvedor, Revisor (nomes no idioma ativo). */
export function connectionUsers(agents: Agents | null, id: string): string[] {
  if (agents === null) return [];
  return ROLE_ORDER.filter((role) => agents[role].connection_id === id).map((role) => ROLE_INFO[role].label);
}

/** "usada por Arquiteto, Revisor" ou "sem uso", no idioma ativo. */
export function connectionUsageText(users: readonly string[]): string {
  return users.length === 0 ? t("settings.connection.unused") : t("settings.connection.usedBy", { roles: users.join(", ") });
}

/** Endereço curto da conexão (só o host; a URL inválida volta inteira). */
export function connectionHost(baseUrl: string): string {
  try {
    const host = new URL(baseUrl).host;
    return host === "" ? baseUrl : host;
  } catch {
    return baseUrl;
  }
}

/**
 * Detalhe da linha: host · protocolo · chave mascarada (ou a referência env:). Nas conexões nativas
 * (spec 132), o host é o endereço efetivo (o avançado, quando houver) e, no lugar do protocolo, o
 * provedor.
 */
export function connectionDetail(connection: ConnectionView): string {
  const native = isNativeKind(connection.kind) ? connection.kind : null;
  return t("settings.connection.detail", {
    host: connectionHost(native !== null ? (connection.base_url_override ?? connection.base_url) : connection.base_url),
    protocol: native !== null ? NATIVE_PROVIDERS[native].brand : wireApiCardLabel(wireApiOf(connection)),
    key: connection.api_key_masked,
  });
}

// --- Preços ---

export interface PriceRow {
  input: string;
  output: string;
  cached: string;
  /** US$ por imagem (092); vazio = estimativa padrão por qualidade. */
  image: string;
  source: "manual" | "estimated";
  estimatedAt?: string;
  /**
   * US$ por 1M de tokens gravados no cache (spec 132): o campo opcional "escrita de cache" (T10);
   * vazio ou ausente = o servidor cobra a escrita pelo preço de entrada.
   */
  cacheWrite?: string;
}

export type PriceRows = Record<string, PriceRow>;

export const EMPTY_PRICE_ROW: PriceRow = { input: "", output: "", cached: "", image: "", source: "manual" };

/**
 * Número com o separador decimal do idioma ativo (vírgula em pt-BR, ponto em en) pelo
 * `Intl.NumberFormat` (`formatNumber`, spec 115 T04), até 6 algarismos significativos, sem cauda de
 * float e sem separador de milhar (o campo aceita os dois separadores decimais, `parsePrice`).
 */
export function formatPrice(v: number): string {
  return formatNumber(v, { maximumSignificantDigits: 6, useGrouping: false });
}

export function priceRowOf(p: ModelPricing): PriceRow {
  return {
    input: formatPrice(p.input_per_mtok),
    output: formatPrice(p.output_per_mtok),
    cached: p.cached_input_per_mtok === undefined ? "" : formatPrice(p.cached_input_per_mtok),
    image: p.image_per_unit === undefined ? "" : formatPrice(p.image_per_unit),
    source: p.source === "estimated" ? "estimated" : "manual",
    ...(p.estimated_at !== undefined ? { estimatedAt: p.estimated_at } : {}),
    cacheWrite: p.cache_write_per_mtok === undefined ? "" : formatPrice(p.cache_write_per_mtok),
  };
}

export function priceRowsOf(pricing: Record<string, ModelPricing> | undefined): PriceRows {
  const out: PriceRows = {};
  for (const [model, p] of Object.entries(pricing ?? {})) out[model] = priceRowOf(p);
  return out;
}

/** Valor digitado (vírgula ou ponto) maior ou igual a zero, ou `null`. */
export function parsePrice(text: string): number | null {
  const v = Number(text.trim().replace(",", "."));
  return text.trim() !== "" && Number.isFinite(v) && v >= 0 ? v : null;
}

export function hasPrice(row: PriceRow | undefined): boolean {
  return row !== undefined && (row.input.trim() !== "" || row.output.trim() !== "");
}

/** Selo do modelo: sem preço, estimado (com a data) ou manual. */
export function priceBadge(row: PriceRow | undefined): { text: string; tone: Tone } {
  if (row === undefined || !hasPrice(row)) return { text: t("settings.pricing.badge.none"), tone: "muted" };
  if (row.source === "estimated") {
    const text = row.estimatedAt !== undefined ? t("settings.pricing.badge.estimatedAt", { date: row.estimatedAt }) : t("settings.pricing.badge.estimated");
    return { text, tone: "amber" };
  }
  return { text: t("settings.pricing.badge.manual"), tone: "violet" };
}

type PricingBuild = { ok: true; pricing: Record<string, ModelPricing> } | { ok: false; model: string };

/** Mapa a salvar: linhas sem entrada e saída ficam de fora; valor inválido aponta o modelo. */
export function buildPricing(models: readonly string[], rows: PriceRows): PricingBuild {
  const out: Record<string, ModelPricing> = {};
  for (const model of models) {
    const row = rows[model];
    if (row === undefined || !hasPrice(row)) continue;
    const input = parsePrice(row.input);
    const output = parsePrice(row.output);
    const cached = row.cached.trim() === "" ? undefined : parsePrice(row.cached);
    const image = row.image.trim() === "" ? undefined : parsePrice(row.image);
    const writeText = row.cacheWrite ?? "";
    const cacheWrite = writeText.trim() === "" ? undefined : parsePrice(writeText);
    if (input === null || output === null || cached === null || image === null || cacheWrite === null) return { ok: false, model };
    const entry: ModelPricing = { input_per_mtok: input, output_per_mtok: output, source: row.source };
    if (cached !== undefined) entry.cached_input_per_mtok = cached;
    if (image !== undefined) entry.image_per_unit = image;
    if (cacheWrite !== undefined) entry.cache_write_per_mtok = cacheWrite;
    if (row.source === "estimated" && row.estimatedAt !== undefined) entry.estimated_at = row.estimatedAt;
    out[model] = entry;
  }
  return { ok: true, pricing: out };
}

/** Mensagem do preço inválido. */
export function invalidPriceMessage(model: string): string {
  return t("settings.pricing.invalid", { model });
}
