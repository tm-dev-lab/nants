/*
 * Uma mensagem da Conversa (095/T04, decisão 7):
 * - usuário: "Você · hh:mm" (ou só "Você", C-16) acima da bolha; texto puro, imagens, "Reenviar";
 * - Arquiteto e agentes: cartão (`ChatCard`) com cabeçalho (avatar, nome, tarefa, hora) e corpo em
 *   Markdown restrito; o Arquiteto com perguntas com botões e o rodapé de ação opcional; o Revisor
 *   com veredito, apontamentos, observações, capturas e a matriz de critérios;
 * - sistema: cartão com a faixa rosa (pelo `code`, no idioma ativo; o aviso da queda da conexão
 *   com "Tentar de novo", spec 127 T01); pedido aprovado pela aprovação automática: pílula de evento.
 * Consecutivas da mesma fonte (agente, tarefa e tentativa) são agrupadas sem repetir o cabeçalho.
 */
import { memo, type ReactNode } from "react";
import { looksLikeChoicesAnswer, parseChoices } from "../../choices";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { isInfoSystemMessage, systemMessageText } from "../../i18n/serverCodes";
import { recoveryMessageText } from "../../recoveryAnalysis";
import { parseReviewMessage } from "../../reviewMessage";
import type { ChatMessage } from "../../store";
import { AgentAvatar } from "../shared/AgentAvatar";
import { CaptureThumbnails } from "../shared/CaptureThumbnails";
import { MarkdownText } from "../shared/ReportMarkdown";
import { ProviderEvidenceDetails } from "../shared/ProviderEvidenceDetails";
import {
  ReviewDecisionList,
  ReviewerChecklist,
  ReviewFindingList,
  ReviewPrefix,
  ReviewResponseList,
  ReviewStateSummary,
  ReviewVerdictBadge,
} from "../shared/ReviewMessage";
import { YoloBadge } from "../shared/YoloBadge";
import { ChatImageThumbs } from "./ChatAttachments";
import { ChatCard, ChatCardHead } from "./ChatCard";
import { userMeta } from "./chatTime";
import { ChoicesForm } from "./ChoicesForm";

/*
 * Aprovação automática: desde a spec 131 T06 o servidor manda o `code` `run.auto_approved` com o
 * pedido em `params.detail`. Os prefixos (`approvals::YOLO_AUTO_PREFIX` e a marca da rodada
 * autônoma da recuperação), sempre em pt-BR no texto gravado, só reconhecem as mensagens gravadas
 * antes; a pílula mostra o rótulo do dicionário. Os antigos, com a sigla, também.
 */
const AUTO_APPROVED_CODE = "run.auto_approved";
const AUTO_PREFIXES = ["aprovada automaticamente", "aprovado automaticamente (YOLO)"];
const ROUND_MARKER = "[rodada autônoma]";
const LEGACY_ROUND_MARKER = "[rodada autônoma YOLO]";

/**
 * Comando de uma aprovação automática (`params.detail`; na mensagem antiga, o texto após o
 * prefixo), ou `null` se não for uma.
 */
function parseYoloAuto(message: Pick<ChatMessage, "text" | "code" | "params">): string | null {
  if (message.code === AUTO_APPROVED_CODE) {
    const detail = message.params?.detail;
    return typeof detail === "string" ? detail.trim() : "";
  }
  const text = message.text;
  const prefix = AUTO_PREFIXES.find((p) => text.startsWith(p));
  if (prefix === undefined) return null;
  return text.slice(prefix.length).replace(/^\s*:\s*/, "").trim();
}

/**
 * Texto do Arquiteto com a marca da rodada autônoma (a antiga e a atual, gravadas pelo servidor em
 * pt-BR) trocada pela marca no idioma ativo: a marca é texto do Nants, não conteúdo do agente.
 */
function withCurrentRoundMarker(text: string, marker: string): string {
  return text.split(LEGACY_ROUND_MARKER).join(ROUND_MARKER).split(ROUND_MARKER).join(marker);
}

/** Spec das capturas de uma mensagem do Revisor (da carga, da chegada ou a ativa). */
export function captureSpecOf(message: ChatMessage, activeSpecId: string | null): string | null {
  return message.specId ?? message.arrivedUnderSpec ?? activeSpecId;
}

/** Mesma fonte visual (agente, tarefa e tentativa) que a anterior. */
export function sameSource(a: ChatMessage | undefined, b: ChatMessage): boolean {
  return a !== undefined && a.role === "agent" && b.role === "agent" && a.agent === b.agent && a.taskId === b.taskId && a.attempt === b.attempt;
}

/** Estado das perguntas de uma mensagem do Arquiteto (bloco nants-choices). */
interface ChoicesState {
  interactive: boolean;
  answered: string | null;
  unanswered: boolean;
  disabled: boolean;
  onSubmit: (text: string) => void;
}

/** Resposta às perguntas: a primeira mensagem do usuário depois da do Arquiteto. */
export function answerAfter(shown: readonly ChatMessage[], id: number, text: string): { answered: string | null; unanswered: boolean } {
  const questions = parseChoices(text).questions ?? [];
  const at = shown.findIndex((m) => m.id === id);
  const next = shown.slice(at + 1).find((m) => m.role === "user");
  if (next === undefined) return { answered: null, unanswered: false };
  if (next.fromChoices === true || looksLikeChoicesAnswer(questions, next.text)) return { answered: next.text, unanswered: false };
  return { answered: null, unanswered: true };
}

interface BubbleProps {
  message: ChatMessage;
  grouped: boolean;
  projectId: string;
  activeSpecId: string | null;
  onResend?: ((message: ChatMessage) => void) | undefined;
  /** "Reenviar" desabilitado (prontidão do chat bloqueada ou envio em andamento). */
  resendDisabled?: boolean | undefined;
  choices?: ChoicesState | undefined;
  /** Rodapé de ação do cartão (o "Plano pronto · Abrir" na última mensagem do Arquiteto). */
  footer?: ReactNode;
  /**
   * "Tentar de novo" do aviso da queda da conexão (spec 127 T01): só no aviso mais recente da
   * Conversa. Ausente = sem botão.
   */
  onRetryTurn?: (() => void) | undefined;
  retryDisabled?: boolean | undefined;
}

function BubbleView({
  message,
  grouped,
  projectId,
  activeSpecId,
  onResend,
  resendDisabled = false,
  choices,
  footer,
  onRetryTurn,
  retryDisabled = false,
}: BubbleProps) {
  // Assina o idioma: a bolha é `memo` e precisa re-renderizar na troca.
  const { t, lang } = useT();
  if (message.role === "user") {
    return (
      <div className="nd-msg-user">
        <p className="nd-msg-user-meta" data-user-meta="">
          {userMeta(message.at)}
        </p>
        <p className="nd-bubble-user">{message.text}</p>
        {message.attachments !== undefined && message.attachments.length > 0 ? <ChatImageThumbs projectId={projectId} names={message.attachments} /> : null}
        {message.unsent !== undefined ? (
          <p role="alert" className="nd-unsent">
            <span>{message.unsent}</span>
            <button type="button" className="nd-btn nd-btn-sm" disabled={resendDisabled} onClick={() => onResend?.(message)}>
              <Icon name="refresh" size={14} />
              {t("conversation.resend")}
            </button>
          </p>
        ) : null}
      </div>
    );
  }
  if (message.role === "system") {
    // Spec 131 T03: o aviso que só informa sai no tom neutro; o resto continua como alerta.
    const info = isInfoSystemMessage(message);
    return (
      <ChatCard tone={info ? undefined : "rose"} data-system-message={info ? "info" : ""}>
        <div className="nd-chat-card-body nd-chat-card-row">
          <AgentAvatar kind="system" decorative size="sm" />
          <span className="nd-sr-only">{t("conversation.systemPrefix")}</span>
          <div className="nd-stack-xs">
            <p role="status" className="nd-system-text" data-tone={info ? "info" : undefined}>
              {systemMessageText(message)}
            </p>
            <ProviderEvidenceDetails code={message.code} params={message.params} />
            {onRetryTurn !== undefined ? (
              <div className="nd-row">
                <button type="button" className="nd-btn nd-btn-sm" data-retry-turn="" disabled={retryDisabled} onClick={onRetryTurn}>
                  <Icon name="refresh" size={14} />
                  {t("common.tryAgain")}
                </button>
              </div>
            ) : null}
          </div>
        </div>
      </ChatCard>
    );
  }
  const yoloAuto = message.role === "agent" ? parseYoloAuto(message) : null;
  if (yoloAuto !== null) {
    // Evento de agente: pílula central entre linhas (095/T04).
    return (
      <div className="nd-event-line" data-testid="yolo-auto">
        <span className="nd-event-pill">
          <Icon name="zap" size={14} />
          <span>{t("conversation.autoApproved")}</span>
          {yoloAuto !== "" ? <code className="nd-mono nd-break">{yoloAuto}</code> : null}
        </span>
      </div>
    );
  }
  const agent = message.role === "architect" ? "architect" : (message.agent ?? "");
  // Arquiteto no chat (role architect) ou no diálogo da tarefa (agente architect, ex.: análise de
  // recuperação): a marca da rodada autônoma vira o selo e o texto fixo do servidor, o idioma ativo.
  const fromArchitect = agent === "architect";
  const yoloRound = fromArchitect && (message.text.includes(ROUND_MARKER) || message.text.includes(LEGACY_ROUND_MARKER));
  const checklist = message.role === "agent" && agent === "reviewer" ? parseReviewMessage(message.text) : null;
  const captureSpec = checklist !== null ? captureSpecOf(message, activeSpecId) : null;
  // Mensagem do sistema com `code` (spec 102): pelo dicionário no idioma atual; o resto é conteúdo.
  const marker = `[${t("conversation.roundMarker")}]`;
  const body =
    message.role === "agent" && message.agent === "system"
      ? systemMessageText(message)
      : fromArchitect
        ? withCurrentRoundMarker(recoveryMessageText(message, lang), marker)
        : message.text;
  const parsed = message.role === "architect" ? parseChoices(body) : { text: body, questions: null };
  const showChoices = parsed.questions !== null && choices !== undefined && (choices.interactive || choices.answered !== null || choices.unanswered);
  return (
    <ChatCard data-grouped={grouped ? "true" : undefined} data-agent-card={agent}>
      {grouped ? null : (
        <ChatCardHead
          agent={agent}
          at={message.at}
          extra={
            <>
              {yoloRound ? <YoloBadge /> : null}
              {message.taskId !== undefined ? (
                <span className="nd-task-tag nd-mono">
                  {message.taskId}
                  {message.attempt !== undefined && message.attempt > 1 ? ` · ${t("conversation.attempt", { attempt: message.attempt })}` : ""}
                </span>
              ) : null}
            </>
          }
        />
      )}
      <div className="nd-chat-card-body nd-bubble">
        {checklist !== null && checklist.prefix !== "" ? <ReviewPrefix prefix={checklist.prefix} /> : null}
        {checklist !== null && checklist.verdict !== null ? <ReviewVerdictBadge verdict={checklist.verdict} /> : null}
        {checklist !== null ? <ReviewStateSummary review={checklist} /> : null}
        <MarkdownText text={checklist?.summary ?? parsed.text} keepLineBreaks={message.role === "agent"} />
        {message.role === "agent" && message.agent === "system" ? <ProviderEvidenceDetails code={message.code} params={message.params} /> : null}
        {checklist !== null ? <ReviewFindingList findings={checklist.findings} kind="blocking" /> : null}
        {checklist !== null ? <ReviewFindingList findings={checklist.observations} kind="observation" /> : null}
        {checklist !== null ? <ReviewFindingList findings={checklist.resolved} kind="resolved" /> : null}
        {checklist !== null ? <ReviewResponseList responses={checklist.responses} /> : null}
        {checklist !== null ? <ReviewDecisionList decisions={checklist.decisions} /> : null}
        {checklist !== null && checklist.captures.length > 0 && captureSpec !== null ? (
          <CaptureThumbnails projectId={projectId} specId={captureSpec} names={checklist.captures} />
        ) : null}
        {checklist !== null && checklist.items.length > 0 ? <ReviewerChecklist items={checklist.items} /> : null}
        {showChoices && parsed.questions !== null && choices !== undefined ? (
          <ChoicesForm
            questions={parsed.questions}
            answered={choices.answered}
            unanswered={choices.unanswered && !choices.interactive}
            disabled={choices.disabled}
            onSubmit={choices.onSubmit}
          />
        ) : null}
      </div>
      {footer ?? null}
    </ChatCard>
  );
}

export const Bubble = memo(BubbleView);
