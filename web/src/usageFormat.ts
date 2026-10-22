/*
 * Formatação do uso (spec 031): tokens compactos e US$ em texto puro, no idioma ativo (spec 102:
 * números e moeda pelos formatadores da 101).
 */
import type { TaskUsage } from "./api/types";
import { formatNumber, formatUsd as formatUsdActive, t } from "./i18n";

/** Tempo ativo: "< 1 min", "12 min", "1 h 05 min". */
export function formatActive(secs: number): string {
  const minutes = Math.floor(secs / 60);
  if (minutes < 1) return "< 1 min";
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h ${String(minutes % 60).padStart(2, "0")} min`;
}

/**
 * Linha do cartão da tarefa (086/T06): "12 min · US$ 0,04"; sem preço, só o tempo; sem dados
 * (tarefa ausente do uso), `null`. O custo segue o formato do indicador ("~" = preço estimado).
 */
export function taskUsageLine(t: TaskUsage | undefined): string | null {
  if (t === undefined) return null;
  const time = formatActive(typeof t.active_secs === "number" && Number.isFinite(t.active_secs) ? t.active_secs : 0);
  if (t.cost_usd === null || !Number.isFinite(t.cost_usd)) return time;
  return `${time} · ${formatUsd(t.cost_usd)}${t.cost_estimated === true ? " ~" : ""}`;
}

/** Uma casa decimal no idioma ativo ("1,2" em pt-BR, "1.2" em en). */
function oneDecimal(value: number): string {
  return formatNumber(value, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
}

/** 1234 -> "1,2 mil" (en: "1.2K"); 1_200_000 -> "1,2M" (en: "1.2M"); abaixo de mil, o número inteiro. */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return t("usage.tokens.millions", { value: oneDecimal(n / 1_000_000) });
  if (n >= 1_000) return t("usage.tokens.thousands", { value: oneDecimal(n / 1_000) });
  return String(n);
}

/** US$ no idioma ativo; valores pequenos (modelos baratos) não viram "US$ 0,00": mostram 4 casas. */
export function formatUsd(v: number): string {
  return formatUsdActive(v);
}
