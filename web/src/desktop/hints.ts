/*
 * Dicas contextuais do primeiro uso (spec 093, decisão 15): no máximo duas, descartáveis,
 * lembradas na seção global `hints` do ui-state. Sem tutorial em sobreposição.
 */
import { localized } from "../i18n";

export type HintId = "snap" | "dock";

export const HINT_IDS: readonly HintId[] = ["snap", "dock"];

/** Texto de cada dica (no máximo duas). */
export const HINTS: Readonly<Record<HintId, string>> = localized({
  snap: "hint.snap",
  dock: "hint.dock",
});

export const MAX_HINTS = 2;

interface HintsState {
  /** Dicas já mostradas (não voltam). */
  seen: HintId[];
}

export function parseHints(value: unknown): HintsState {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return { seen: [] };
  const raw = (value as { seen?: unknown }).seen;
  if (!Array.isArray(raw)) return { seen: [] };
  const seen = HINT_IDS.filter((id) => raw.includes(id));
  return { seen };
}

/** A dica deve aparecer? Só se nunca foi vista e o limite não foi atingido. */
export function shouldShowHint(state: HintsState, id: HintId): boolean {
  return !state.seen.includes(id) && state.seen.length < MAX_HINTS;
}

/** Estado depois de mostrar a dica. */
export function markHintSeen(state: HintsState, id: HintId): HintsState {
  return state.seen.includes(id) ? state : { seen: [...state.seen, id] };
}
