/*
 * Pedido de permissão como cartão da Conversa (095/T04: a mesma linguagem das mensagens, âmbar
 * enquanto espera): quem pediu (avatar e papel), tarefa e horário; pendente, o
 * comando (ou o motivo, o arquivo e o diff da mudança de ambiente) e as decisões honestas;
 * resolvido (aqui ou em qualquer outro lugar), a mesma mensagem vira o resultado, sem botões.
 *
 * A chegada de um pedido não move o foco (spec 093, decisão 10: nada rouba o foco). O aviso flutuante (`role="alert"`) anuncia o pedido.
 */
import { useId, useState, type ReactNode } from "react";
import { t } from "../../i18n";
import { useT } from "../../i18n/react";
import { isResolvedPermission, type PermissionEntry } from "../../permissionTimeline";
import type { PendingPermission, ResolvedPermission } from "../../store";
import { PermissionDecisions } from "../shared/PermissionDecisions";
import { outcomeText } from "../shared/permissions";
import { ChatCard, ChatCardHead } from "./ChatCard";

/** Comandos mais longos que isso começam truncados. */
export const COMMAND_PREVIEW_CHARS = 160;

/** Comando em texto puro, mono; longo começa truncado e expande sob demanda. */
function CommandBlock({ command }: { command: string }) {
  const [expanded, setExpanded] = useState(false);
  const { t } = useT();
  const long = command.length > COMMAND_PREVIEW_CHARS;
  const shown = long && !expanded ? command.slice(0, COMMAND_PREVIEW_CHARS) : command;
  return (
    <>
      <pre className="nd-command nd-mono" data-truncated={long && !expanded ? "true" : undefined}>
        {shown}
      </pre>
      {long ? (
        <button type="button" className="nd-link-btn" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
          {expanded ? t("permission.collapse") : t("permission.expand")}
        </button>
      ) : null}
    </>
  );
}

/** Diff unificado: texto puro, mono, rolável nos dois eixos. */
function DiffBlock({ diff }: { diff: string }) {
  const { t } = useT();
  return (
    <pre tabIndex={0} aria-label={t("permission.diffLabel")} className="nd-diff nd-mono">
      {diff}
    </pre>
  );
}

function MessageFrame({ entry, children, waiting = false }: { entry: PermissionEntry; children: ReactNode; waiting?: boolean }) {
  const titleId = useId();
  return (
    <ChatCard as="article" aria-labelledby={titleId} tone={waiting ? "amber" : undefined} data-permission-card="">
      <ChatCardHead
        agent={entry.agent}
        nameId={titleId}
        avatarState={waiting ? "waiting" : undefined}
        at={entry.at}
        extra={entry.taskId !== null ? <span className="nd-task-tag nd-mono">{entry.taskId}</span> : null}
      />
      <div className="nd-chat-card-body">{children}</div>
    </ChatCard>
  );
}

/** Título do pedido conforme o tipo (comando, ambiente, correção da verificação final). */
export function permissionTitle(request: { kind?: string | null }): string {
  if (request.kind === "environment") return t("permission.title.environment");
  if (request.kind === "final_fix") return t("permission.title.final_fix");
  return t("permission.title.command");
}

export function PermissionMessage({ projectId, entry }: { projectId: string; entry: PermissionEntry }) {
  // Título, resultado e hora no idioma ativo (assina a troca).
  useT();
  if (isResolvedPermission(entry)) return <ResolvedMessage entry={entry} />;
  return <PendingMessage projectId={projectId} request={entry} />;
}

function ResolvedMessage({ entry }: { entry: ResolvedPermission }) {
  const { formatTime } = useT();
  return (
    <MessageFrame entry={entry}>
      <div className="nd-permission" data-resolved="true">
        <CommandBlock command={entry.command} />
        <p className="nd-muted">
          {outcomeText(entry)}
          {" · "}
          <time dateTime={new Date(entry.resolvedAt).toISOString()}>{formatTime(entry.resolvedAt, { hour: "2-digit", minute: "2-digit" })}</time>
        </p>
      </div>
    </MessageFrame>
  );
}

function PendingMessage({ projectId, request }: { projectId: string; request: PendingPermission }) {
  const titleId = useId();
  const { t } = useT();
  const env = request.kind === "environment";
  return (
    <MessageFrame entry={request} waiting>
      <section aria-labelledby={titleId} className="nd-permission" data-pending="true">
        <h3 id={titleId} className="nd-permission-title">
          {permissionTitle(request)}
        </h3>
        {env ? (
          <>
            <p className="nd-break">
              <span className="nd-muted">{t("permission.reason")}</span>
              {request.reason ?? ""}
            </p>
            <p className="nd-mono nd-muted">{request.command}</p>
            <DiffBlock diff={request.diff ?? ""} />
          </>
        ) : (
          <CommandBlock command={request.command} />
        )}
        <PermissionDecisions projectId={projectId} request={request} />
      </section>
    </MessageFrame>
  );
}
