/*
 * Modelo puro da aba Ajustes do Nanty (spec 138 T11): o rascunho editável, o pedido com só o que
 * mudou (o servidor também só grava o que difere do padrão), o estado da voz, o orçamento do dia e
 * o tempo da janela de pareamento.
 *
 * Os números ficam como texto no rascunho (o que a pessoa digitou); o pedido só sai com todos
 * válidos, nas mesmas faixas do servidor (`server::nanty::settings`). O servidor segue a autoridade.
 */
import type { ApiFailure, ApiResult } from "../../api/client";
import type { ConnectionKind, ConnectionView, Effort } from "../../api/types";
import type { NantyLanguage, NantyRole, NantyRoute, NantySettings, NantySettingsPatch, NantyVoiceOption, NantyVoices } from "./nantyApi";
import { NANTY_ROLES } from "./nantyApi";

/** Faixas do servidor (`server::nanty::settings`). */
export const NANTY_LIMITS = {
  budgetUsd: { min: 0, max: 1000 },
  budgetTokens: { min: 0, max: 100_000_000 },
  voiceInputSecs: { min: 5, max: 1800 },
  visibleHoldSecs: { min: 0, max: 3600 },
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
  /** Idioma próprio do Nanty (spec 139 T05). */
  language: NantyLanguage;
  /** Projetos fora da observação. */
  excluded: string[];
  reserve: Record<NantyRole, NantyRoute>;
  voice_enabled: boolean;
  /** A conexão do Gemini da voz; vazia = automática. */
  voice_connection_id: string;
  speech_model: string;
  voice_name: string;
  transcribe_model: string;
  max_input_secs: string;
  hold_while_visible: boolean;
  visible_hold_secs: string;
  /** O endereço do Nants que o celular alcança (spec 140 T03); o servidor o confere ao gravar. */
  address: string;
  /** O cenário proativo nos avisos importantes (spec 140 T12). */
  proactive: boolean;
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
    language: s.language,
    excluded: [...s.excluded_projects],
    reserve,
    voice_enabled: s.voice.enabled,
    voice_connection_id: s.voice.connection_id,
    speech_model: s.voice.speech_model,
    voice_name: s.voice.voice,
    transcribe_model: s.voice.transcribe_model,
    max_input_secs: String(s.voice.max_input_secs),
    hold_while_visible: s.telegram.hold_while_visible,
    visible_hold_secs: String(s.telegram.visible_hold_secs),
    address: s.address,
    proactive: s.analysis.proactive,
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
  if (d.language !== base.language) patch.language = d.language;
  const excluded = [...d.excluded].sort();
  if (!sameList(excluded, [...base.excluded].sort())) patch.excluded_projects = excluded;
  if (NANTY_ROLES.some((role) => !sameRoute(d.reserve[role], base.reserve[role]))) {
    const reserve: Partial<Record<NantyRole, NantyRoute>> = {};
    for (const role of NANTY_ROLES) reserve[role] = trimRoute(d.reserve[role]);
    patch.reserve = reserve;
  }
  const voice: NonNullable<NantySettingsPatch["voice"]> = {};
  if (d.voice_enabled !== base.voice_enabled) voice.enabled = d.voice_enabled;
  if (d.voice_connection_id !== base.voice_connection_id) voice.connection_id = d.voice_connection_id;
  if (d.speech_model.trim() !== base.speech_model) voice.speech_model = d.speech_model.trim();
  if (d.voice_name.trim() !== base.voice_name) voice.voice = d.voice_name.trim();
  if (d.transcribe_model.trim() !== base.transcribe_model) voice.transcribe_model = d.transcribe_model.trim();
  const maxInput = Number(d.max_input_secs.trim());
  if (maxInput !== saved.voice.max_input_secs) voice.max_input_secs = maxInput;
  if (Object.keys(voice).length > 0) patch.voice = voice;
  const telegram: NonNullable<NantySettingsPatch["telegram"]> = {};
  if (d.hold_while_visible !== base.hold_while_visible) telegram.hold_while_visible = d.hold_while_visible;
  const hold = Number(d.visible_hold_secs.trim());
  if (hold !== saved.telegram.visible_hold_secs) telegram.visible_hold_secs = hold;
  if (Object.keys(telegram).length > 0) patch.telegram = telegram;
  if (d.address.trim() !== base.address) patch.address = d.address.trim();
  if (d.proactive !== base.proactive) patch.analysis = { proactive: d.proactive };
  return patch;
}

/**
 * O provedor que recebe o texto do cenário (spec 140 T12, C-12): a marca das conexões nativas, o
 * plano ChatGPT ou, na conexão por chave, o endereço dela (o nome do servidor). `null` = o endereço
 * não se lê: a tela diz "endereço próprio".
 */
export function providerOf(c: ConnectionView): { kind: "brand"; name: string } | { kind: "plan" } | { kind: "host"; name: string } | null {
  if (c.kind === "google_gemini") return { kind: "brand", name: "Google Gemini" };
  if (c.kind === "anthropic") return { kind: "brand", name: "Anthropic" };
  if (c.kind === "chatgpt_plan") return { kind: "plan" };
  try {
    return { kind: "host", name: new URL(c.base_url).hostname };
  } catch {
    return null;
  }
}

/**
 * A conexão pode estar no plano gratuito da Gemini API, que usa o conteúdo para melhorar os produtos
 * (C-12). O plano da chave não se lê pela conexão: o aviso vale para toda conexão do Gemini, a nativa
 * e a compatível com a OpenAI.
 */
export function maybeFreeGemini(c: ConnectionView): boolean {
  if (c.kind === "google_gemini") return true;
  try {
    return new URL(c.base_url).hostname === "generativelanguage.googleapis.com";
  } catch {
    return false;
  }
}

/** A conexão serve para a voz? Só o Google Gemini, que fala e transcreve (`voice_capable` do servidor). */
export function voiceCapable(kind: ConnectionKind | undefined): boolean {
  return kind === "google_gemini";
}

/**
 * O aviso da conexão gravada na voz: `gone` (não existe mais), `unusable` (não é do Gemini) ou
 * `null` (vazia ou certa).
 */
export function voiceWarning(id: string, connections: readonly ConnectionView[]): "gone" | "unusable" | null {
  if (id === "") return null;
  const conn = connections.find((c) => c.id === id);
  if (conn === undefined) return "gone";
  return voiceCapable(conn.kind) ? null : "unusable";
}

/** A resposta de `GET /api/nanty/voices`, na forma do cartão Voz. */
export type VoiceCheck =
  | { status: "loading" }
  | { status: "ready"; connection_id: string; voices: NantyVoiceOption[]; speech_models: { id: string }[] }
  | { status: "no_key" }
  | { status: "no_connection" }
  /** A lista não veio: `connection_id` e `reason` (o código do motivo), quando o provedor é que falhou. */
  | { status: "failed"; connection_id: string | null; reason: string | null; error: ApiFailure };

export function voiceCheckOf(result: ApiResult<NantyVoices>): VoiceCheck {
  if (result.ok) return { status: "ready", connection_id: result.data.connection_id, voices: result.data.voices, speech_models: result.data.speech_models };
  const { error } = result;
  if (error.code === "nanty_voice_no_key") return { status: "no_key" };
  if (error.code === "nanty_voice_no_connection") return { status: "no_connection" };
  const params = error.code === "nanty_voice_list_failed" ? error.params : undefined;
  const textOf = (value: unknown): string | null => (typeof value === "string" ? value : null);
  return { status: "failed", connection_id: textOf(params?.connection_id), reason: textOf(params?.reason), error };
}

export type VoiceState = "ready" | "no_key" | "key_refused" | "no_connection";

/** O provedor recusou a chave: repetir não resolve, e toda fala cairia em texto. */
export function keyRefused(check: VoiceCheck): boolean {
  return check.status === "failed" && check.reason === "auth";
}

/**
 * O estado que a tela mostra, sempre o do servidor. Uma falha passageira do provedor na lista não
 * muda a regra (a conexão existe e tem chave), e por isso continua pronta; a chave recusada não está
 * pronta; sem resposta do servidor, nada a afirmar.
 */
export function voiceState(check: VoiceCheck): VoiceState | null {
  if (check.status === "loading") return null;
  if (keyRefused(check)) return "key_refused";
  if (check.status === "failed") return check.connection_id === null ? null : "ready";
  return check.status;
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
