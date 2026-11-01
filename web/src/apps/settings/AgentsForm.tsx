/*
 * Agentes: conexão, modelo e esforço por papel, salvos de uma
 * vez com `putAgents`, e "Usar o mesmo modelo para todos" (Primeiros passos, decisão 15):
 * Desenvolvedor e Revisor seguem a conexão e o modelo do Arquiteto (o esforço continua por papel).
 * Usado pelos Primeiros passos (T05) e pela janela Configurações (T08).
 *
 * O esforço tem três níveis e, abaixo, uma dica curta pelo provedor da conexão do papel (spec 132).
 *
 * O campo Modelo é um combobox: a lista vem de `GET .../models` da conexão salva (uma carga por
 * endpoint, compartilhada entre papéis); a entrada manual é sempre permitida.
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { putAgents } from "../../api/client";
import type { AgentConfig, Agents, ConnectionView, Effort } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import type { Tone } from "../../desktop/theme/tokens";
import {
  connectionKindOf,
  DEFAULT_MAX_STEPS,
  EFFORT_OPTIONS,
  effortHint,
  hasStepsLimit,
  MAX_STEPS_MAX,
  MAX_STEPS_MIN,
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
import { ModelCombobox } from "../shared/ModelCombobox";
import { modelsHint, useConnectionModels, type ModelsEntry } from "../shared/useConnectionModels";


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
  const mountedRef = useRef(true);

  useEffect(() => {
    setDraft(agents);
    setSteps(stepsTextOf(agents));
  }, [agents]);

  // Com "mesmo modelo", o que vale é o Arquiteto.
  const effective = same ? withSameModel(draft) : draft;

  const models = useConnectionModels(
    ROLE_ORDER.flatMap((role) => connections.filter((c) => c.id === effective[role].connection_id)),
    connections,
  );

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  function patch(role: AgentRole, change: Partial<AgentConfig>): void {
    setDraft((d) => ({ ...d, [role]: { ...d[role], ...change } }));
    setSaved(false);
    if (change.connection_id !== undefined) {
      const conn = connections.find((c) => c.id === change.connection_id);
      if (conn !== undefined) models.retryFailed(conn);
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
                modelsEntry={models.entryOf(conn)}
                onRefresh={conn !== undefined ? () => models.load(conn, true) : undefined}
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
  const conn = connections.find((c) => c.id === agent.connection_id);
  const missing = agent.connection_id !== "" && conn === undefined;
  const [switched, setSwitched] = useState(false);
  const loading = modelsEntry?.status === "loading";
  const ready = modelsEntry?.status === "ready" ? modelsEntry : null;
  const notListed = switched && ready !== null && !ready.truncated && agent.model.trim() !== "" && !ready.models.some((m) => m.id === agent.model);

  const hint = follows ? { text: t("settings.agents.follows"), warn: false } : modelsHint(modelsEntry, notListed);

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
        {/* Linha do rótulo na altura do rótulo (113/T05): a ação discreta não empurra o campo. */}
        <div className="nd-field-labelrow">
          <label htmlFor={modelId} className="nd-field-label">
            {t("settings.agents.model")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: info.label })}</span>
          </label>
          {modelsEntry !== undefined && !follows ? (
            <button type="button" className="nd-btn nd-label-action" data-variant="ghost" onClick={onRefresh} disabled={loading}>
              <Icon name="refresh" size={14} />
              <span>{t("settings.agents.refreshList")}</span>
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
          <p aria-live="polite" className={hint.warn ? "nd-field-warn" : "nd-field-hint"}>
            {hint.text}
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
        {conn !== undefined ? (
          <p className="nd-field-hint" data-effort-hint={connectionKindOf(conn)}>
            {effortHint(connectionKindOf(conn))}
          </p>
        ) : null}
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
