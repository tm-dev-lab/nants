/*
 * Linguagem de cartão da Conversa (095/T04, decisão 7; `reference/Main.dc.html`):
 * - `ChatCard`: fundo `--lane` com o relevo `--card-edge` e raio 12, sem contorno; o tom opcional
 *   só marca o pedido que espera você (âmbar) ou a falha (rosa) com uma faixa fina à esquerda;
 * - `ChatCardHead`: avatar, nome no tom do papel, extras (tarefa, selos) e a hora à direita
 *   ("09:14", com o tempo relativo no `title`), separado do corpo por `--divider`;
 * - corpo (`nd-chat-card-body`) e rodapé de ação (`nd-chat-card-foot`, também com `--divider`).
 * Arquiteto, agentes (Revisor incluso), sistema, permissões, relatório, pausa e escalada usam o
 * mesmo cartão.
 */
import type { HTMLAttributes, ReactNode } from "react";
import { relativeTime } from "../../copy";
import { useLanguage } from "../../i18n/react";
import { AgentAvatar } from "../shared/AgentAvatar";
import { agentLabel, agentTone } from "../shared/agents";
import { clockLabel, hasTime } from "./chatTime";

type CardTone = "amber" | "rose";

interface ChatCardProps extends HTMLAttributes<HTMLElement> {
  as?: "div" | "article" | "section";
  tone?: CardTone | undefined;
  children: ReactNode;
}

export function ChatCard({ as: Tag = "div", tone, className, children, ...rest }: ChatCardProps) {
  return (
    <Tag {...rest} className={className === undefined ? "nd-chat-card" : `nd-chat-card ${className}`} data-card-tone={tone}>
      {children}
    </Tag>
  );
}

interface HeadProps {
  /** Papel (avatar e tom do nome). */
  agent: string;
  /** Nome exibido; padrão: o rótulo do papel. */
  label?: ReactNode;
  /** Id do nome (para `aria-labelledby` do cartão). */
  nameId?: string | undefined;
  /** Tarefa, selos e outros extras depois do nome. */
  extra?: ReactNode;
  /** Instante da mensagem; sem hora, o cabeçalho não mostra hora. */
  at?: number | null | undefined;
  avatarState?: "thinking" | "working" | "waiting" | undefined;
}

export function ChatCardHead({ agent, label, nameId, extra, at, avatarState }: HeadProps) {
  // Nome do papel, hora e tempo relativo no idioma ativo.
  useLanguage();
  return (
    <div className="nd-chat-card-head">
      <AgentAvatar kind={agent} decorative size="sm" state={avatarState} />
      <span {...(nameId !== undefined ? { id: nameId } : {})} className="nd-agent-name" data-tone={agentTone(agent)}>
        {label ?? agentLabel(agent)}
      </span>
      {extra}
      <span className="nd-chat-card-spacer" />
      {hasTime(at) ? (
        <time dateTime={new Date(at).toISOString()} title={relativeTime(at, Date.now())} className="nd-time">
          {clockLabel(at)}
        </time>
      ) : null}
    </div>
  );
}
