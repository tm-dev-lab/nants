/*
 * Modelo puro da aba Ajustes do Nanty (spec 138 T11): o rascunho editável, o pedido com só o que
 * mudou (o servidor também só grava o que difere do padrão), a ordem da voz, o orçamento do dia e
 * o tempo da janela de pareamento.
 *
 * Os números ficam como texto no rascunho (o que a pessoa digitou); o pedido só sai com todos
 * válidos, nas mesmas faixas do servidor (`server::nanty::settings`). O servidor segue a autoridade.
 */
import type { ConnectionKind, ConnectionView, Effort } from "../../api/types";
import type { NantyRole, NantyRoute, NantySettings, NantySettingsPatch, NantyVoiceRoute } from "./nantyApi";
import { NANTY_ROLES } from "./nantyApi";

/** Faixas do servidor (`server::nanty::settings`). */
export const NANTY_LIMITS = {
  budgetUsd: { min: 0, max: 1000 },
  budgetTokens: { min: 0, max: 100_000_000 },
  voiceInputSecs: { min: 5, max: 1800 },
  visibleHoldSecs: { min: 0, max: 3600 },
  voiceSteps: 5,
} as const;

export type NantyNumberField = "daily_budget_usd" | "daily_budget_tokens" | "max_input_secs" | "visible_hold_secs";

export interface NantyDraft {
  connection_id: string;
  model: string;
  effort: Effort;
  daily_budget_usd: string;
  daily_budget_tokens: string;
  quiet_start: string;
  quiet_end: string;
  digest_time: string;
  digest_empty: "short" | "skip";
  /** Projetos fora da observação. */
  excluded: string[];
  reserve: Record<NantyRole, NantyRoute>;
  voice_enabled: boolean;
  voice_order: NantyVoiceRoute[];
  transcribe: NantyRoute;
  max_input_secs: string;
  hold_while_visible: boolean;
  visible_hold_secs: string;
}

const EMPTY_ROUTE: NantyRoute = { connection_id: "", model: "" };

export function draftOf(s: NantySettings): NantyDraft {
  const reserve = {} as Record<NantyRole, NantyRoute>;
  for (const role of NANTY_ROLES) reserve[role] = { ...(s.reserve[role] ?? EMPTY_ROUTE) };
  return {
    connection_id: s.connection_id,
    model: s.model,
    effort: s.effort,
    daily_budget_usd: String(s.daily_budget_usd),
    daily_budget_tokens: String(s.daily_budget_tokens),
    quiet_start: s.quiet_start,
    quiet_end: s.quiet_end,
    digest_time: s.digest_time,
    digest_empty: s.digest_empty,
    excluded: [...s.excluded_projects],
    reserve,
    voice_enabled: s.voice.enabled,
    voice_order: s.voice.order.map((r) => ({ ...r })),
    transcribe: { ...s.voice.transcribe },
    max_input_secs: String(s.voice.max_input_secs),
    hold_while_visible: s.telegram.hold_while_visible,
    visible_hold_secs: String(s.telegram.visible_hold_secs),
  };
}

/** Número do campo, na faixa; `null` fora dela ou se não é número. */
function numberIn(raw: string, min: number, max: number, integer: boolean): number | null {
  const text = raw.trim().replace(",", ".");
  if (text === "" || !/^\d+(\.\d+)?$/.test(text)) return null;
  const n = Number(text);
  if (!Number.isFinite(n) || n < min || n > max || (integer && !Number.isInteger(n))) return null;
  return n;
}

/** Os campos numéricos inválidos do rascunho (vazio = tudo certo). */
export function invalidNumbers(d: NantyDraft): NantyNumberField[] {
  const out: NantyNumberField[] = [];
  if (numberIn(d.daily_budget_usd, NANTY_LIMITS.budgetUsd.min, NANTY_LIMITS.budgetUsd.max, false) === null) out.push("daily_budget_usd");
  if (numberIn(d.daily_budget_tokens, NANTY_LIMITS.budgetTokens.min, NANTY_LIMITS.budgetTokens.max, true) === null) out.push("daily_budget_tokens");
  if (numberIn(d.max_input_secs, NANTY_LIMITS.voiceInputSecs.min, NANTY_LIMITS.voiceInputSecs.max, true) === null) out.push("max_input_secs");
  if (numberIn(d.visible_hold_secs, NANTY_LIMITS.visibleHoldSecs.min, NANTY_LIMITS.visibleHoldSecs.max, true) === null) out.push("visible_hold_secs");
  return out;
}

const sameRoute = (a: NantyRoute, b: NantyRoute): boolean => a.connection_id === b.connection_id && a.model.trim() === b.model.trim();
const sameList = (a: readonly string[], b: readonly string[]): boolean => a.length === b.length && a.every((v, i) => v === b[i]);
const sameVoiceOrder = (a: readonly NantyVoiceRoute[], b: readonly NantyVoiceRoute[]): boolean =>
  a.length === b.length && a.every((r, i) => b[i] !== undefined && sameRoute(r, b[i]) && r.voice === b[i].voice);
const trimRoute = (r: NantyRoute): NantyRoute => ({ connection_id: r.connection_id, model: r.model.trim() });

/**
 * O pedido com só o que mudou do salvo para o rascunho, ou `null` se um número está fora da faixa.
 * Pedido vazio (`{}`) = nada mudou.
 */
export function settingsPatch(saved: NantySettings, d: NantyDraft): NantySettingsPatch | null {
  if (invalidNumbers(d).length > 0) return null;
  const base = draftOf(saved);
  const patch: NantySettingsPatch = {};
  if (d.connection_id !== base.connection_id) patch.connection_id = d.connection_id;
  if (d.model.trim() !== base.model) patch.model = d.model.trim();
  if (d.effort !== base.effort) patch.effort = d.effort;
  const usd = Number(d.daily_budget_usd.trim().replace(",", "."));
  if (usd !== saved.daily_budget_usd) patch.daily_budget_usd = usd;
  const tokens = Number(d.daily_budget_tokens.trim());
  if (tokens !== saved.daily_budget_tokens) patch.daily_budget_tokens = tokens;
  if (d.quiet_start !== base.quiet_start) patch.quiet_start = d.quiet_start;
  if (d.quiet_end !== base.quiet_end) patch.quiet_end = d.quiet_end;
  if (d.digest_time !== base.digest_time) patch.digest_time = d.digest_time;
  if (d.digest_empty !== base.digest_empty) patch.digest_empty = d.digest_empty;
  const excluded = [...d.excluded].sort();
  if (!sameList(excluded, [...base.excluded].sort())) patch.excluded_projects = excluded;
  if (NANTY_ROLES.some((role) => !sameRoute(d.reserve[role], base.reserve[role]))) {
    const reserve: Partial<Record<NantyRole, NantyRoute>> = {};
    for (const role of NANTY_ROLES) reserve[role] = trimRoute(d.reserve[role]);
    patch.reserve = reserve;
  }
  const voice: NonNullable<NantySettingsPatch["voice"]> = {};
  if (d.voice_enabled !== base.voice_enabled) voice.enabled = d.voice_enabled;
  if (!sameVoiceOrder(d.voice_order, base.voice_order)) voice.order = d.voice_order.map((r) => ({ ...trimRoute(r), voice: r.voice }));
  if (!sameRoute(d.transcribe, base.transcribe)) voice.transcribe = trimRoute(d.transcribe);
  const maxInput = Number(d.max_input_secs.trim());
  if (maxInput !== saved.voice.max_input_secs) voice.max_input_secs = maxInput;
  if (Object.keys(voice).length > 0) patch.voice = voice;
  const telegram: NonNullable<NantySettingsPatch["telegram"]> = {};
  if (d.hold_while_visible !== base.hold_while_visible) telegram.hold_while_visible = d.hold_while_visible;
  const hold = Number(d.visible_hold_secs.trim());
  if (hold !== saved.telegram.visible_hold_secs) telegram.visible_hold_secs = hold;
  if (Object.keys(telegram).length > 0) patch.telegram = telegram;
  return patch;
}

/** Move o passo `index` da voz uma posição (`-1` sobe, `1` desce); fora da lista, igual. */
export function moveStep<T>(list: readonly T[], index: number, delta: -1 | 1): T[] {
  const to = index + delta;
  if (index < 0 || index >= list.length || to < 0 || to >= list.length) return [...list];
  const out = [...list];
  const [item] = out.splice(index, 1);
  if (item !== undefined) out.splice(to, 0, item);
  return out;
}

/**
 * A conexão serve para a fala? Só o Google Gemini e as por chave (a fala da OpenAI): a mesma regra
 * do servidor (`voice::speaks`). Sem `kind` = por chave.
 */
export function speaks(kind: ConnectionKind | undefined): boolean {
  const k = kind ?? "api_key";
  return k === "google_gemini" || k === "api_key";
}

/** A conexão serve para a transcrição? Só o Google Gemini (`voice::transcribes` do servidor). */
export function transcribes(kind: ConnectionKind | undefined): boolean {
  return kind === "google_gemini";
}

/**
 * O aviso de uma conexão gravada na voz: `gone` (não existe mais), `unusable` (o tipo não serve
 * para o caminho) ou `null` (vazia ou certa).
 */
export function voiceWarning(id: string, connections: readonly ConnectionView[], usable: (kind: ConnectionKind | undefined) => boolean): "gone" | "unusable" | null {
  if (id === "") return null;
  const conn = connections.find((c) => c.id === id);
  if (conn === undefined) return "gone";
  return usable(conn.kind) ? null : "unusable";
}

/** Fração gasta do orçamento do dia (0 a 1), em US$ ou em tokens. */
export function budgetFraction(spent: number, budget: number): number {
  if (budget <= 0) return spent > 0 ? 1 : 0;
  return Math.min(1, Math.max(0, spent / budget));
}

/** Segundos como `m:ss` (a validade do código de pareamento). */
export function minutesText(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  return `${String(Math.floor(s / 60))}:${String(s % 60).padStart(2, "0")}`;
}
