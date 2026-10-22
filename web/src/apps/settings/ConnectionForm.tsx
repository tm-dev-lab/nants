/*
 * Formulário de conexão: usado
 * pelos Primeiros passos (T05) e pela janela Configurações (T08). A chave de API só existe no
 * campo e no corpo do envio: sai do estado logo no envio (sucesso ou erro) e nunca é exibida.
 * Validação espelha o servidor; 422 vira mensagem fixa.
 */
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { putConnection, type ApiFailure } from "../../api/client";
import type { ConnectionInput, ConnectionView, WireApi } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import {
  normalizeBaseUrl,
  validateConnectionForm,
  WIRE_API_OPTIONS,
  wireApiOf,
  type ConnectionFormValues,
  type FieldErrors,
} from "../../settings/rules";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore } from "../../store";


const EMPTY_FORM: ConnectionFormValues = { id: "", name: "", base_url: "", api_key: "" };

/** Insere ou substitui a conexão pelo id (mesma regra de upsert do servidor). */
function upsertConnection(list: readonly ConnectionView[], view: ConnectionView): ConnectionView[] {
  return list.some((c) => c.id === view.id) ? list.map((c) => (c.id === view.id ? view : c)) : [...list, view];
}

/** Valores iniciais de edição: a chave nunca é preenchida (vazia mantém a salva). */
function formFromView(view: ConnectionView): ConnectionFormValues {
  return { id: view.id, name: view.name, base_url: view.base_url, api_key: "" };
}

interface ConnectionFormProps {
  /** Ids já salvos: criar com um deles é recusado. */
  existingIds: readonly string[];
  /** Conexão a editar (identificador fixo). Ausente = criação. */
  initial?: ConnectionView;
  onSaved: (view: ConnectionView) => void;
  onCancel?: () => void;
}

function Field({ id, label, error, hint, hintId, children }: { id: string; label: string; error?: string; hint?: string; hintId?: string; children: ReactNode }) {
  return (
    <div className="nd-field">
      <label htmlFor={id} className="nd-field-label">
        {label}
      </label>
      {children}
      {hint !== undefined ? (
        <p id={hintId} className="nd-field-hint">
          {hint}
        </p>
      ) : null}
      {error !== undefined ? <p className="nd-field-error">{error}</p> : null}
    </div>
  );
}

export function ConnectionForm({ existingIds, initial, onSaved, onCancel }: ConnectionFormProps) {
  const errorId = useId();
  const keyHintId = useId();
  const protocolHintId = useId();
  const protocolName = useId();
  const ids = { id: useId(), name: useId(), url: useId(), key: useId() };
  const [form, setForm] = useState<ConnectionFormValues>(() => (initial !== undefined ? formFromView(initial) : EMPTY_FORM));
  const [wireApi, setWireApi] = useState<WireApi>(() => (initial !== undefined ? wireApiOf(initial) : "chat"));
  const [errors, setErrors] = useState<FieldErrors>({});
  const { t } = useT();
  // Falha do envio guardada como veio: o texto sai no idioma ativo na hora de exibir.
  const [submitFailure, setSubmitFailure] = useState<ApiFailure | null>(null);
  const submitError = submitFailure === null ? null : submitFailure.status === 422 ? t("settings.form.unprocessable") : apiFailureText(submitFailure);
  const [busy, setBusy] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  function update<K extends keyof ConnectionFormValues>(key: K, value: ConnectionFormValues[K]): void {
    setForm((f) => ({ ...f, [key]: value }));
  }

  async function submit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (busy) return;
    const id = form.id.trim();
    const isEdit = initial !== undefined || existingIds.includes(id);
    const found = validateConnectionForm(form, isEdit);
    if (initial === undefined && existingIds.includes(id)) found.id = t("settings.form.idTaken");
    if (initial !== undefined && form.api_key.trim() === "") {
      const oldUrl = normalizeBaseUrl(initial.base_url);
      const newUrl = normalizeBaseUrl(form.base_url);
      if (oldUrl !== null && newUrl !== null && oldUrl !== newUrl) found.api_key = t("settings.form.keyForNewUrl");
    }
    setErrors(found);
    setSubmitFailure(null);
    if (Object.keys(found).length > 0) return;
    const payload: ConnectionInput = {
      id,
      name: form.name.trim(),
      base_url: form.base_url.trim(),
      api_key: form.api_key.trim(),
      wire_api: wireApi,
    };
    // A chave sai do estado local já no envio.
    setForm((f) => ({ ...f, api_key: "" }));
    setBusy(true);
    const result = await putConnection(payload);
    if (!mounted.current) return;
    setBusy(false);
    if (!result.ok) {
      setSubmitFailure(result.error);
      return;
    }
    const view = result.data;
    appStore.setState((s) => ({
      config: s.config === null ? null : { ...s.config, connections: upsertConnection(s.config.connections, view) },
    }));
    onSaved(view);
  }

  return (
    <form className="nd-form" onSubmit={(event) => void submit(event)} noValidate aria-describedby={submitError !== null ? errorId : undefined} data-connection-form="">
      <Field id={ids.id} label={t("settings.form.id")} error={errors.id} hint={t("settings.form.idHint")}>
        <input
          id={ids.id}
          type="text"
          className="nd-input"
          value={form.id}
          onChange={(e) => update("id", e.target.value)}
          disabled={initial !== undefined}
          autoComplete="off"
          spellCheck={false}
        />
      </Field>
      <Field id={ids.name} label={t("settings.form.name")} error={errors.name}>
        <input id={ids.name} type="text" className="nd-input" value={form.name} onChange={(e) => update("name", e.target.value)} autoComplete="off" />
      </Field>
      <Field id={ids.url} label={t("settings.form.url")} error={errors.base_url} hint={t("settings.form.urlHint")}>
        <input
          id={ids.url}
          type="url"
          className="nd-input"
          value={form.base_url}
          onChange={(e) => update("base_url", e.target.value)}
          placeholder="https://api.openai.com/v1"
          autoComplete="off"
          spellCheck={false}
        />
      </Field>
      <fieldset className="nd-fieldset" aria-describedby={protocolHintId}>
        <legend className="nd-field-label">{t("settings.form.protocol")}</legend>
        <div role="radiogroup" aria-describedby={protocolHintId} className="nd-radio-grid">
          {WIRE_API_OPTIONS.map((option) => (
            <label key={option.value} className="nd-radio-card" data-checked={wireApi === option.value ? "true" : undefined}>
              <span className="nd-row">
                <input type="radio" name={protocolName} value={option.value} checked={wireApi === option.value} onChange={() => setWireApi(option.value)} />
                <span className="nd-strong">{option.label}</span>
              </span>
              <span className="nd-field-hint">{option.text}</span>
            </label>
          ))}
        </div>
        <p id={protocolHintId} className="nd-field-hint">
          {t("settings.form.protocolHint")}
        </p>
      </fieldset>
      <Field
        id={ids.key}
        label={t("settings.form.key")}
        error={errors.api_key}
        hintId={keyHintId}
        hint={t("settings.form.keyHint")}
      >
        <input
          id={ids.key}
          type="password"
          className="nd-input"
          value={form.api_key}
          onChange={(e) => update("api_key", e.target.value)}
          autoComplete="new-password"
          spellCheck={false}
          aria-describedby={keyHintId}
        />
      </Field>
      {submitError !== null ? (
        <p id={errorId} role="alert" className="nd-dialog-error">
          {submitError}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        {onCancel !== undefined ? (
          <button type="button" className="nd-btn" onClick={onCancel}>
            {t("common.cancel")}
          </button>
        ) : null}
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
          <Icon name="save" size={14} />
          {t("settings.form.save")}
        </button>
      </div>
    </form>
  );
}
