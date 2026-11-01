/*
 * Aba Ajustes do Nanty (spec 138 T11, `reference/Settings.dc.html`): cartões em grade (duas colunas
 * na janela larga, uma na estreita e no celular).
 *
 * - **Modelo e esforço**, pelas conexões que já existem, com o combobox de modelos dos Agentes;
 * - **Orçamento do dia**, com a barra do gasto e o aviso "sem preço, o orçamento conta tokens";
 * - **Telegram** (`NantyTelegramCard`), com a espera com a aba aberta;
 * - **Voz** (spec 139 T09): ligar, uma conexão do Gemini para falar e transcrever, os modelos e a voz
 *   à vista e o estado que o servidor diz (pronta, sem chave, sem conexão);
 * - **Horários:** silêncio e resumo da manhã, com a hora do Nants agora (C-15, lida do servidor) e
 *   os horários no formato da tela;
 * - **Idioma** (spec 139 T05): o seletor do idioma próprio do Nanty para os avisos e o resumo,
 *   seguir a conversa (o padrão), português ou inglês; a conversa responde no idioma de quem falou;
 * - **Reserva** por papel e **projetos** observados.
 *
 * Um só "Salvar" no fim manda só o que mudou (`PUT /api/nanty/config`); o erro sai com o código
 * traduzido. As ações do Telegram (token, parear, teste, desparear) gravam na hora, pelas rotas do
 * canal.
 */
import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import type { ConnectionView } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { formatNumber, formatUsd, t as tr, type MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { EFFORT_OPTIONS, NATIVE_PROVIDERS, ROLE_INFO } from "../../settings/rules";
import { useAppStore } from "../../store";
import { ModelCombobox, type ModelOption } from "../shared/ModelCombobox";
import { modelsHint, useConnectionModels, type ConnectionModels } from "../shared/useConnectionModels";
import { getNantySettings, NANTY_LANGUAGES, NANTY_ROLES, putNantySettings, type NantyLanguage, type NantyRole, type NantySettings } from "./nantyApi";
import { budgetFraction, draftOf, invalidNumbers, keyRefused, NANTY_LIMITS, settingsPatch, voiceCapable, voiceState, voiceWarning, type NantyDraft, type NantyNumberField, type VoiceCheck, type VoiceState } from "./nantySettingsModel";
import { useNanty } from "./nantyStore";
import { NantyTelegramCard } from "./NantyTelegramCard";
import { clockText } from "./nantyText";
import { useNantyVoices } from "./useNantyVoices";

const EMPTY_CONNECTIONS: readonly ConnectionView[] = [];

type Patch = (change: Partial<NantyDraft>) => void;

/** A conexão do Gemini que a voz usa agora: a escolhida ou, na automática, a que o servidor resolveu. */
function voiceConnection(draft: NantyDraft, connections: readonly ConnectionView[], check: VoiceCheck): ConnectionView | undefined {
  const resolved = check.status === "ready" || check.status === "failed" ? check.connection_id : null;
  const id = draft.voice_connection_id !== "" ? draft.voice_connection_id : resolved;
  return connections.find((c) => c.id === id && voiceCapable(c.kind));
}

function Card({ id, title, aside, children }: { id: string; title: string; aside?: ReactNode; children: ReactNode }) {
  const headingId = useId();
  return (
    <section className="nd-nanty-pref" data-nanty-pref={id} aria-labelledby={headingId}>
      <div className="nd-nanty-pref-head">
        <h3 id={headingId} className="nd-nanty-pref-title">
          {title}
        </h3>
        {aside}
      </div>
      {children}
    </section>
  );
}

function ConnectionSelect({
  id,
  value,
  connections,
  empty,
  onChange,
  label,
  nameOf,
  labelOf,
}: {
  id: string;
  value: string;
  connections: readonly ConnectionView[];
  empty: string;
  onChange: (id: string) => void;
  label?: string;
  /** Nome da conexão gravada que não está na lista (incompatível); sem ele, o id. */
  nameOf?: (id: string) => string;
  /** O texto de cada opção; sem ele, o nome. */
  labelOf?: (connection: ConnectionView) => string;
}) {
  return (
    <select id={id} className="nd-input" value={value} onChange={(event) => onChange(event.target.value)} aria-label={label}>
      <option value="">{empty}</option>
      {connections.map((c) => (
        <option key={c.id} value={c.id}>
          {labelOf?.(c) ?? c.name}
        </option>
      ))}
      {value !== "" && !connections.some((c) => c.id === value) ? <option value={value}>{nameOf?.(value) ?? value}</option> : null}
    </select>
  );
}

/**
 * O campo do modelo, com a lista da conexão (o mesmo combobox dos Agentes), sem filtro por nome: a
 * lista completa e o texto livre. Com `title`, leva rótulo e o "Atualizar lista".
 */
function ModelField({
  title,
  label,
  value,
  connection,
  models,
  disabled,
  placeholder,
  listed,
  onChange,
}: {
  title?: string;
  label?: string;
  value: string;
  connection: ConnectionView | undefined;
  models: ConnectionModels;
  disabled: boolean;
  placeholder?: string;
  /** A lista própria do campo, quando há; sem ela, os modelos de texto da conexão. */
  listed?: readonly ModelOption[];
  onChange: (model: string) => void;
}) {
  const { t } = useT();
  const id = useId();
  const entry = models.entryOf(connection);
  const own = listed !== undefined && listed.length > 0 ? listed : null;
  const hint = own !== null ? null : modelsHint(entry);
  const options = own ?? (entry?.status === "ready" ? entry.models : []);
  return (
    <div className="nd-field">
      {title !== undefined ? (
        <div className="nd-field-labelrow">
          <label htmlFor={id} className="nd-field-label">
            {title}
          </label>
          {connection !== undefined && entry !== undefined ? (
            <button type="button" className="nd-btn nd-label-action" data-variant="ghost" disabled={entry.status === "loading"} onClick={() => models.load(connection, true)}>
              <Icon name="refresh" size={14} />
              <span>{t("settings.agents.refreshList")}</span>
            </button>
          ) : null}
        </div>
      ) : null}
      <ModelCombobox id={id} label={label} value={value} options={options} disabled={disabled} placeholder={placeholder} onChange={onChange} />
      {hint !== null ? (
        <p aria-live="polite" className={hint.warn ? "nd-field-warn" : "nd-field-hint"}>
          {hint.text}
        </p>
      ) : null}
    </div>
  );
}

function NumberField({ field, label, value, min, max, step, invalid, onChange }: { field: NantyNumberField; label: string; value: string; min: number; max: number; step?: number; invalid: boolean; onChange: (v: string) => void }) {
  const { t } = useT();
  const id = useId();
  const errId = useId();
  return (
    <div className="nd-field">
      <label htmlFor={id} className="nd-field-label">
        {label}
      </label>
      <input
        id={id}
        type="number"
        inputMode="decimal"
        className="nd-input"
        min={min}
        max={max}
        step={step ?? 1}
        value={value}
        aria-invalid={invalid ? true : undefined}
        aria-describedby={invalid ? errId : undefined}
        onChange={(event) => onChange(event.target.value)}
        data-nanty-number={field}
      />
      {invalid ? (
        <p id={errId} className="nd-field-error">
          {t("nanty.prefs.number", { min: formatNumber(min), max: formatNumber(max) })}
        </p>
      ) : null}
    </div>
  );
}

function ModelCard({ draft, connections, models, patch }: { draft: NantyDraft; connections: readonly ConnectionView[]; models: ConnectionModels; patch: Patch }) {
  const { t } = useT();
  const connId = useId();
  const effortName = useId();
  const gone = draft.connection_id !== "" && !connections.some((c) => c.id === draft.connection_id);
  return (
    <Card id="model" title={t("nanty.prefs.model.title")}>
      <div className="nd-field">
        <label htmlFor={connId} className="nd-field-label">
          {t("nanty.prefs.model.connection")}
        </label>
        <ConnectionSelect
          id={connId}
          value={draft.connection_id}
          connections={connections}
          empty={t("nanty.prefs.model.none")}
          onChange={(connection_id) => {
            patch({ connection_id });
            const conn = connections.find((c) => c.id === connection_id);
            if (conn !== undefined) models.retryFailed(conn);
          }}
        />
        {gone ? <p className="nd-field-error">{t("nanty.prefs.model.gone", { id: draft.connection_id })}</p> : null}
      </div>
      <ModelField
        title={t("nanty.prefs.model.model")}
        value={draft.model}
        connection={connections.find((c) => c.id === draft.connection_id)}
        models={models}
        disabled={draft.connection_id === ""}
        onChange={(model) => patch({ model })}
      />
      <div className="nd-field">
        <span className="nd-field-label">{t("nanty.prefs.model.effort")}</span>
        <div role="radiogroup" aria-label={t("nanty.prefs.model.effort")} className="nd-segment nd-chrome">
          {EFFORT_OPTIONS.map((option) => (
            <label key={option.value} className="nd-segment-item" data-checked={draft.effort === option.value ? "true" : undefined}>
              <input type="radio" name={effortName} value={option.value} checked={draft.effort === option.value} onChange={() => patch({ effort: option.value })} className="nd-sr-only" />
              <span>{option.label}</span>
            </label>
          ))}
        </div>
      </div>
      <p className="nd-nanty-pref-note">{t(draft.connection_id === "" || draft.model.trim() === "" ? "nanty.prefs.model.off" : "nanty.prefs.model.hint")}</p>
    </Card>
  );
}

function BudgetCard({ draft, invalid, patch }: { draft: NantyDraft; invalid: readonly NantyNumberField[]; patch: Patch }) {
  const { t } = useT();
  const { status } = useNanty();
  const budget = status?.budget;
  const tokens = budget?.mode === "tokens";
  const spent = budget === undefined ? null : tokens ? budget.tokens : budget.spent_usd;
  const total = budget === undefined ? null : tokens ? budget.budget_tokens : budget.budget_usd;
  const fraction = spent === null || total === null ? 0 : budgetFraction(spent, total);
  const aside =
    budget !== undefined && spent !== null && total !== null ? (
      <span className="nd-nanty-pref-value nd-num" data-nanty-spent="">
        {tokens ? t("nanty.prefs.budget.spentTokens", { spent: formatNumber(spent), budget: formatNumber(total) }) : t("nanty.prefs.budget.spent", { spent: formatUsd(spent), budget: formatUsd(total) })}
      </span>
    ) : undefined;
  return (
    <Card id="budget" title={t("nanty.prefs.budget.title")} aside={aside}>
      {budget !== undefined ? (
        <div
          className="nd-nanty-meter"
          role="progressbar"
          aria-label={t("nanty.prefs.budget.title")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(fraction * 100)}
          data-over={budget.exhausted ? "true" : undefined}
        >
          <span className="nd-nanty-meter-fill" style={{ width: `${String(Math.round(fraction * 100))}%` }} />
        </div>
      ) : null}
      {budget !== undefined && (tokens || budget.unpriced) ? (
        <p className="nd-field-warn" data-nanty-unpriced="">
          {t("nanty.prefs.budget.unpriced")}
        </p>
      ) : null}
      {budget?.exhausted === true ? <p className="nd-field-warn">{t("nanty.prefs.budget.exhausted")}</p> : null}
      <div className="nd-nanty-fields">
        <NumberField
          field="daily_budget_usd"
          label={t("nanty.prefs.budget.usd")}
          value={draft.daily_budget_usd}
          min={NANTY_LIMITS.budgetUsd.min}
          max={NANTY_LIMITS.budgetUsd.max}
          step={0.01}
          invalid={invalid.includes("daily_budget_usd")}
          onChange={(daily_budget_usd) => patch({ daily_budget_usd })}
        />
        <NumberField
          field="daily_budget_tokens"
          label={t("nanty.prefs.budget.tokens")}
          value={draft.daily_budget_tokens}
          min={NANTY_LIMITS.budgetTokens.min}
          max={NANTY_LIMITS.budgetTokens.max}
          step={1000}
          invalid={invalid.includes("daily_budget_tokens")}
          onChange={(daily_budget_tokens) => patch({ daily_budget_tokens })}
        />
      </div>
      <p className="nd-nanty-pref-note">{t("nanty.prefs.budget.hint")}</p>
    </Card>
  );
}

function HoldField({ draft, invalid, patch }: { draft: NantyDraft; invalid: boolean; patch: Patch }) {
  const { t } = useT();
  return (
    <div className="nd-nanty-subfield" data-nanty-hold="">
      <button
        type="button"
        role="switch"
        aria-checked={draft.hold_while_visible}
        className="nd-btn nd-toggle"
        data-variant="ghost"
        onClick={() => patch({ hold_while_visible: !draft.hold_while_visible })}
      >
        <span className="nd-switch" data-on={draft.hold_while_visible ? "true" : undefined} aria-hidden="true" />
        <span>{t("nanty.tg.hold")}</span>
      </button>
      <p className="nd-field-hint">{t("nanty.tg.holdHint")}</p>
      {draft.hold_while_visible ? (
        <NumberField
          field="visible_hold_secs"
          label={t("nanty.tg.holdSecs")}
          value={draft.visible_hold_secs}
          min={NANTY_LIMITS.visibleHoldSecs.min}
          max={NANTY_LIMITS.visibleHoldSecs.max}
          invalid={invalid}
          onChange={(visible_hold_secs) => patch({ visible_hold_secs })}
        />
      ) : null}
    </div>
  );
}

const VOICE_STATE_LABEL: Readonly<Record<VoiceState, MessageKey>> = {
  ready: "nanty.prefs.voice.state.ready",
  no_key: "nanty.prefs.voice.state.noKey",
  key_refused: "nanty.prefs.voice.state.keyRefused",
  no_connection: "nanty.prefs.voice.state.noConnection",
};

/** O estado da voz, o que o servidor respondeu: pronta, sem chave ou sem conexão. */
function VoiceStateLine({ check, name }: { check: VoiceCheck; name: string | null }) {
  const { t } = useT();
  const state = voiceState(check);
  if (state === null) {
    return check.status === "loading" ? <p className="nd-field-hint" data-nanty-voice-state="loading">{t("nanty.prefs.voice.state.checking")}</p> : null;
  }
  const label = t(VOICE_STATE_LABEL[state]);
  return (
    <p className="nd-nanty-state nd-nanty-voice-state" role="status" data-nanty-voice-state={state}>
      <span className="nd-dot" data-tone={state === "ready" ? "green" : "amber"} aria-hidden="true" />
      <span>{state === "ready" && name !== null ? t("nanty.prefs.voice.state.readyVia", { name }) : label}</span>
    </p>
  );
}

function VoiceCard({
  draft,
  connections,
  invalid,
  patch,
  check,
  reloadVoices,
  models,
}: {
  draft: NantyDraft;
  connections: readonly ConnectionView[];
  invalid: boolean;
  patch: Patch;
  check: VoiceCheck;
  reloadVoices: () => void;
  models: ConnectionModels;
}) {
  const { t } = useT();
  const shell = useShell();
  const connectionId = useId();
  const voiceId = useId();
  const nameOf = (id: string): string => connections.find((c) => c.id === id)?.name ?? id;
  // Um seletor só, com as conexões do Gemini: a regra de `voice_capable` do servidor.
  const gemini = connections.filter((c) => voiceCapable(c.kind));
  const warn = voiceWarning(draft.voice_connection_id, connections);
  const conn = voiceConnection(draft, connections, check);
  const noneYet = gemini.length === 0;
  const voices = check.status === "ready" ? check.voices : [];
  const speechModels = check.status === "ready" ? check.speech_models : [];
  const toggle = (
    <button
      type="button"
      role="switch"
      aria-checked={draft.voice_enabled}
      className="nd-btn nd-toggle"
      data-variant="ghost"
      onClick={() => patch({ voice_enabled: !draft.voice_enabled })}
      data-nanty-voice-toggle=""
    >
      <span className="nd-switch" data-on={draft.voice_enabled ? "true" : undefined} aria-hidden="true" />
      <span>{t("nanty.prefs.voice.toggle")}</span>
    </button>
  );
  return (
    <Card id="voice" title={t("nanty.prefs.voice.title")} aside={toggle}>
      <p className="nd-nanty-pref-text">{t("nanty.prefs.voice.hint")}</p>
      <VoiceStateLine check={check} name={conn?.name ?? null} />
      {check.status === "no_key" ? (
        <p className="nd-field-warn" data-nanty-voice-why="no_key">
          {t("nanty.prefs.voice.noKeyHint")}
        </p>
      ) : null}
      {keyRefused(check) ? (
        <p className="nd-field-warn" data-nanty-voice-why="key_refused">
          {t("nanty.prefs.voice.keyRefusedHint")}
        </p>
      ) : null}
      {noneYet ? (
        <p className="nd-field-warn" data-nanty-voice-why="no_connection">
          {t("nanty.prefs.voice.noConnections")}
        </p>
      ) : null}
      {!noneYet || draft.voice_connection_id !== "" ? (
        <div className="nd-field">
          <label htmlFor={connectionId} className="nd-field-label">
            {t("nanty.prefs.voice.connection")}
          </label>
          <ConnectionSelect
            id={connectionId}
            value={draft.voice_connection_id}
            connections={gemini}
            nameOf={nameOf}
            labelOf={(c) => t("nanty.prefs.voice.connectionOption", { name: c.name, kind: NATIVE_PROVIDERS.google_gemini.brand })}
            empty={t("nanty.prefs.voice.connectionAuto")}
            onChange={(voice_connection_id) => {
              patch({ voice_connection_id });
              const next = gemini.find((c) => c.id === voice_connection_id);
              if (next !== undefined) models.retryFailed(next);
            }}
          />
          {warn === "gone" ? (
            <p className="nd-field-error" data-nanty-voice-warn="">
              {t("nanty.prefs.model.gone", { id: draft.voice_connection_id })}
            </p>
          ) : warn === "unusable" ? (
            <p className="nd-field-error" data-nanty-voice-warn="">
              {t("nanty.prefs.voice.connectionUnusable", { name: nameOf(draft.voice_connection_id) })}
            </p>
          ) : null}
        </div>
      ) : null}
      <div className="nd-nanty-voice-where">
        {noneYet ? null : <p className="nd-nanty-pref-note">{t("nanty.prefs.voice.connectionHint")}</p>}
        <button type="button" className="nd-btn" data-variant={noneYet ? "primary" : undefined} onClick={() => shell.openSettings("connections")} data-nanty-voice-connections="">
          {noneYet ? null : <Icon name="link" size={14} />}
          {t(noneYet ? "nanty.prefs.voice.createConnection" : "nanty.prefs.voice.openConnections")}
        </button>
      </div>
      <ModelField title={t("nanty.prefs.voice.speechModel")} value={draft.speech_model} connection={conn} models={models} disabled={false} listed={speechModels} onChange={(speech_model) => patch({ speech_model })} />
      <div className="nd-field">
        <div className="nd-field-labelrow">
          <label htmlFor={voiceId} className="nd-field-label">
            {t("nanty.prefs.voice.voice")}
          </label>
          {check.status === "ready" || check.status === "failed" ? (
            <button type="button" className="nd-btn nd-label-action" data-variant="ghost" onClick={reloadVoices}>
              <Icon name="refresh" size={14} />
              <span>{t("settings.agents.refreshList")}</span>
            </button>
          ) : null}
        </div>
        <ModelCombobox id={voiceId} value={draft.voice_name} options={voices} disabled={false} listLabel={t("nanty.prefs.voice.voices")} onChange={(voice_name) => patch({ voice_name })} />
        {check.status === "failed" && !keyRefused(check) ? (
          <p aria-live="polite" className="nd-field-warn" data-nanty-voices-failed="">
            {t("nanty.prefs.voice.listFailed", { error: apiFailureText(check.error) })}
          </p>
        ) : null}
      </div>
      <ModelField
        title={t("nanty.prefs.voice.transcribeModel")}
        value={draft.transcribe_model}
        connection={conn}
        models={models}
        disabled={false}
        onChange={(transcribe_model) => patch({ transcribe_model })}
      />
      <NumberField
        field="max_input_secs"
        label={t("nanty.prefs.voice.maxInput")}
        value={draft.max_input_secs}
        min={NANTY_LIMITS.voiceInputSecs.min}
        max={NANTY_LIMITS.voiceInputSecs.max}
        invalid={invalid}
        onChange={(max_input_secs) => patch({ max_input_secs })}
      />
    </Card>
  );
}

function HoursCard({ draft, now, patch, digestRef }: { draft: NantyDraft; now: string | null; patch: Patch; digestRef: RefObject<HTMLInputElement | null> }) {
  const { t } = useT();
  const startId = useId();
  const endId = useId();
  const digestId = useId();
  const emptyId = useId();
  const quiet = draft.quiet_start !== draft.quiet_end;
  const aside =
    now !== null ? (
      <span className="nd-nanty-pref-value nd-num" data-nanty-now="">
        {t("nanty.prefs.hours.now", { time: clockText(now) })}
      </span>
    ) : undefined;
  return (
    <Card id="hours" title={t("nanty.prefs.hours.title")} aside={aside}>
      <div className="nd-nanty-fields">
        <div className="nd-field">
          <label htmlFor={startId} className="nd-field-label">
            {t("nanty.prefs.hours.quietStart")}
          </label>
          <input id={startId} type="time" className="nd-input" value={draft.quiet_start} onChange={(event) => event.target.value !== "" && patch({ quiet_start: event.target.value })} />
        </div>
        <div className="nd-field">
          <label htmlFor={endId} className="nd-field-label">
            {t("nanty.prefs.hours.quietEnd")}
          </label>
          <input id={endId} type="time" className="nd-input" value={draft.quiet_end} onChange={(event) => event.target.value !== "" && patch({ quiet_end: event.target.value })} />
        </div>
        <div className="nd-field">
          <label htmlFor={digestId} className="nd-field-label">
            {t("nanty.prefs.hours.digest")}
          </label>
          <input
            id={digestId}
            ref={digestRef}
            type="time"
            className="nd-input"
            value={draft.digest_time}
            onChange={(event) => event.target.value !== "" && patch({ digest_time: event.target.value })}
            data-nanty-digest-time=""
          />
        </div>
        <div className="nd-field">
          <label htmlFor={emptyId} className="nd-field-label">
            {t("nanty.prefs.hours.empty")}
          </label>
          <select id={emptyId} className="nd-input" value={draft.digest_empty} onChange={(event) => patch({ digest_empty: event.target.value === "skip" ? "skip" : "short" })}>
            <option value="short">{t("nanty.prefs.hours.emptyShort")}</option>
            <option value="skip">{t("nanty.prefs.hours.emptySkip")}</option>
          </select>
        </div>
      </div>
      <p className="nd-nanty-pref-text" data-nanty-hours-summary="">
        {quiet
          ? t("nanty.prefs.hours.summary", { start: clockText(draft.quiet_start), end: clockText(draft.quiet_end), digest: clockText(draft.digest_time) })
          : t("nanty.prefs.hours.noQuiet", { digest: clockText(draft.digest_time) })}
      </p>
      <p className="nd-nanty-pref-note">{t("nanty.prefs.hours.hint")}</p>
    </Card>
  );
}

/** Rótulo de cada opção do idioma próprio do Nanty. */
const LANGUAGE_LABEL: Record<NantyLanguage, MessageKey> = {
  follow: "nanty.prefs.lang.follow",
  "pt-BR": "nanty.prefs.lang.pt",
  en: "nanty.prefs.lang.en",
};

/** Spec 139 T05: o idioma próprio do Nanty (avisos e resumo); a conversa segue quem falou. */
function LanguageCard({ draft, lang, patch }: { draft: NantyDraft; lang: NantySettings["lang"]; patch: Patch }) {
  const { t } = useT();
  const id = useId();
  return (
    <Card id="lang" title={t("nanty.prefs.lang.title")}>
      <p className="nd-nanty-pref-text">{t("nanty.prefs.lang.text")}</p>
      <div className="nd-field">
        <label htmlFor={id} className="nd-field-label">
          {t("nanty.prefs.lang.field")}
        </label>
        <select
          id={id}
          className="nd-input"
          value={draft.language}
          onChange={(event) => {
            const next = NANTY_LANGUAGES.find((l) => l === event.target.value);
            if (next !== undefined) patch({ language: next });
          }}
          data-nanty-language=""
        >
          {NANTY_LANGUAGES.map((l) => (
            <option key={l} value={l}>
              {t(LANGUAGE_LABEL[l])}
            </option>
          ))}
        </select>
      </div>
      {draft.language === "follow" ? (
        <p className="nd-nanty-pref-note" data-nanty-lang={lang}>
          {t("nanty.prefs.lang.followHint", { lang: t(lang === "en" ? "nanty.prefs.lang.enLower" : "nanty.prefs.lang.ptLower") })}
        </p>
      ) : null}
    </Card>
  );
}

function ReserveCard({ draft, connections, models, patch }: { draft: NantyDraft; connections: readonly ConnectionView[]; models: ConnectionModels; patch: Patch }) {
  const { t } = useT();
  const set = (role: NantyRole, change: Partial<NantyDraft["reserve"][NantyRole]>): void => patch({ reserve: { ...draft.reserve, [role]: { ...draft.reserve[role], ...change } } });
  return (
    <Card id="reserve" title={t("nanty.prefs.reserve.title")}>
      <div className="nd-nanty-reserve">
        {NANTY_ROLES.map((role) => {
          const route = draft.reserve[role];
          const label = ROLE_INFO[role].label;
          return (
            <div key={role} className="nd-nanty-reserve-row" data-nanty-reserve={role}>
              <span className="nd-nanty-reserve-role">{label}</span>
              <ConnectionSelect
                id={`nanty-reserve-${role}`}
                value={route.connection_id}
                connections={connections}
                empty={t("nanty.prefs.reserve.none")}
                label={t("nanty.prefs.reserve.connection", { role: label })}
                onChange={(connection_id) => {
                  set(role, connection_id === "" ? { connection_id, model: "" } : { connection_id });
                  const conn = connections.find((c) => c.id === connection_id);
                  if (conn !== undefined) models.retryFailed(conn);
                }}
              />
              <ModelField
                label={t("nanty.prefs.reserve.model", { role: label })}
                value={route.model}
                connection={connections.find((c) => c.id === route.connection_id)}
                models={models}
                disabled={route.connection_id === ""}
                placeholder={t("nanty.prefs.reserve.modelPlaceholder")}
                onChange={(model) => set(role, { model })}
              />
            </div>
          );
        })}
      </div>
      <p className="nd-nanty-pref-note">{t("nanty.prefs.reserve.hint")}</p>
    </Card>
  );
}

function ProjectsCard({ draft, patch }: { draft: NantyDraft; patch: Patch }) {
  const { t } = useT();
  const projects = useAppStore((s) => s.config?.projects);
  const list = projects ?? [];
  return (
    <Card id="projects" title={t("nanty.prefs.projects.title")}>
      <p className="nd-nanty-pref-text">{t("nanty.prefs.projects.hint")}</p>
      {list.length === 0 ? <p className="nd-field-hint">{t("nanty.prefs.projects.empty")}</p> : null}
      <div className="nd-nanty-projects">
        {list.map((p) => {
          const watched = !draft.excluded.includes(p.id);
          return (
            <label key={p.id} className="nd-check" data-nanty-project={p.id}>
              <input
                type="checkbox"
                checked={watched}
                onChange={() => patch({ excluded: watched ? [...draft.excluded, p.id] : draft.excluded.filter((id) => id !== p.id) })}
              />
              {p.name}
            </label>
          );
        })}
      </div>
    </Card>
  );
}

type Notice = { tone: "ok" | "warn"; text: string };

export function NantySettingsView({ focusDigest, onFocused }: { focusDigest: boolean; onFocused: () => void }) {
  const { t } = useT();
  const connections = useAppStore((s) => s.config?.connections) ?? EMPTY_CONNECTIONS;
  const [saved, setSaved] = useState<NantySettings | null>(null);
  const [draft, setDraft] = useState<NantyDraft | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const digestRef = useRef<HTMLInputElement | null>(null);
  const mounted = useRef(true);
  const geminiSignature = connections
    .filter((c) => voiceCapable(c.kind))
    .map((c) => `${c.id}|${c.base_url}|${c.api_key_masked}`)
    .join("\n");
  const voices = useNantyVoices(draft !== null, draft?.voice_connection_id ?? "", geminiSignature);
  const inUse = draft === null ? [] : [draft.connection_id, ...NANTY_ROLES.map((role) => draft.reserve[role].connection_id)];
  const voiceConn = draft === null ? undefined : voiceConnection(draft, connections, voices.check);
  const models = useConnectionModels(
    [...connections.filter((c) => inUse.includes(c.id)), ...(voiceConn === undefined ? [] : [voiceConn])],
    connections,
  );

  useEffect(() => {
    mounted.current = true;
    void getNantySettings().then((r) => {
      if (!mounted.current) return;
      if (r.ok) {
        setSaved(r.data);
        setDraft(draftOf(r.data));
      } else setLoadError(tr("nanty.prefs.loadFailed", { error: apiFailureText(r.error) }));
    });
    return () => {
      mounted.current = false;
    };
  }, []);

  // "Mudar horário" do Previsto (feed): o campo do resumo da manhã, em foco.
  useEffect(() => {
    if (!focusDigest || draft === null) return;
    const el = digestRef.current;
    if (el !== null) {
      el.scrollIntoView?.({ block: "center" });
      el.focus();
    }
    onFocused();
  }, [focusDigest, draft === null]);

  if (draft === null || saved === null) {
    return (
      <div className="nd-nanty-prefs-state">
        {loadError !== null ? (
          <p role="alert" className="nd-field-error">
            {loadError}
          </p>
        ) : (
          <p className="nd-field-hint">{t("nanty.prefs.loading")}</p>
        )}
      </div>
    );
  }

  const patch: Patch = (change) => {
    setDraft((d) => (d === null ? d : { ...d, ...change }));
    setNotice(null);
  };
  const invalid = invalidNumbers(draft);
  const changes = settingsPatch(saved, draft);
  const dirty = changes === null || Object.keys(changes).length > 0;

  const save = async (): Promise<void> => {
    if (saving) return;
    if (changes === null) return;
    if (Object.keys(changes).length === 0) {
      setNotice({ tone: "ok", text: t("nanty.prefs.unchanged") });
      return;
    }
    setSaving(true);
    setNotice(null);
    const result = await putNantySettings(changes);
    if (!mounted.current) return;
    setSaving(false);
    if (!result.ok) {
      setNotice({ tone: "warn", text: t("nanty.prefs.failed", { error: apiFailureText(result.error) }) });
      return;
    }
    setSaved(result.data);
    setDraft(draftOf(result.data));
    setNotice({ tone: "ok", text: t("nanty.prefs.saved") });
  };

  return (
    <div className="nd-nanty-settings" data-nanty-settings="">
      <div className="nd-nanty-prefs">
        <ModelCard draft={draft} connections={connections} models={models} patch={patch} />
        <BudgetCard draft={draft} invalid={invalid} patch={patch} />
        <NantyTelegramCard hold={<HoldField draft={draft} invalid={invalid.includes("visible_hold_secs")} patch={patch} />} />
        <VoiceCard draft={draft} connections={connections} invalid={invalid.includes("max_input_secs")} patch={patch} check={voices.check} reloadVoices={voices.reload} models={models} />
        <HoursCard draft={draft} now={saved.now} patch={patch} digestRef={digestRef} />
        <ReserveCard draft={draft} connections={connections} models={models} patch={patch} />
        <ProjectsCard draft={draft} patch={patch} />
        <LanguageCard draft={draft} lang={saved.lang} patch={patch} />
      </div>
      <div className="nd-nanty-savebar" data-nanty-savebar="">
        {notice !== null ? (
          <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-save-notice="">
            {notice.text}
          </p>
        ) : null}
        <button type="button" className="nd-btn" data-variant="primary" disabled={saving || changes === null || !dirty} aria-busy={saving} onClick={() => void save()} data-nanty-save="">
          <Icon name="save" size={14} />
          {saving ? t("nanty.prefs.saving") : t("nanty.prefs.save")}
        </button>
      </div>
    </div>
  );
}
