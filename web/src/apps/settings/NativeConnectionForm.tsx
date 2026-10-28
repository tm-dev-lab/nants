/*
 * Conexão nativa (spec 132 T10): Anthropic (Claude) e Google Gemini (AI Studio).
 * - Só o nome e a chave (com `env:NOME` aceito); o endereço oficial é fixo e o avançado (um proxy
 *   da empresa) fica recolhido. O identificador nasce do nome (`nativeConnectionId`).
 * - A dica da chave leva ao endereço oficial onde ela é criada.
 * - "Testar" consulta a lista de modelos do provedor sem salvar e mostra os modelos que a chave
 *   alcança, na ordem do provedor.
 * - Ao salvar a primeira conexão de um provedor, sugere um modelo por papel (`suggestRoleModels`:
 *   o de menor custo no catálogo de preços entre os que raciocinam e aceitam imagem, sem nome de
 *   modelo no código), editável, com "Usar nos papéis" e "Agora não".
 * A chave só existe no campo e no corpo do envio: sai do estado logo no envio e nunca é exibida.
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { getConnectionModels, getPriceEstimate, putAgents, putConnection, testConnection, type ApiFailure } from "../../api/client";
import type { AgentConfig, Agents, ConnectionInput, ConnectionView, ModelEntry, TestConnectionInput } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import {
  connectionKindOf,
  describeModelsError,
  describeTestResult,
  NATIVE_PROVIDERS,
  nativeConnectionId,
  ROLE_INFO,
  ROLE_ORDER,
  suggestionCandidates,
  suggestRoleModels,
  validateNativeForm,
  type AgentRole,
  type NativeFieldErrors,
  type NativeFormValues,
  type NativeKind,
} from "../../settings/rules";
import { appStore, useAppStore } from "../../store";
import { storeConnection } from "./PlanConnection";

/** Quantos modelos aparecem em pílulas depois do "Testar"; o resto vira "e mais N". */
const MODELS_SHOWN = 12;

/** Teto de modelos por consulta de preço (o mesmo do servidor). */
const PRICE_LOOKUP_MAX = 64;

/** Variável de ambiente do exemplo da dica da chave (o nome usual de cada provedor). */
const ENV_EXAMPLE: Readonly<Record<NativeKind, string>> = { anthropic: "ANTHROPIC_API_KEY", google_gemini: "GEMINI_API_KEY" };

interface NativeConnectionFormProps {
  kind: NativeKind;
  /** Ids já salvos: o id derivado do nome evita todos. */
  existingIds: readonly string[];
  /** Conexão a editar. Ausente = criação. */
  initial?: ConnectionView;
  onSaved: (view: ConnectionView) => void;
  onCancel?: () => void;
}

interface TestState {
  ok: boolean;
  message: () => string;
  models: ModelEntry[];
}

function valuesOf(kind: NativeKind, initial: ConnectionView | undefined): NativeFormValues {
  if (initial === undefined) return { name: NATIVE_PROVIDERS[kind].brand, api_key: "", base_url_override: "" };
  return { name: initial.name, api_key: "", base_url_override: initial.base_url_override ?? "" };
}

export function NativeConnectionForm({ kind, existingIds, initial, onSaved, onCancel }: NativeConnectionFormProps) {
  const { t, tp } = useT();
  const provider = NATIVE_PROVIDERS[kind];
  const connections = useAppStore((s) => s.config?.connections) ?? [];
  const ids = { name: useId(), key: useId(), url: useId(), keyHint: useId(), urlHint: useId(), error: useId(), models: useId() };
  const [form, setForm] = useState<NativeFormValues>(() => valuesOf(kind, initial));
  const [errors, setErrors] = useState<NativeFieldErrors>({});
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<TestState | null>(null);
  // Conexão salva que espera a sugestão de modelos (só a primeira do provedor).
  const [suggesting, setSuggesting] = useState<ConnectionView | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const isEdit = initial !== undefined;
  // Calculado antes de salvar: depois, a própria conexão nova já estaria na lista.
  const firstOfKind = !isEdit && !connections.some((c) => connectionKindOf(c) === kind);

  function update<K extends keyof NativeFormValues>(key: K, value: NativeFormValues[K]): void {
    setForm((f) => ({ ...f, [key]: value }));
    if (key !== "name") setTest(null);
  }

  async function runTest(): Promise<void> {
    if (testing) return;
    const found = validateNativeForm(form, isEdit);
    delete found.name;
    setErrors(found);
    setFailure(null);
    if (Object.keys(found).length > 0) return;
    const body: TestConnectionInput = { kind, base_url_override: form.base_url_override.trim() };
    if (initial !== undefined) body.id = initial.id;
    if (form.api_key.trim() !== "") body.api_key = form.api_key.trim();
    setTesting(true);
    setTest(null);
    const outcome = await testConnection(body);
    if (!mounted.current) return;
    setTesting(false);
    if (!outcome.ok) {
      const error = outcome.error;
      setTest({ ok: false, message: () => apiFailureText(error), models: [] });
      return;
    }
    const data = outcome.data;
    const models = Array.isArray(data.models) ? data.models.filter((m) => typeof m?.id === "string") : [];
    setTest({ ok: data.ok === true, message: () => describeTestResult(data), models });
  }

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (busy) return;
    const found = validateNativeForm(form, isEdit);
    setErrors(found);
    setFailure(null);
    if (Object.keys(found).length > 0) return;
    const payload: ConnectionInput = {
      id: initial?.id ?? nativeConnectionId(form.name, kind, existingIds),
      name: form.name.trim(),
      base_url: "",
      api_key: form.api_key.trim(),
      kind,
      base_url_override: form.base_url_override.trim(),
    };
    // A chave sai do estado local já no envio.
    setForm((f) => ({ ...f, api_key: "" }));
    setBusy(true);
    const result = await putConnection(payload);
    if (!mounted.current) return;
    setBusy(false);
    if (!result.ok) {
      setFailure(result.error);
      return;
    }
    storeConnection(result.data);
    if (firstOfKind) setSuggesting(result.data);
    else onSaved(result.data);
  }

  if (suggesting !== null) {
    return <RoleSuggestion connection={suggesting} kind={kind} onDone={() => onSaved(suggesting)} />;
  }

  // O 422 com código mostra a mensagem dele (a chave pedida ao trocar o endereço avançado, spec 132
  // T11); só o 422 sem código cai no texto genérico.
  const submitError =
    failure === null ? null : failure.status === 422 && failure.code === undefined ? t("settings.form.unprocessable") : apiFailureText(failure);
  const shown = test?.models.slice(0, MODELS_SHOWN) ?? [];
  const more = (test?.models.length ?? 0) - shown.length;
  return (
    <form className="nd-form" onSubmit={(event) => void submit(event)} noValidate aria-describedby={submitError !== null ? ids.error : undefined} data-native-form={kind}>
      <h4 className="nd-section-label">{t(`native.title.${kind}`)}</h4>
      <div className="nd-field">
        <label htmlFor={ids.name} className="nd-field-label">
          {t("settings.form.name")}
        </label>
        <input id={ids.name} type="text" className="nd-input" value={form.name} onChange={(e) => update("name", e.target.value)} autoComplete="off" />
        {errors.name !== undefined ? <p className="nd-field-error">{errors.name}</p> : null}
      </div>
      <div className="nd-field">
        <label htmlFor={ids.key} className="nd-field-label">
          {t("settings.form.key")}
        </label>
        <input
          id={ids.key}
          type="password"
          className="nd-input"
          value={form.api_key}
          onChange={(e) => update("api_key", e.target.value)}
          autoComplete="new-password"
          spellCheck={false}
          aria-describedby={ids.keyHint}
        />
        <p id={ids.keyHint} className="nd-field-hint">
          {t("native.keyHint", { env: ENV_EXAMPLE[kind] })}
          {isEdit ? ` ${t("native.keyHintEdit")}` : ""}
        </p>
        <a className="nd-btn nd-btn-sm nd-self-start" data-variant="ghost" href={provider.keyUrl} target="_blank" rel="noopener noreferrer" data-key-link="">
          <Icon name="external" size={14} />
          {t(`native.keyLink.${kind}`)}
        </a>
        {errors.api_key !== undefined ? <p className="nd-field-error">{errors.api_key}</p> : null}
      </div>
      <details className="nd-details" data-native-advanced="" open={form.base_url_override.trim() !== "" || errors.base_url_override !== undefined}>
        <summary>
          <Icon name="chevron-right" size={14} className="nd-details-chevron" />
          {t("native.advanced")}
        </summary>
        <div className="nd-field">
          <label htmlFor={ids.url} className="nd-field-label">
            {t("native.advancedLabel")}
          </label>
          <input
            id={ids.url}
            type="url"
            className="nd-input"
            value={form.base_url_override}
            onChange={(e) => update("base_url_override", e.target.value)}
            placeholder={provider.officialUrl}
            autoComplete="off"
            spellCheck={false}
            aria-describedby={ids.urlHint}
          />
          <p id={ids.urlHint} className="nd-field-hint">
            {t("native.advancedHint", { url: provider.officialUrl })}
          </p>
          {errors.base_url_override !== undefined ? <p className="nd-field-error">{errors.base_url_override}</p> : null}
        </div>
      </details>
      {testing ? (
        <p role="status" className="nd-muted">
          {t("native.testing")}
        </p>
      ) : null}
      {test !== null ? (
        <div className="nd-stack-xs" data-native-test={test.ok ? "ok" : "failed"}>
          <p role="status" className="nd-test-result" data-ok={test.ok ? "true" : "false"}>
            <Icon name={test.ok ? "check" : "alert"} size={14} />
            {test.message()}
          </p>
          {shown.length > 0 ? (
            <section aria-labelledby={ids.models} className="nd-stack-xs" data-native-models="">
              <h5 id={ids.models} className="nd-section-label">
                {t("native.models")}
              </h5>
              <ul className="nd-plan-models">
                {shown.map((m) => (
                  <li key={m.id} className="nd-pill">
                    {m.name !== undefined && m.name.trim() !== "" ? m.name : m.id}
                  </li>
                ))}
              </ul>
              {more > 0 ? <p className="nd-muted">{tp("native.modelsMore", more)}</p> : null}
            </section>
          ) : null}
        </div>
      ) : null}
      {submitError !== null ? (
        <p id={ids.error} role="alert" className="nd-dialog-error">
          {submitError}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        {onCancel !== undefined ? (
          <button type="button" className="nd-btn" onClick={onCancel}>
            {t("common.cancel")}
          </button>
        ) : null}
        <button type="button" className="nd-btn" disabled={testing} aria-busy={testing} data-native-test-button="" onClick={() => void runTest()}>
          <Icon name="zap" size={14} />
          {testing ? t("native.testing") : t("native.test")}
        </button>
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
          <Icon name="save" size={14} />
          {t("settings.form.save")}
        </button>
      </div>
    </form>
  );
}

type SuggestionLoad =
  | { status: "loading" }
  | { status: "ready"; models: ModelEntry[]; picks: Record<AgentRole, string> }
  | { status: "empty" }
  | { status: "error"; message: string };

/** Valor do seletor que mantém o modelo atual do papel. */
const KEEP = "";

/**
 * Sugestão de modelo por papel depois da primeira conexão do provedor: um seletor por papel com os
 * modelos da conexão, já com a sugestão; "Manter o atual" deixa o papel como está.
 */
function RoleSuggestion({ connection, kind, onDone }: { connection: ConnectionView; kind: NativeKind; onDone: () => void }) {
  const { t } = useT();
  const headingId = useId();
  const [load, setLoad] = useState<SuggestionLoad>({ status: "loading" });
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    void getConnectionModels(connection.id).then((outcome) => {
      if (!mounted.current) return;
      if (!outcome.ok) {
        setLoad({ status: "error", message: t("native.suggest.failed") });
        return;
      }
      const data = outcome.data;
      if (typeof data.error === "string") {
        setLoad({ status: "error", message: describeModelsError(data.error, data.status) });
        return;
      }
      const models = data.models.filter((m) => typeof m?.id === "string");
      const candidates = suggestionCandidates(models).map((m) => m.id);
      if (candidates.length === 0) {
        setLoad({ status: "empty" });
        return;
      }
      // O preço dos candidatos pelo catálogo (o mesmo caminho da tela de Preços), pelo tipo da
      // conexão; sem catálogo, a sugestão segue a ordem do provedor.
      void getPriceEstimate(candidates.slice(0, PRICE_LOOKUP_MAX), { kind }).then((estimate) => {
        if (!mounted.current) return;
        const picks = suggestRoleModels(models, estimate.ok ? estimate.data : {});
        setLoad(picks === null ? { status: "empty" } : { status: "ready", models, picks });
      });
    });
    return () => {
      mounted.current = false;
    };
    // `t` lido na hora da resposta; carrega uma vez por conexão.
  }, [connection.id, kind]);

  async function apply(): Promise<void> {
    if (saving || load.status !== "ready") return;
    const current = appStore.getState().config?.agents;
    if (current === undefined) return;
    const next = (role: AgentRole): AgentConfig => {
      const pick = load.picks[role];
      return pick === KEEP ? current[role] : { ...current[role], connection_id: connection.id, model: pick };
    };
    const payload: Agents = { architect: next("architect"), developer: next("developer"), reviewer: next("reviewer") };
    setSaving(true);
    setFailure(null);
    const result = await putAgents(payload);
    if (!mounted.current) return;
    setSaving(false);
    if (!result.ok) {
      setFailure(result.error);
      return;
    }
    appStore.setState((s) => ({ config: s.config === null ? null : { ...s.config, agents: result.data } }));
    onDone();
  }

  return (
    <section aria-labelledby={headingId} className="nd-form" data-role-suggestion={kind}>
      <h4 id={headingId} className="nd-section-label">
        {t("native.suggest.title")}
      </h4>
      {load.status === "loading" ? (
        <p role="status" className="nd-muted">
          {t("native.suggest.loading")}
        </p>
      ) : load.status === "error" ? (
        <p className="nd-field-warn">{load.message}</p>
      ) : load.status === "empty" ? (
        <p className="nd-field-warn">{t("native.suggest.empty")}</p>
      ) : (
        <>
          <p className="nd-field-hint">{t("native.suggest.hint", { provider: NATIVE_PROVIDERS[kind].brand })}</p>
          <div className="nd-suggest-grid">
            {ROLE_ORDER.map((role) => (
              <SuggestionPick
                key={role}
                role={role}
                models={load.models}
                value={load.picks[role]}
                onChange={(model) => setLoad({ ...load, picks: { ...load.picks, [role]: model } })}
              />
            ))}
          </div>
        </>
      )}
      {failure !== null ? (
        <p role="alert" className="nd-dialog-error">
          {failure.status === 422 ? t("settings.agents.unprocessable") : apiFailureText(failure)}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        <button type="button" className="nd-btn" onClick={onDone} disabled={saving} data-suggestion-skip="">
          {t("native.suggest.skip")}
        </button>
        {load.status === "ready" ? (
          <button type="button" className="nd-btn" data-variant="primary" disabled={saving} aria-busy={saving} data-suggestion-apply="" onClick={() => void apply()}>
            <Icon name="check" size={14} />
            {saving ? t("native.suggest.applying") : t("native.suggest.apply")}
          </button>
        ) : null}
      </div>
    </section>
  );
}

function SuggestionPick({ role, models, value, onChange }: { role: AgentRole; models: readonly ModelEntry[]; value: string; onChange: (model: string) => void }) {
  const { t } = useT();
  const id = useId();
  const info = ROLE_INFO[role];
  return (
    <div className="nd-field" data-suggestion-role={role}>
      <label htmlFor={id} className="nd-field-label">
        {info.label}
      </label>
      <select id={id} className="nd-input" value={value} aria-label={t("native.suggest.modelOf", { role: info.label })} onChange={(e) => onChange(e.target.value)}>
        <option value={KEEP}>{t("native.suggest.keep")}</option>
        {models.map((m) => (
          <option key={m.id} value={m.id}>
            {m.name !== undefined && m.name.trim() !== "" ? m.name : m.id}
          </option>
        ))}
      </select>
    </div>
  );
}
