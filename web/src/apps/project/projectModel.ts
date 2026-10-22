/*
 * Regras puras da janela Projeto (spec 093, T08): limites do
 * projeto e o resumo da janela.
 */
import { t, tp } from "../../i18n";
import type { AppSummary, AppSummaryInput } from "../registry";

export const LIMIT_MIN = 1;
export const LIMIT_MAX = 10;

/** Inteiro de 1 a 10, ou `null`. */
export function parseLimit(raw: string): number | null {
  if (!/^\d{1,2}$/.test(raw.trim())) return null;
  const n = Number(raw);
  return n >= LIMIT_MIN && n <= LIMIT_MAX ? n : null;
}

/** "aprovação automática · internet · 5 tentativas" (o que vale no projeto agora), no idioma ativo. */
export function projectSummary({ space }: AppSummaryInput): AppSummary {
  const state = space?.state ?? null;
  if (state === null) return {};
  const parts = [t(state.yolo === true ? "project.summary.yolo" : "project.summary.standard")];
  if (state.architect_internet === true) parts.push(t("project.summary.internet"));
  parts.push(tp("project.summary.attempts", state.max_attempts));
  return { subtitle: parts.join(" · ") };
}
