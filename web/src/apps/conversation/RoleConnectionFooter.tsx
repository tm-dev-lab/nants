/*
 * Rodapé da Conversa com a conexão do papel com quem se fala (spec 133 T03, mockup da T02): o papel,
 * o modelo e a conexão (o nome e o tipo), para qualquer tipo de conexão. Tocar abre o painel
 * "Conexões em uso" com os três papéis. Substitui o selo "Usando o plano ChatGPT", que parecia
 * valer para o projeto inteiro.
 */
import { useState } from "react";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { ROLE_INFO, type AgentRole } from "../../settings/rules";
import { useAppStore } from "../../store";
import { runStopMessage } from "../board/boardModel";
import { agentTone } from "../shared/agents";
import { ConnectionsInUse, type RolePause } from "../shared/ConnectionsInUse";
import { connectionKindLabel, roleOf } from "../shared/roleConnection";

interface Props {
  /** Papel com quem a Conversa fala. */
  role?: AgentRole;
  projectId: string;
  /** Spec com a execução parada (para dizer qual papel pausou e por quê), ou `null`. */
  pausedSpecId: string | null;
}

export function RoleConnectionFooter({ role = "architect", projectId, pausedSpecId }: Props) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const agents = useAppStore((s) => s.config?.agents ?? null);
  const connections = useAppStore((s) => s.config?.connections ?? null);
  const running = useAppStore((s) => s.projectStates[projectId]?.running === true);
  const stop = useAppStore((s) => (pausedSpecId === null ? null : runStopMessage(s.messages[projectId], pausedSpecId, s.runStarts[projectId])));
  if (agents === null || connections === null) return null;
  const agent = agents[role];
  const conn = connections.find((c) => c.id === agent.connection_id);
  const kind = connectionKindLabel(conn?.kind);
  const name = conn?.name ?? agent.connection_id;
  const pause: RolePause | null = stop !== null && stop.code !== undefined ? { role: roleOf(stop.params?.role), code: stop.code } : null;
  const roleLabel = ROLE_INFO[role].label;
  return (
    <>
      <p className="nd-role-conn" data-role-conn-footer="">
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
          <span>{name}</span>
          {kind !== name ? <span className="nd-muted">{kind}</span> : null}
          <Icon name="chevron-down" size={14} />
        </button>
        {pause !== null && pause.role !== null && pause.role !== role ? (
          <span className="nd-role-conn-alert" data-role-conn-paused={pause.role}>
            <span className="nd-dot" data-tone="amber" aria-hidden="true" />
            {t("roleConn.otherPaused", { role: ROLE_INFO[pause.role].label })}
          </span>
        ) : null}
      </p>
      {open ? <ConnectionsInUse agents={agents} connections={connections} pause={pause} running={running} onClose={() => setOpen(false)} /> : null}
    </>
  );
}
