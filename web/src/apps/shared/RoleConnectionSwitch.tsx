/*
 * Seletor de troca da conexão de UM papel (spec 133 T02), usado no cartão de pausa por limite, no
 * de falha do provedor e no painel de conexões em uso (T03).
 *
 * Mostra as conexões prontas (a atual marcada) e o modelo da conexão escolhida, da lista dela
 * (`GET .../models`), com o modelo que outro papel já usa nessa conexão sugerido. Controlado: quem
 * usa decide quando gravar (`putAgents` com só esse papel mudado) e continuar.
 */
import { useEffect, useId, useState } from "react";
import { getConnectionModels } from "../../api/client";
import type { Agents, ConnectionView } from "../../api/types";
import { useT } from "../../i18n/react";
import { ROLE_INFO, type AgentRole } from "../../settings/rules";
import { AgentAvatar } from "./AgentAvatar";
import { agentTone } from "./agents";
import { connectionKindLabel, readyConnections, suggestedModel } from "./roleConnection";

export interface RoleConnectionChoice {
  connectionId: string;
  model: string;
}

/** Escolha inicial: a primeira conexão pronta diferente da atual, com o modelo sugerido. */
export function initialChoice(role: AgentRole, agents: Agents, connections: readonly ConnectionView[]): RoleConnectionChoice {
  const current = agents[role].connection_id;
  const other = readyConnections(connections).find((c) => c.id !== current);
  if (other === undefined) return { connectionId: current, model: agents[role].model };
  return { connectionId: other.id, model: suggestedModel(agents, other.id) };
}

interface Props {
  role: AgentRole;
  agents: Agents;
  connections: readonly ConnectionView[];
  value: RoleConnectionChoice;
  onChange: (next: RoleConnectionChoice) => void;
  disabled?: boolean;
}

export function RoleConnectionSwitch({ role, agents, connections, value, onChange, disabled = false }: Props) {
  const { t } = useT();
  const connId = useId();
  const modelId = useId();
  const ready = readyConnections(connections);
  const current = agents[role].connection_id;
  const [models, setModels] = useState<string[] | null>(null);

  useEffect(() => {
    let alive = true;
    setModels(null);
    if (value.connectionId === "") return;
    void getConnectionModels(value.connectionId).then((r) => {
      if (!alive) return;
      const ids = r.ok && typeof r.data.error !== "string" ? r.data.models.flatMap((m) => (typeof m?.id === "string" ? [m.id] : [])) : [];
      setModels(ids);
      if (value.model === "" && ids[0] !== undefined) onChange({ ...value, model: ids[0] });
    });
    return () => {
      alive = false;
    };
  }, [value.connectionId]);

  const options = models === null ? [] : value.model !== "" && !models.includes(value.model) ? [value.model, ...models] : models;
  const roleLabel = ROLE_INFO[role].label;

  return (
    <div className="nd-role-switch" data-role-switch={role}>
      <div className="nd-field">
        <label htmlFor={connId} className="nd-field-label">
          {t("roleConn.newConnection")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: roleLabel })}</span>
        </label>
        <select
          id={connId}
          className="nd-input"
          value={value.connectionId}
          disabled={disabled}
          onChange={(e) => onChange({ connectionId: e.target.value, model: suggestedModel(agents, e.target.value) })}
        >
          {ready.map((c) => {
            const kind = connectionKindLabel(c.kind);
            const label = kind === c.name ? c.name : `${c.name} (${kind})`;
            return (
              <option key={c.id} value={c.id}>
                {c.id === current ? t("roleConn.current", { name: label }) : label}
              </option>
            );
          })}
        </select>
      </div>
      <div className="nd-field">
        <label htmlFor={modelId} className="nd-field-label">
          {t("settings.agents.model")} <span className="nd-sr-only">{t("settings.agents.ofRole", { role: roleLabel })}</span>
        </label>
        <select
          id={modelId}
          className="nd-input"
          data-role-switch-model=""
          value={value.model}
          disabled={disabled || models === null}
          onChange={(e) => onChange({ ...value, model: e.target.value })}
        >
          {models === null ? <option value={value.model}>{t("settings.models.loading")}</option> : null}
          {options.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}

/** Papel, conexão (nome e tipo) e modelo, lado a lado no desktop e empilhados no celular. */
export function RoleFacts({ role, connectionName, connectionKind, model }: { role: AgentRole; connectionName: string; connectionKind: unknown; model: string }) {
  const { t } = useT();
  return (
    <dl className="nd-role-facts" data-role-facts={role}>
      <div>
        <dt>{t("roleConn.role")}</dt>
        <dd className="nd-role-facts-role">
          <AgentAvatar kind={role} decorative size="sm" />
          <span className="nd-agent-name" data-tone={agentTone(role)}>
            {ROLE_INFO[role].label}
          </span>
        </dd>
      </div>
      <div>
        <dt>{t("roleConn.connection")}</dt>
        <dd>
          <span className="nd-strong">{connectionName}</span> <span className="nd-muted">{connectionKindLabel(connectionKind)}</span>
        </dd>
      </div>
      <div>
        <dt>{t("roleConn.model")}</dt>
        <dd className="nd-break">{model}</dd>
      </div>
    </dl>
  );
}
