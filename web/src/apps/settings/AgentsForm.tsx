/*
 * Agentes: conexão, modelo e esforço por papel, salvos de uma
 * vez com `putAgents`, e "Usar o mesmo modelo para todos" (Primeiros passos, decisão 15):
 * Desenvolvedor e Revisor seguem a conexão e o modelo do Arquiteto (o esforço continua por papel).
 * Usado pelos Primeiros passos (T05) e pela janela Configurações (T08).
 *
 * O campo Modelo é um combobox: a lista vem de `GET .../models` da conexão salva (uma carga por
 * endpoint, compartilhada entre papéis); a entrada manual é sempre permitida.
 */
import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { getConnectionModels, putAgents, type ApiResult } from "../../api/client";
import type { AgentConfig, Agents, ConnectionView, Effort, ModelsResult } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import type { Tone } from "../../desktop/theme/tokens";
import {
  DEFAULT_MAX_STEPS,
  describeModelsError,
  EFFORT_OPTIONS,
  hasStepsLimit,
  MAX_STEPS_MAX,
  MAX_STEPS_MIN,
  MODELS_MESSAGES,
  parseMaxSteps,
  ROLE_INFO,
  ROLE_ORDER,
  validateModel,
  type AgentRole,
  type StepsRole,
} from "../../settings/rules";
import { t } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore } from "../../store";


const ROLE_TONE: Readonly<Record<AgentRole, Tone>> = { architect: "violet", developer: "cyan", reviewer: "amber" };

interface RoleErrors {
  connection?: string;
  model?: string;
  steps?: string;
}

type StepsText = Record<StepsRole, string>;

function stepsTextOf(agents: Agents): StepsText {
  return {
    developer: agents.developer.max_steps === undefined ? "" : String(agents.developer.max_steps),
    reviewer: agents.reviewer.max_steps === undefined ? "" : String(agents.reviewer.max_steps),
  };
}

function agentPayload(agent: AgentConfig, maxSteps: number | undefined): AgentConfig {
  const { max_steps: _previous, ...rest } = agent;
  const out: AgentConfig = { ...rest, model: agent.model.trim() };
  if (maxSteps !== undefined) out.max_steps = maxSteps;
  return out;
}

/** Sugestões do blueprint (Arquiteto low, Desenvolvedor low, Revisor medium). */
function suggestedEffort(role: AgentRole): Effort {
  return ROLE_INFO[role].defaultEffort;
}

/** Desenvolvedor e Revisor com a conexão e o modelo do Arquiteto (esforço e limite preservados). */
function withSameModel(agents: Agents): Agents {
  const { connection_id, model } = agents.architect;
  return {
    architect: agents.architect,
    developer: { ...agents.developer, connection_id, model },
    reviewer: { ...agents.reviewer, connection_id, model },
  };
}

function validateAgents(draft: Agents, connections: readonly ConnectionView[], steps: StepsText): Partial<Record<AgentRole, RoleErrors>> {
  const found: Partial<Record<AgentRole, RoleErrors>> = {};
  for (const role of ROLE_ORDER) {
    const agent = draft[role];
    const errors: RoleErrors = {};
    if (!connections.some((c) => c.id === agent.connection_id)) errors.connection = t("settings.agents.pickSaved");
    const model = validateModel(agent.model);
    if (model !== null) errors.model = model;
    if (hasStepsLimit(role)) {
      const parsed = parseMaxSteps(steps[role]);
      if (!parsed.ok) errors.steps = parsed.message;
    }
    if (Object.keys(errors).length > 0) found[role] = errors;
  }
  return found;
}

type ModelsEntry =
  | { status: "loading" }
  | { status: "ready"; models: string[]; truncated: boolean }
  | { status: "error"; message: string };

function endpointKey(connection: ConnectionView): string {
  return `${connection.id}|${connection.base_url}|${connection.api_key_masked}`;
}

function toModelsEntry(outcome: ApiResult<ModelsResult>): ModelsEntry {
  if (!outcome.ok) return { status: "error", message: t("settings.models.requestFailed", { message: apiFailureText(outcome.error) }) };
  const data = outcome.data;
  if (typeof data.error === "string") return { status: "error", message: describeModelsError(data.error, data.status) };
  const models = data.models.flatMap((model) => (typeof model?.id === "string" ? [model.id] : []));
  return { status: "ready", models, truncated: data.truncated === true };
}

interface AgentsFormProps {
  agents: Agents;
  connections: readonly ConnectionView[];
  onSaved?: (agents: Agents) => void;
}

export function AgentsForm({ agents, connections, onSaved }: AgentsFormProps) {
  const { t } = useT();
  const headingId = useId();
  const sameId = useId();
  const [draft, setDraft] = useState<Agents>(agents);
  const [same, setSame] = useState(false);
  const [steps, setSteps] = useState<StepsText>(() => stepsTextOf(agents));
  const [errors, setErrors] = useState<Partial<Record<AgentRole, RoleErrors>>>({});
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [models, setModels] = useState<ReadonlyMap<string, ModelsEntry>>(() => new Map());
  const entriesRef = useRef<Map<string, ModelsEntry>>(new Map());
  const tokensRef = useRef<Map<string, number>>(new Map());
  const activeRef = useRef<Set<string>>(new Set());
  const mountedRef = useRef(true);

  useEffect(() => {
    setDraft(agents);
    setSteps(stepsTextOf(agents));
  }, [agents]);

  // Com "mesmo modelo", o que vale é o Arquiteto.
  const effective = same ? withSameModel(draft) : draft;

  const activeConns = new Map<string, ConnectionView>();
  for (const role of ROLE_ORDER) {
    const conn = connections.find((c) => c.id === effective[role].connection_id);
    if (conn !== undefined) activeConns.set(endpointKey(conn), conn);
  }
  const savedKey = [...activeConns.keys()].sort().join("\n");

  function storeEntry(key: string, entry: ModelsEntry | null): void {
    if (entry === null) entriesRef.current.delete(key);
    else entriesRef.current.set(key, entry);
    setModels(new Map(entriesRef.current));
  }

  function loadModels(conn: ConnectionView, refresh: boolean): void {
    const key = endpointKey(conn);
    const token = (tokensRef.current.get(key) ?? 0) + 1;
    tokensRef.current.set(key, token);
    storeEntry(key, { status: "loading" });
    void getConnectionModels(conn.id, { refresh }).then((outcome) => {
      if (!mountedRef.current || tokensRef.current.get(key) !== token) return;
      if (!activeRef.current.has(key)) {
        storeEntry(key, null);
        return;
      }
      storeEntry(key, toModelsEntry(outcome));
    });
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const active = new Set(savedKey === "" ? [] : savedKey.split("\n"));
    activeRef.current = active;
    const keyById = new Map(connections.map((c) => [c.id, endpointKey(c)]));
    for (const [key, entry] of [...entriesRef.current]) {
      const id = key.split("|")[0] ?? key;
      const stale = keyById.has(id) && keyById.get(id) !== key;
      const droppable = stale || (active.has(key) ? entry.status === "error" : entry.status !== "ready");
      if (droppable) entriesRef.current.delete(key);
    }
    setModels(new Map(entriesRef.current));
    for (const [key, conn] of activeConns) {
      if (!entriesRef.current.has(key)) loadModels(conn, false);
    }
  }, [savedKey]);

  function patch(role: AgentRole, change: Partial<AgentConfig>): void {
    setDraft((d) => ({ ...d, [role]: { ...d[role], ...change } }));
    setSaved(false);
    if (change.connection_id !== undefined) {
      const conn = connections.find((c) => c.id === change.connection_id);
      if (conn !== undefined && entriesRef.current.get(endpointKey(conn))?.status === "error") loadModels(conn, false);
    }
  }

  async function save(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (saving) return;
    const found = validateAgents(effective, connections, steps);
    setErrors(found);
    setSaveError(null);
    setSaved(false);
    if (Object.keys(found).length > 0) return;
    const limit = (role: StepsRole): number | undefined => {
      const parsed = parseMaxSteps(steps[role]);
      return parsed.ok ? parsed.value : undefined;
    };
    const payload: Agents = {
      architect: { ...effective.architect, model: effective.architect.model.trim() },
      developer: agentPayload(effective.developer, limit("developer")),
      reviewer: agentPayload(effective.reviewer, limit("reviewer")),
    };
    setSaving(true);
    const result = await putAgents(payload);
    if (!mountedRef.current) return;
    setSaving(false);
    if (!result.ok) {
      setSaveError(result.error.status === 422 ? t("settings.agents.unprocessable") : apiFailureText(result.error));
      return;
    }
    appStore.setState((s) => ({ config: s.config === null ? null : { ...s.config, agents: result.data } }));
    setSaved(true);
    onSaved?.(result.data);
  }

  return (
    <section aria-labelledby={headingId} className="nd-form" data-agents-form="">
      <div className="nd-row nd-row-between">
        <div>
          <h3 id={headingId} className="nd-strong">
            {t("settings.agents.routing")}
          </h3>
          <p className="nd-field-hint">{t("settings.agents.suggestion")}</p>
        </div>
        <button
          type="button"
          className="nd-btn"
          data-variant="ghost"
          onClick={() =>
            setDraft((d) => ({
              architect: { ...d.architect, effort: suggestedEffort("architect") },
              developer: { ...d.developer, effort: suggestedEffort("developer") },
              reviewer: { ...d.reviewer, effort: suggestedEffort("reviewer") },
            }))
          }
        >
          {t("settings.agents.restoreEffort")}
        </button>
      </div>
      <label htmlFor={sameId} className="nd-check" data-same-model="">
        <input
          id={sameId}
          type="checkbox"
          checked={same}
          onChange={(event) => {
            setSame(event.target.checked);
            setSaved(false);
          }}
        />
        {t("settings.agents.sameModel")}
      </label>
      <form className="nd-form" onSubmit={(event) => void save(event)} noValidate>
        <div className="nd-role-grid">
          {ROLE_ORDER.map((role) => {
            const conn = connections.find((c) => c.id === effective[role].connection_id);
            const follows = same && role !== "architect";
            return (
              <RoleCard
                key={role}
                role={role}
                agent={effective[role]}
                connections={connections}
                errors={errors[role]}
                follows={follows}
                modelsEntry={conn !== undefined ? models.get(endpointKey(conn)) : undefined}
                onRefresh={conn !== undefined ? () => loadModels(conn, true) : undefined}
                onChange={(change) => patch(role, change)}
                {...(hasStepsLimit(role)
                  ? {
                      steps: steps[role],
                      onStepsChange: (text: string) => {
                        setSteps((current) => ({ ...current, [role]: text }));
                        setSaved(false);
                      },
                    }
                  : {})}
              />
            );
          })}
        </div>
        {saveError !== null ? (
          <p role="alert" className="nd-dialog-error">
            {saveError}
          </p>
        ) : null}
        {saved ? (
          <p role="status" className="nd-ok">
            <Icon name="check" size={14} />
            {t("settings.agents.saved")}
          </p>
        ) : null}
        <div className="nd-dialog-actions">
          <button type="submit" className="nd-btn" data-variant="primary" disabled={saving} aria-busy={saving}>
            <Icon name="save" size={14} />
            {saving ? t("settings.agents.saving") : t("settings.agents.save")}
          </button>
        </div>
      </form>
    </section>
  );
}

function RoleCard({
  role,
  agent,
  connections,
  errors,
  follows,
  modelsEntry,
  onRefresh,
  onChange,
  steps,
  onStepsChange,
}: {
  role: AgentRole;
  agent: AgentConfig;
  connections: readonly ConnectionView[];
  errors: RoleErrors | undefined;
  /** Segue o Arquiteto ("mesmo modelo para todos"): conexão e modelo travados. */
  follows: boolean;
  modelsEntry: ModelsEntry | undefined;
  onRefresh: (() => void) | undefined;
  onChange: (change: Partial<AgentConfig>) => void;
  steps?: string;
  onStepsChange?: (text: string) => void;
}) {
  const { t } = useT();
  const info = ROLE_INFO[role];
  const selectId = useId();
  const stepsId = useId();
  const stepsHelpId = useId();
  const modelId = useId();
  const effortName = useId();
  const missing = agent.connection_id !== "" && !connections.some((c) => c.id === agent.connection_id);
  const [switched, setSwitched] = useState(false);
  const loading = modelsEntry?.status === "loading";
  const ready = modelsEntry?.status === "ready" ? modelsEntry : null;
  const notListed = switched && ready !== null && !ready.truncated && agent.model.trim() !== "" && !ready.models.includes(agent.model);

  let hint: string | null = null;
  let warn = false;
  if (follows) hint = t("settings.agents.follows");
  else if (loading) hint = MODELS_MESSAGES.loading;
  else if (modelsEntry?.status === "error") {
    hint = modelsEntry.message;
    warn = true;
  } else if (ready !== null && ready.models.length === 0) hint = MODELS_MESSAGES.empty;
  else if (ready?.truncated === true) hint = MODELS_MESSAGES.truncated;
  else if (notListed) hint = MODELS_MESSAGES.notListed;

  return (
    <section aria-label={info.label} className="nd-role-card" data-role={role}>
      <h4 className="nd-role-title" data-tone={ROLE_TONE[role]}>
        {info.label}
      </h4>
      {missing ? (
        <p role="alert" className="nd-field-error">
          {t("settings.agents.connectionGone", { id: agent.connection_id })}
        </p>
      ) : null}
      <div className="nd-field">
        <label htmlFor={selectId} className="nd-field-label">
          {t("settings.agents.connection")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: info.label })}</span>
        </label>
        <select
          id={selectId}
          className="nd-input"
          value={agent.connection_id}
          disabled={follows}
          onChange={(event) => {
            setSwitched(true);
            onChange({ connection_id: event.target.value });
          }}
        >
          <option value="" disabled>
            {t("settings.agents.pickConnection")}
          </option>
          {connections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
        {errors?.connection !== undefined ? <p className="nd-field-error">{errors.connection}</p> : null}
      </div>
      <div className="nd-field">
        <div className="nd-row nd-row-between">
          <label htmlFor={modelId} className="nd-field-label">
            {t("settings.agents.model")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: info.label })}</span>
          </label>
          {modelsEntry !== undefined && !follows ? (
            <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" onClick={onRefresh} disabled={loading}>
              <Icon name="refresh" size={14} />
              {t("settings.agents.refreshList")}
            </button>
          ) : null}
        </div>
        <ModelCombobox
          id={modelId}
          value={agent.model}
          options={ready?.models ?? []}
          disabled={follows}
          onChange={(model) => {
            setSwitched(false);
            onChange({ model });
          }}
        />
        {hint !== null ? (
          <p aria-live="polite" className={warn ? "nd-field-warn" : "nd-field-hint"}>
            {hint}
          </p>
        ) : null}
        {errors?.model !== undefined ? <p className="nd-field-error">{errors.model}</p> : null}
      </div>
      <div className="nd-field">
        <span className="nd-field-label">{t("settings.agents.effort")}</span>
        <div role="radiogroup" aria-label={t("settings.agents.effortOf", { role: info.label })} className="nd-segment nd-chrome">
          {EFFORT_OPTIONS.map((option) => (
            <label key={option.value} className="nd-segment-item" data-checked={agent.effort === option.value ? "true" : undefined}>
              <input
                type="radio"
                name={effortName}
                value={option.value}
                checked={agent.effort === option.value}
                onChange={() => onChange({ effort: option.value })}
                className="nd-sr-only"
              />
              <span>{option.label}</span>
            </label>
          ))}
        </div>
      </div>
      {hasStepsLimit(role) && steps !== undefined && onStepsChange !== undefined ? (
        <div className="nd-field">
          <label htmlFor={stepsId} className="nd-field-label">
            {t("settings.agents.steps")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: info.label })}</span>
          </label>
          <input
            id={stepsId}
            type="number"
            inputMode="numeric"
            className="nd-input"
            min={MAX_STEPS_MIN}
            max={MAX_STEPS_MAX}
            step={1}
            value={steps}
            placeholder={String(DEFAULT_MAX_STEPS[role])}
            aria-describedby={stepsHelpId}
            aria-invalid={errors?.steps !== undefined ? true : undefined}
            onChange={(event) => onStepsChange(event.target.value)}
          />
          <p id={stepsHelpId} className="nd-field-hint">
            {t("settings.agents.stepsHint", { default: DEFAULT_MAX_STEPS[role], min: MAX_STEPS_MIN, max: MAX_STEPS_MAX })}
          </p>
          {errors?.steps !== undefined ? <p className="nd-field-error">{errors.steps}</p> : null}
        </div>
      ) : null}
    </section>
  );
}

/** Combobox do modelo (texto livre sempre aceito; a lista filtra pela digitação). */
function ModelCombobox({
  id,
  value,
  options,
  disabled,
  onChange,
}: {
  id: string;
  value: string;
  options: readonly string[];
  disabled: boolean;
  onChange: (model: string) => void;
}) {
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [typed, setTyped] = useState("");
  const query = typed.trim().toLowerCase();
  const filtered = query === "" ? [...options] : options.filter((m) => m.toLowerCase().includes(query));
  const visible = open && !disabled && filtered.length > 0;
  const current = visible && active < filtered.length ? active : -1;
  const optionId = (index: number): string => `${listId}-opt-${index}`;

  const close = (): void => {
    setOpen(false);
    setActive(-1);
    setTyped("");
  };
  const choose = (model: string): void => {
    onChange(model);
    close();
  };
  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>): void => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (filtered.length === 0) return;
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      if (!visible) {
        setOpen(true);
        setTyped("");
        setActive(delta > 0 ? 0 : filtered.length - 1);
        return;
      }
      const base = current < 0 ? (delta > 0 ? -1 : filtered.length) : current;
      setActive((base + delta + filtered.length) % filtered.length);
    } else if (event.key === "Enter") {
      if (visible) {
        event.preventDefault();
        if (current >= 0) choose(filtered[current] ?? value);
        else close();
      }
    } else if (event.key === "Escape") {
      if (visible) {
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    }
  };

  return (
    <div className="nd-combo">
      <input
        id={id}
        type="text"
        role="combobox"
        className="nd-input"
        aria-autocomplete="list"
        aria-expanded={visible}
        aria-controls={visible ? listId : undefined}
        aria-activedescendant={current >= 0 ? optionId(current) : undefined}
        value={value}
        disabled={disabled}
        onChange={(event) => {
          onChange(event.target.value);
          setOpen(true);
          setActive(-1);
          setTyped(event.target.value);
        }}
        onClick={() => {
          setOpen(true);
          setTyped("");
        }}
        onBlur={close}
        onKeyDown={onKeyDown}
        autoComplete="off"
        spellCheck={false}
      />
      {visible ? (
        <ul id={listId} role="listbox" aria-label={t("settings.agents.models")} className="nd-combo-list">
          {filtered.map((model, index) => (
            <li
              key={model}
              id={optionId(index)}
              role="option"
              aria-selected={model === value}
              data-active={index === current ? "true" : undefined}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(model)}
            >
              {model}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
