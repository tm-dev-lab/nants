/*
 * Painel "Conexões em uso" (spec 133 T03, desenhado na T02): os três papéis, cada um com a conexão
 * (o nome e o tipo), o modelo e o estado (disponível ou pausado por limite, com o motivo). Por
 * conexão, só nas que têm página própria, o link de uso ou de limites; no plano ChatGPT, a conta e
 * o novo login. Cada papel pode trocar a conexão ali mesmo, com o seletor do cartão de pausa.
 */
import { useId, useState } from "react";
import { putAgents } from "../../api/client";
import type { Agents, ConnectionView } from "../../api/types";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { ROLE_INFO, ROLE_ORDER, type AgentRole } from "../../settings/rules";
import { appStore } from "../../store";
import { pauseReasonOf, pauseReasonText } from "../board/boardModel";
import { planEmail, planState } from "../settings/chatgptPlan";
import { useChatgptStart } from "../settings/PlanConnection";
import { AgentAvatar } from "./AgentAvatar";
import { agentTone } from "./agents";
import { connectionKindLabel, providerPage, withRoleConnection } from "./roleConnection";
import { initialChoice, RoleConnectionSwitch, type RoleConnectionChoice } from "./RoleConnectionSwitch";

/** O papel que pausou a execução e o `code` da parada. */
export interface RolePause {
  role: AgentRole | null;
  code: string;
}

interface Props {
  agents: Agents;
  connections: readonly ConnectionView[];
  pause: RolePause | null;
  /** Execução em andamento: a troca de Desenvolvedor ou Revisor só vale quando ela recomeçar. */
  running: boolean;
  onClose: () => void;
}

export function ConnectionsInUse({ agents, connections, pause, running, onClose }: Props) {
  const { t } = useT();
  const shell = useShell();
  const titleId = useId();
  const hintId = useId();
  return (
    <Dialog titleId={titleId} title={t("roleConn.panelTitle")} describedById={hintId} onClose={onClose} width={600}>
      <div className="nd-stack" data-connections-in-use="">
        <p id={hintId} className="nd-muted">
          {t("roleConn.panelHint")}
        </p>
        <ul className="nd-plain-list nd-conn-use-list">
          {ROLE_ORDER.map((role) => (
            <RoleRow key={role} role={role} agents={agents} connections={connections} pause={pause !== null && pause.role === role ? pause.code : null} running={running} />
          ))}
        </ul>
      </div>
      <div className="nd-dialog-actions">
        <button
          type="button"
          className="nd-btn"
          data-variant="ghost"
          onClick={() => {
            onClose();
            shell.openSettings("agents");
          }}
        >
          <Icon name="settings" size={14} />
          {t("roleConn.openAgents")}
        </button>
      </div>
    </Dialog>
  );
}

function RoleRow({ role, agents, connections, pause, running }: { role: AgentRole; agents: Agents; connections: readonly ConnectionView[]; pause: string | null; running: boolean }) {
  const { t } = useT();
  const agent = agents[role];
  const conn = connections.find((c) => c.id === agent.connection_id) ?? null;
  const name = conn?.name ?? agent.connection_id;
  const kind = connectionKindLabel(conn?.kind);
  const reason = pause === null ? null : pauseReasonOf(pause);
  const isPlan = conn?.kind === "chatgpt_plan";
  const needsLogin = isPlan && conn !== null && planState(conn) !== "connected";
  const page = providerPage(conn, conn?.kind, pause === "run.billing" ? "billing" : "limits");
  const reconnect = useChatgptStart(isPlan && conn !== null ? conn.id : undefined);
  const [editing, setEditing] = useState<RoleConnectionChoice | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedWhileRunning, setSavedWhileRunning] = useState(false);

  async function save(): Promise<void> {
    if (editing === null || saving) return;
    setSaving(true);
    setError(null);
    const result = await putAgents(withRoleConnection(agents, role, editing.connectionId, editing.model));
    setSaving(false);
    if (!result.ok) {
      setError(apiFailureText(result.error));
      return;
    }
    appStore.setState((s) => ({ config: s.config === null ? null : { ...s.config, agents: result.data } }));
    setEditing(null);
    // O `RunJob` é montado na partida: a execução em andamento segue com a conexão de antes.
    setSavedWhileRunning(running && role !== "architect");
  }

  return (
    <li className="nd-conn-use" data-conn-use={role}>
      <div className="nd-conn-use-head">
        <AgentAvatar kind={role} decorative size="sm" />
        <span className="nd-agent-name" data-tone={agentTone(role)}>
          {ROLE_INFO[role].label}
        </span>
        <span className="nd-chat-card-spacer" />
        <span className="nd-conn-use-state" data-state={reason !== null ? "paused" : needsLogin ? "reauth" : "ok"}>
          <span className="nd-dot" data-tone={reason !== null || needsLogin ? "amber" : "green"} aria-hidden="true" />
          {reason !== null ? t("roleConn.state.paused", { reason: pauseReasonText(reason) }) : needsLogin ? t("chatgpt.state.needsLogin") : t("roleConn.state.ok")}
        </span>
      </div>
      <p className="nd-meta-line nd-conn-use-meta">
        <span className="nd-strong nd-conn-use-name">{name}</span>
        {kind !== name ? <span>{kind}</span> : null}
        {isPlan && conn !== null && planEmail(conn) !== "" ? <span>· {planEmail(conn)}</span> : null}
        <span>·</span>
        <span>{agent.model}</span>
      </p>
      {editing !== null ? (
        <div className="nd-stack-sm">
          <RoleConnectionSwitch role={role} agents={agents} connections={connections} value={editing} onChange={setEditing} disabled={saving} />
          {error !== null ? (
            <p role="alert" className="nd-dialog-error">
              {error}
            </p>
          ) : null}
          <div className="nd-actions nd-conn-use-actions">
            <button type="button" className="nd-btn nd-btn-sm" data-variant="primary" disabled={saving || editing.model.trim() === ""} aria-busy={saving} onClick={() => void save()}>
              <Icon name="check" size={14} />
              {t("roleConn.save")}
            </button>
            <button type="button" className="nd-btn nd-btn-sm" disabled={saving} onClick={() => setEditing(null)}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      ) : (
        <div className="nd-actions nd-conn-use-actions">
          <button type="button" className="nd-btn nd-btn-sm" data-conn-use-switch="" onClick={() => {
              setSavedWhileRunning(false);
              setEditing(initialChoice(role, agents, connections));
            }}>
            <Icon name="refresh" size={14} />
            {t("roleConn.switch")}
          </button>
          {needsLogin ? (
            <button type="button" className="nd-btn nd-btn-sm" disabled={reconnect.busy} onClick={() => reconnect.start()}>
              <Icon name="refresh" size={14} />
              {t("chatgpt.reconnect")}
            </button>
          ) : null}
          {page !== null ? (
            <a className="nd-btn nd-btn-sm" data-variant="ghost" href={page.url} target="_blank" rel="noopener noreferrer" data-conn-use-page="">
              <Icon name="external" size={14} />
              {page.label}
            </a>
          ) : null}
        </div>
      )}
      {savedWhileRunning ? (
        <p role="status" className="nd-muted" data-saved-running="">
          {t("roleConn.savedRunning")}
        </p>
      ) : null}
      {reconnect.notice}
    </li>
  );
}
