/*
 * Aba Ajustes do Nanty (spec 138 T11, `reference/Settings.dc.html`): cartões em grade (duas colunas
 * na janela larga, uma na estreita e no celular).
 *
 * - **Modelo e esforço**, pelas conexões que já existem;
 * - **Orçamento do dia**, com a barra do gasto e o aviso "sem preço, o orçamento conta tokens";
 * - **Telegram** (`NantyTelegramCard`), com a espera com a aba aberta;
 * - **Voz:** ligar, a ordem das conexões e a conexão da transcrição;
 * - **Horários:** silêncio e resumo da manhã, com a hora do Nants agora (C-15, lida do servidor) e
 *   os horários no formato da tela;
 * - **Idioma:** o do Nanty segue o da interface (C-08); o botão abre a Aparência;
 * - **Reserva** por papel e **projetos** observados.
 *
 * Um só "Salvar" no fim manda só o que mudou (`PUT /api/nanty/config`); o erro sai com o código
 * traduzido. As ações do Telegram (token, parear, teste, desparear) gravam na hora, pelas rotas do
 * canal.
 */
import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import type { ConnectionView } from "../../api/types";
import { useShell } from "../../desktop/shellContext";
import { Icon } from "../../desktop/Icon";
import { formatNumber, formatUsd, t as tr } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { EFFORT_OPTIONS, ROLE_INFO } from "../../settings/rules";
import { useAppStore } from "../../store";
import { getNantySettings, NANTY_ROLES, putNantySettings, type NantyRole, type NantySettings } from "./nantyApi";
import { budgetFraction, draftOf, speaks, transcribes, voiceWarning, invalidNumbers, moveStep, NANTY_LIMITS, settingsPatch, type NantyDraft, type NantyNumberField } from "./nantySettingsModel";
import { useNanty } from "./nantyStore";
import { NantyTelegramCard } from "./NantyTelegramCard";
import { clockText } from "./nantyText";

const EMPTY_CONNECTIONS: readonly ConnectionView[] = [];

type Patch = (change: Partial<NantyDraft>) => void;

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
}: {
  id: string;
  value: string;
  connections: readonly ConnectionView[];
  empty: string;
  onChange: (id: string) => void;
  label?: string;
  /** Nome da conexão gravada que não está na lista (incompatível); sem ele, o id. */
  nameOf?: (id: string) => string;
}) {
  return (
    <select id={id} className="nd-input" value={value} onChange={(event) => onChange(event.target.value)} aria-label={label}>
      <option value="">{empty}</option>
      {connections.map((c) => (
        <option key={c.id} value={c.id}>
          {c.name}
        </option>
      ))}
      {value !== "" && !connections.some((c) => c.id === value) ? <option value={value}>{nameOf?.(value) ?? value}</option> : null}
    </select>
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

function ModelCard({ draft, connections, patch }: { draft: NantyDraft; connections: readonly ConnectionView[]; patch: Patch }) {
  const { t } = useT();
  const connId = useId();
  const modelId = useId();
  const effortName = useId();
  const gone = draft.connection_id !== "" && !connections.some((c) => c.id === draft.connection_id);
  return (
    <Card id="model" title={t("nanty.prefs.model.title")}>
      <div className="nd-field">
        <label htmlFor={connId} className="nd-field-label">
          {t("nanty.prefs.model.connection")}
        </label>
        <ConnectionSelect id={connId} value={draft.connection_id} connections={connections} empty={t("nanty.prefs.model.none")} onChange={(connection_id) => patch({ connection_id })} />
        {gone ? <p className="nd-field-error">{t("nanty.prefs.model.gone", { id: draft.connection_id })}</p> : null}
      </div>
      <div className="nd-field">
        <label htmlFor={modelId} className="nd-field-label">
          {t("nanty.prefs.model.model")}
        </label>
        <input id={modelId} type="text" className="nd-input" value={draft.model} disabled={draft.connection_id === ""} onChange={(event) => patch({ model: event.target.value })} autoComplete="off" spellCheck={false} />
      </div>
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

function VoiceCard({ draft, connections, invalid, patch }: { draft: NantyDraft; connections: readonly ConnectionView[]; invalid: boolean; patch: Patch }) {
  const { t } = useT();
  const addId = useId();
  const transcribeId = useId();
  const nameOf = (id: string): string => connections.find((c) => c.id === id)?.name ?? id;
  // Cada seletor oferece só as conexões que o caminho usa (as regras de `voice::speaks` e
  // `voice::transcribes` do servidor).
  const speakers = connections.filter((c) => speaks(c.kind));
  const transcribers = connections.filter((c) => transcribes(c.kind));
  const free = speakers.filter((c) => !draft.voice_order.some((r) => r.connection_id === c.id));
  const transcribeWarn = voiceWarning(draft.transcribe.connection_id, connections, transcribes);
  const full = draft.voice_order.length >= NANTY_LIMITS.voiceSteps;
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
      <span className="nd-field-label">{t("nanty.prefs.voice.order")}</span>
      {draft.voice_order.length === 0 ? <p className="nd-field-hint" data-nanty-voice-auto="">{t("nanty.prefs.voice.auto")}</p> : null}
      <ol className="nd-nanty-order" data-nanty-voice-order="">
        {draft.voice_order.map((step, i) => {
          const name = nameOf(step.connection_id);
          const warn = voiceWarning(step.connection_id, connections, speaks);
          return (
            <li key={step.connection_id} className="nd-nanty-order-item" data-nanty-voice-step={step.connection_id} data-warn={warn ?? undefined}>
              <span className="nd-nanty-order-n nd-num" aria-hidden="true">
                {i + 1}
              </span>
              <span className="nd-nanty-order-name">
                {name}
                {warn !== null ? (
                  <span className="nd-field-error" data-nanty-voice-warn={step.connection_id}>
                    {warn === "gone" ? t("nanty.prefs.model.gone", { id: step.connection_id }) : t("nanty.prefs.voice.stepUnusable", { name })}
                  </span>
                ) : null}
              </span>
              <span className="nd-nanty-order-tools">
                <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" aria-label={t("nanty.prefs.voice.up", { name })} disabled={i === 0} onClick={() => patch({ voice_order: moveStep(draft.voice_order, i, -1) })}>
                  <Icon name="arrow-up" size={14} />
                </button>
                <button
                  type="button"
                  className="nd-btn nd-btn-sm"
                  data-variant="ghost"
                  aria-label={t("nanty.prefs.voice.down", { name })}
                  disabled={i === draft.voice_order.length - 1}
                  onClick={() => patch({ voice_order: moveStep(draft.voice_order, i, 1) })}
                >
                  <Icon name="arrow-down" size={14} />
                </button>
                <button
                  type="button"
                  className="nd-btn nd-btn-sm"
                  data-variant="ghost"
                  aria-label={t("nanty.prefs.voice.remove", { name })}
                  onClick={() => patch({ voice_order: draft.voice_order.filter((_, k) => k !== i) })}
                >
                  <Icon name="close" size={14} />
                </button>
              </span>
            </li>
          );
        })}
        {draft.voice_order.length > 0 ? (
          <li className="nd-nanty-order-item" data-nanty-voice-step="text">
            <span className="nd-nanty-order-n nd-num" aria-hidden="true">
              {draft.voice_order.length + 1}
            </span>
            <span className="nd-nanty-order-name">{t("nanty.prefs.voice.textOnly")}</span>
          </li>
        ) : null}
      </ol>
      {free.length > 0 && !full ? (
        <div className="nd-field">
          <label htmlFor={addId} className="nd-field-label">
            {t("nanty.prefs.voice.add")}
          </label>
          <select
            id={addId}
            className="nd-input"
            value=""
            onChange={(event) => {
              const id = event.target.value;
              if (id !== "") patch({ voice_order: [...draft.voice_order, { connection_id: id, model: "", voice: "" }] });
            }}
          >
            <option value="">{t("nanty.prefs.voice.addPick")}</option>
            {free.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </div>
      ) : null}
      <div className="nd-field">
        <label htmlFor={transcribeId} className="nd-field-label">
          {t("nanty.prefs.voice.transcribe")}
        </label>
        <ConnectionSelect
          id={transcribeId}
          value={draft.transcribe.connection_id}
          connections={transcribers}
          nameOf={nameOf}
          empty={t("nanty.prefs.voice.transcribeAuto")}
          onChange={(connection_id) => patch({ transcribe: { connection_id, model: "" } })}
        />
        {transcribeWarn === "gone" ? (
          <p className="nd-field-error" data-nanty-voice-warn="transcribe">
            {t("nanty.prefs.model.gone", { id: draft.transcribe.connection_id })}
          </p>
        ) : transcribeWarn === "unusable" ? (
          <p className="nd-field-error" data-nanty-voice-warn="transcribe">
            {t("nanty.prefs.voice.transcribeUnusable", { name: nameOf(draft.transcribe.connection_id) })}
          </p>
        ) : null}
      </div>
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

function LanguageCard({ lang }: { lang: NantySettings["lang"] }) {
  const { t } = useT();
  const shell = useShell();
  return (
    <Card id="lang" title={t("nanty.prefs.lang.title")}>
      <p className="nd-nanty-pref-text" data-nanty-lang={lang}>
        {t("nanty.prefs.lang.text", { lang: t(lang === "en" ? "language.name.en" : "language.name.pt-BR") })}
      </p>
      <button type="button" className="nd-btn nd-btn-sm nd-self-start" onClick={() => shell.openSettings("appearance")}>
        <Icon name="globe" size={14} />
        {t("nanty.prefs.lang.change")}
      </button>
    </Card>
  );
}

function ReserveCard({ draft, connections, patch }: { draft: NantyDraft; connections: readonly ConnectionView[]; patch: Patch }) {
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
                onChange={(connection_id) => set(role, connection_id === "" ? { connection_id, model: "" } : { connection_id })}
              />
              <input
                type="text"
                className="nd-input"
                value={route.model}
                disabled={route.connection_id === ""}
                aria-label={t("nanty.prefs.reserve.model", { role: label })}
                placeholder={t("nanty.prefs.reserve.modelPlaceholder")}
                onChange={(event) => set(role, { model: event.target.value })}
                autoComplete="off"
                spellCheck={false}
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
        <ModelCard draft={draft} connections={connections} patch={patch} />
        <BudgetCard draft={draft} invalid={invalid} patch={patch} />
        <NantyTelegramCard hold={<HoldField draft={draft} invalid={invalid.includes("visible_hold_secs")} patch={patch} />} />
        <VoiceCard draft={draft} connections={connections} invalid={invalid.includes("max_input_secs")} patch={patch} />
        <HoursCard draft={draft} now={saved.now} patch={patch} digestRef={digestRef} />
        <ReserveCard draft={draft} connections={connections} patch={patch} />
        <ProjectsCard draft={draft} patch={patch} />
        <LanguageCard lang={saved.lang} />
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
