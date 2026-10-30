/*
 * Rodapé da Conversa com a conexão do papel com quem se fala (spec 133 T03, mockup da T02): o papel,
 * o modelo e a conexão (o nome e o tipo), para qualquer tipo de conexão. Tocar abre o painel
 * "Conexões em uso" com os três papéis. Substitui o selo "Usando o plano ChatGPT", que parecia
 * valer para o projeto inteiro.
 *
 * Spec 137 T05: cada linha fica numa linha só (o nome da conexão ganha reticências). O tipo só
 * aparece quando difere do nome. Durante a execução, uma segunda linha diz quem trabalha agora
 * ("agora: Desenvolvedor · T01 · modelo · conexão"); com um papel pausado, o alerta ocupa o lugar
 * dela. No celular, o email e o identificador saem do nome nas duas linhas; no desktop, só na
 * segunda.
 */
import { useState } from "react";
import { Icon } from "../../desktop/Icon";
import { useMobileMode } from "../../desktop/viewport";
import { useT } from "../../i18n/react";
import { ROLE_INFO, type AgentRole } from "../../settings/rules";
import { useAppStore } from "../../store";
import { runStopMessage } from "../board/boardModel";
import { agentTone } from "../shared/agents";
import { ConnectionsInUse, type RolePause } from "../shared/ConnectionsInUse";
import { activeWorker, connectionKindLabel, roleOf, shortConnectionName, showsConnectionKind } from "../shared/roleConnection";

interface Props {
  /** Papel com quem a Conversa fala. */
  role?: AgentRole;
  projectId: string;
  /** Spec com a execução parada (para dizer qual papel pausou e por quê), ou `null`. */
  pausedSpecId: string | null;
}

export function RoleConnectionFooter({ role = "architect", projectId, pausedSpecId }: Props) {
  const { t } = useT();
  const mobile = useMobileMode();
  const [open, setOpen] = useState(false);
  const agents = useAppStore((s) => s.config?.agents ?? null);
  const connections = useAppStore((s) => s.config?.connections ?? null);
  const running = useAppStore((s) => s.projectStates[projectId]?.running === true);
  const runSpecId = useAppStore((s) => s.projectStates[projectId]?.run?.spec_id ?? null);
  const specs = useAppStore((s) => s.projectStates[projectId]?.specs);
  const activities = useAppStore((s) => s.activities[projectId]);
  const stop = useAppStore((s) => (pausedSpecId === null ? null : runStopMessage(s.messages[projectId], pausedSpecId, s.runStarts[projectId])));
  if (agents === null || connections === null) return null;
  const agent = agents[role];
  const conn = connections.find((c) => c.id === agent.connection_id);
  const kind = connectionKindLabel(conn?.kind);
  const name = conn?.name ?? agent.connection_id;
  const shownName = mobile ? shortConnectionName(name) : name;
  const pause: RolePause | null = stop !== null && stop.code !== undefined ? { role: roleOf(stop.params?.role), code: stop.code } : null;
  const roleLabel = ROLE_INFO[role].label;
  const worker = running ? activeWorker(activities, specs, runSpecId) : null;
  let second = null;
  if (worker !== null && worker.role !== role) {
    const wAgent = agents[worker.role];
    const wConn = connections.find((c) => c.id === wAgent.connection_id);
    const wName = shortConnectionName(wConn?.name ?? wAgent.connection_id);
    const wRole = ROLE_INFO[worker.role].label;
    const label =
      worker.taskId === null
        ? t("roleConn.nowLabel", { role: wRole, model: wAgent.model, connection: wName })
        : t("roleConn.nowTaskLabel", { role: wRole, task: worker.taskId, model: wAgent.model, connection: wName });
    second = (
      <span className="nd-role-conn-now" data-role-conn-now={worker.role}>
        <span className="nd-sr-only">{label}</span>
        <span className="nd-dot" data-tone={agentTone(worker.role)} data-pulse="true" aria-hidden="true" />
        <span className="nd-role-conn-now-lead" aria-hidden="true">
          <span className="nd-muted">{t("roleConn.now")}</span> <span className="nd-strong">{wRole}</span>
          {worker.taskId !== null ? ` · ${worker.taskId}` : ""} ·
        </span>
        <span className="nd-role-conn-now-model" aria-hidden="true">
          {wAgent.model}
        </span>
        <span className="nd-role-conn-now-lead" aria-hidden="true">
          ·
        </span>
        <span className="nd-role-conn-now-name" aria-hidden="true">
          {wName}
        </span>
      </span>
    );
  } else if (pause !== null && pause.role !== null && pause.role !== role) {
    second = (
      <span className="nd-role-conn-alert" data-role-conn-paused={pause.role}>
        <span className="nd-dot" data-tone="amber" aria-hidden="true" />
        {t("roleConn.otherPaused", { role: ROLE_INFO[pause.role].label })}
      </span>
    );
  }
  return (
    <>
      <div className="nd-role-conn" data-role-conn-footer="">
        <button
          type="button"
          className="nd-link-btn nd-role-conn-trigger"
          aria-haspopup="dialog"
          aria-label={t("roleConn.footerLabel", { role: roleLabel, model: agent.model, connection: name })}
          onClick={() => setOpen(true)}
        >
          <span className="nd-dot" data-tone={agentTone(role)} aria-hidden="true" />
          <span className="nd-strong">{roleLabel}</span>
          <span aria-hidden="true">·</span>
          <span>{agent.model}</span>
          <span aria-hidden="true">·</span>
          <span className="nd-role-conn-name">{shownName}</span>
          {showsConnectionKind(name, kind) ? <span className="nd-muted">{kind}</span> : null}
          <Icon name="chevron-down" size={14} />
        </button>
        {second}
      </div>
      {open ? <ConnectionsInUse agents={agents} connections={connections} pause={pause} running={running} onClose={() => setOpen(false)} /> : null}
    </>
  );
}
