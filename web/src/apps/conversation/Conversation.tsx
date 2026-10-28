/*
 * Janela Conversa (spec 093, decisão 3):
 * - linha do tempo cronológica: chat do Arquiteto, diálogo dos agentes de todas as tarefas da spec
 *   ativa (consecutivas da mesma fonte agrupadas) e pedidos de permissão (pendentes e resolvidos);
 * - perguntas com botões, relatório final, execução pausada ou parada com as opções de escalada,
 *   verificação final, bolhas de atividade e "pensando";
 * - desenho enxuto da 095/T04 (`Main.dc.html`): separador de dia, "Você · hh:mm" acima da bolha,
 *   Arquiteto e agentes em cartão (`ChatCard`), eventos dos agentes como pílula entre linhas e o
 *   composer separado por divisória;
 * - "Plano pronto · N tarefas · revisado · Abrir" no rodapé da última mensagem do Arquiteto: o
 *   plano em si fica na janela Plano;
 * - campo com anexos (colar, arrastar e soltar, botão), "N novas" e a rolagem restaurada;
 * - sem `readiness.chat`, o campo fica desabilitado com o que falta e "Configurar".
 *
 * O arrasto de arquivos sobre a janela não chega ao desktop: os eventos de arrastar e soltar param
 * aqui, e o gesto de mover janela (ponteiro na barra de título) não participa do arrasto nativo.
 */
import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type DragEvent as ReactDragEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { postChat, postChatContinue, type ApiFailure } from "../../api/client";
import type { SpecView } from "../../api/types";
import { chatFailureMessage } from "../../chatFailure";
import { loadChatHistory } from "../../chatHistory";
import { parseChoices } from "../../choices";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { interleave, pendingKey, permissionEntries, usePendingPin } from "../../permissionTimeline";
import { refreshProjectState } from "../../projectState";
import { architectWireApi } from "../../settings/rules";
import { appStore, belongsToSpec, useAppStore, type ActivityEntry, type ChatMessage, type PendingPermission, type ResolvedPermission } from "../../store";
import { useKeyboardReveal } from "../../desktop/viewport";
import { t as translateNow, tp as tpNow } from "../../i18n";
import { useT } from "../../i18n/react";
import { ActivityBubbles, type ActivityView } from "../shared/ActivityBubble";
import { FinalVerificationNotice } from "../shared/FinalVerificationNotice";
import { gateLevel, prereqFailureMessage } from "../shared/readiness";
import { ReadinessNotice } from "../shared/ReadinessNotice";
import "../shared/apps.css";
import { AutoScrollBox, type ScrollPosition } from "./AutoScrollBox";
import { answerAfter, Bubble, sameSource } from "./Bubble";
import { AttachmentPreview, imageFiles, useChatAttachments } from "./ChatAttachments";
import { setChatSending, takeChatRequest, useChatRequestCount } from "./chatChannel";
import { dayKey, dayLabel, hasTime } from "./chatTime";
import { PausedRunCard } from "./PausedRunCard";
import { RoleConnectionFooter } from "./RoleConnectionFooter";
import { runStopMessage } from "../board/boardModel";
import { PROVIDER_STOP_CODES, providerEvidence } from "../../i18n/providerEvidence";
import { PermissionMessage } from "./PermissionMessage";
import { ResultMessage } from "./ResultMessage";

const ABANDON_GRACE_MS = 12_000;
const SCROLL_ATTRS = { "data-chat-scroll": "" } as const;
const NO_PENDING: readonly PendingPermission[] = [];
const NO_RESOLVED: readonly ResolvedPermission[] = [];
const NO_ACTIVITY: readonly ActivityEntry[] = [];
/**
 * Rascunho da mensagem por projeto: só na memória da aba (decisão 2; nunca no servidor nem no
 * storage). Sobrevive a fechar e reabrir a janela e a trocar de espaço; some ao recarregar.
 */
const drafts = new Map<string, string>();

function loadDraft(projectId: string): string {
  return drafts.get(projectId) ?? "";
}

function saveDraft(projectId: string, text: string): void {
  if (text === "") drafts.delete(projectId);
  else drafts.set(projectId, text);
}

function byTime(messages: readonly ChatMessage[]): ChatMessage[] {
  return messages
    .map((message, index) => ({ message, index }))
    .sort((a, b) => a.message.at - b.message.at || a.index - b.index)
    .map((entry) => entry.message);
}

function isConnectionLost(error: ApiFailure): boolean {
  return error.kind === "network";
}

function isTurnBusy(error: ApiFailure): boolean {
  return error.status === 409 && error.reason === undefined;
}

/** Avisos da queda da conexão no meio do turno (spec 127 T01): ganham "Tentar de novo". */
const CONNECTION_LOST_CODES: ReadonlySet<string> = new Set(["chat.connection_lost", "chat.connection_lost_saved"]);

/** Turno que caiu com o aviso já gravado no histórico: a tela o lê de lá, sem repetir aqui. */
function noticeRecorded(error: ApiFailure): boolean {
  return error.reason === "connection_lost" || error.reason === "failure_recorded";
}

interface PendingSend {
  controller: AbortController;
  messageId: number;
  abandoned: boolean;
}

/** Posição salva no ui-state (`selection.conversationScroll`) como posição da rolagem. */
export function scrollFromSelection(value: number | null): ScrollPosition | null {
  return value === null ? null : { atBottom: false, top: value };
}

interface ConversationProps {
  projectId: string;
  messages: readonly ChatMessage[];
  spec?: SpecView | null;
  /** Plano pronto para aprovar: só na etapa "Aguardando sua aprovação" (`SpaceData.planReady`). */
  planReady?: SpecView | null;
  onDraftUpdated: () => Promise<void>;
  pending?: readonly PendingPermission[];
  resolved?: readonly ResolvedPermission[];
  activities?: readonly ActivityEntry[];
  dialogError?: boolean;
  onRetryDialog?: () => void;
  reportPending?: boolean;
  pausedRun?: boolean;
  /** Turno acompanhado depois da queda do POST (`useSpaceData`). */
  tracking?: boolean;
  onTrackTurn?: (messageId: number | null) => void;
  /** Rolagem salva (lida na montagem) e o registro das mudanças. */
  initialScroll?: ScrollPosition | null;
  onScrollChange?: (position: ScrollPosition) => void;
  /** Pedido do lançador ("Pedir ao Arquiteto"): vai para o campo. */
  request?: { id: number; text: string } | null;
  onRequestConsumed?: (id: number) => void;
}

export function Conversation({
  projectId,
  messages,
  spec = null,
  planReady = null,
  onDraftUpdated,
  pending = NO_PENDING,
  resolved = NO_RESOLVED,
  activities = NO_ACTIVITY,
  dialogError = false,
  onRetryDialog,
  reportPending = false,
  pausedRun = false,
  tracking = false,
  onTrackTurn,
  initialScroll = null,
  onScrollChange,
  request = null,
  onRequestConsumed,
}: ConversationProps) {
  const shell = useShell();
  const { t } = useT();
  const inputId = useId();
  const hintId = useId();
  const [draft, setDraftState] = useState(() => loadDraft(projectId));
  const setDraft = useCallback(
    (text: string) => {
      setDraftState(text);
      saveDraft(projectId, text);
    },
    [projectId],
  );
  const [sending, setSending] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const [dragging, setDragging] = useState(false);
  const dragDepth = useRef(0);
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const images = useChatAttachments(projectId);
  const [sentCount, setSentCount] = useState(0);
  const architectWire = architectWireApi(useAppStore((s) => s.config));
  const remoteTurn = useAppStore((s) => s.architectTurns[projectId]);
  const chatLevel = gateLevel(shell.readiness, "chat");
  const chatBlocked = chatLevel !== null && !chatLevel.ok;

  useEffect(() => {
    const area = areaRef.current;
    if (area === null) return;
    area.style.height = "auto";
    area.style.height = `${area.scrollHeight}px`;
  }, [draft]);

  useEffect(() => {
    setChatSending(projectId, sending || preparing);
  }, [projectId, sending, preparing]);
  useEffect(() => () => setChatSending(projectId, false), [projectId]);

  // Pedido do lançador: o texto vai para o campo, que recebe o foco (foi o usuário que pediu).
  useEffect(() => {
    if (request === null) return;
    setDraft(request.text);
    onRequestConsumed?.(request.id);
    areaRef.current?.focus({ preventScroll: true });
  }, [request, setDraft, onRequestConsumed]);

  // "pensando" segue o servidor; o POST pendurado depois do fim do turno é abandonado.
  const [turnEndedWhileSending, setTurnEndedWhileSending] = useState(false);
  const sawTurnWhileSendingRef = useRef(false);
  useEffect(() => {
    if (!sending) {
      sawTurnWhileSendingRef.current = false;
      return;
    }
    if (remoteTurn !== undefined) sawTurnWhileSendingRef.current = true;
    else if (sawTurnWhileSendingRef.current) setTurnEndedWhileSending(true);
  }, [sending, remoteTurn]);
  const thinking = remoteTurn !== undefined || (sending && !turnEndedWhileSending);
  const pendingSendRef = useRef<PendingSend | null>(null);
  const graceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const current = pendingSendRef.current;
    if (!turnEndedWhileSending || current === null) return;
    const timer = setTimeout(() => {
      graceTimerRef.current = null;
      if (pendingSendRef.current !== current) return;
      current.abandoned = true;
      pendingSendRef.current = null;
      current.controller.abort();
      const own = appStore.getState().messages[projectId]?.find((m) => m.id === current.messageId);
      if (own?.turnId !== undefined) appStore.confirmUserMessage(projectId, own.id, own.turnId);
      appStore.settleUserMessage(projectId, current.messageId);
      setSending(false);
      void loadChatHistory(projectId);
    }, ABANDON_GRACE_MS);
    graceTimerRef.current = timer;
    return () => {
      clearTimeout(timer);
      if (graceTimerRef.current === timer) graceTimerRef.current = null;
    };
  }, [turnEndedWhileSending, projectId]);

  const bubbles: readonly ActivityView[] =
    thinking && !activities.some((a) => a.agent === "architect" && a.taskId === undefined)
      ? [...activities, { key: "architect:planning", agent: "architect", reasoning: "", text: "", tool: "" }]
      : activities;

  const openWindow = shell.openWindow;
  const openPlan = useCallback(() => openWindow("plan"), [openWindow]);

  const specId = spec?.id ?? null;
  const taskIds = useMemo(() => new Set(spec?.tasks.map((task) => task.id) ?? []), [spec]);
  // Spec 133 T02: a mensagem do sistema da parada atual pelo provedor fica só no cartão logo em
  // seguida (a evidência aparece uma vez só), ao vivo e na reconstrução pelo `dialog.jsonl`.
  const stallCard = pausedRun && spec !== null && !spec.tasks.some((task) => task.status === "BLOCKED");
  const cardStopId = useAppStore((s) => {
    if (!stallCard || spec === null) return null;
    const stop = runStopMessage(s.messages[projectId], spec.id, s.runStarts[projectId]);
    // Só quando o cartão mostra a evidência (com o papel); a parada antiga, sem papel, fica na tela.
    return stop !== null && stop.code !== undefined && PROVIDER_STOP_CODES.has(stop.code) && providerEvidence(stop.params) !== null ? stop.id : null;
  });
  const shown = useMemo(
    () => byTime(messages.filter((m) => m.id !== cardStopId && (m.role === "agent" ? specId !== null && belongsToSpec(m, specId) : true))),
    [messages, specId, cardStopId],
  );
  const entries = permissionEntries(pending, resolved, (entry) => entry.taskId === null || taskIds.has(entry.taskId));
  const timeline = interleave(shown, entries);
  const lastArchitectId = [...shown].reverse().find((m) => m.role === "architect")?.id ?? null;
  // "Tentar de novo" só no aviso da queda que ainda é a última mensagem do chat (spec 127 T01).
  const lastChat = [...shown].reverse().find((m) => m.role !== "agent");
  const retryNoticeId =
    lastChat !== undefined && lastChat.role === "system" && lastChat.code !== undefined && CONNECTION_LOST_CODES.has(lastChat.code)
      ? lastChat.id
      : null;

  const reportScope = `${projectId}:${specId ?? ""}`;
  const [knownReport, setKnownReport] = useState<{ scope: string; at: number } | null>(null);
  const onReportAt = useCallback(
    (at: number | null) => {
      if (at === null) return;
      setKnownReport((prev) => (prev !== null && prev.scope === reportScope && prev.at === at ? prev : { scope: reportScope, at }));
    },
    [reportScope],
  );
  const reportAt = knownReport !== null && knownReport.scope === reportScope ? knownReport.at : null;
  const showResult = spec !== null && spec.status === "COMPLETED";
  const timeOf = (item: (typeof timeline)[number]): number => (item.kind === "message" ? item.item.at : (item.entry.at ?? 0));
  const resultIndex = (() => {
    if (reportAt === null) return timeline.length;
    const found = timeline.findIndex((item) => timeOf(item) > reportAt);
    return found === -1 ? timeline.length : found;
  })();

  /** Envia (ou reenvia, com `resendId`) a mensagem. */
  async function submit(text: string, resendId: number | null, fromChoices = false, attachments: readonly string[] = []): Promise<void> {
    if (sending) return;
    let messageId: number;
    if (resendId === null) {
      messageId = appStore.appendMessage(projectId, {
        role: "user",
        text,
        inFlight: true,
        ...(fromChoices ? { fromChoices: true } : {}),
        ...(attachments.length > 0 ? { attachments } : {}),
      });
    } else {
      messageId = resendId;
      appStore.setUserMessageUnsent(projectId, messageId, null);
    }
    setSentCount((count) => count + 1);
    setTurnEndedWhileSending(false);
    setSending(true);
    const current: PendingSend = { controller: new AbortController(), messageId, abandoned: false };
    pendingSendRef.current = current;
    const result = await postChat(projectId, text, { signal: current.controller.signal, ...(attachments.length > 0 ? { attachments } : {}) });
    if (current.abandoned) return;
    if (pendingSendRef.current === current) pendingSendRef.current = null;
    if (graceTimerRef.current !== null) {
      clearTimeout(graceTimerRef.current);
      graceTimerRef.current = null;
    }
    setSending(false);
    const lost = !result.ok && isConnectionLost(result.error);
    if (!lost) appStore.settleUserMessage(projectId, messageId);
    if (!result.ok && lost) onTrackTurn?.(messageId);
    if (!result.ok && isTurnBusy(result.error)) appStore.setUserMessageUnsent(projectId, messageId, translateNow("chat.turnBusy"));
    void refreshProjectState(projectId);
    if (!result.ok) {
      if (lost || isTurnBusy(result.error)) return;
      if (noticeRecorded(result.error)) {
        void loadChatHistory(projectId);
        return;
      }
      if (result.error.status === 409 && result.error.reason === "report_in_progress") {
        appStore.setUserMessageUnsent(projectId, messageId, translateNow("chat.reportBusy"));
        return;
      }
      // 412 `prereq_missing`: a prontidão mudou; o campo passa a mostrar o que falta.
      // Os rótulos vêm da prontidão relida (ids com parâmetro, como `connection_key:<id>`).
      const prereq = await prereqFailureMessage(result.error, shell.reloadReadiness, shell.readiness);
      appStore.appendMessage(projectId, { role: "system", text: prereq ?? chatFailureMessage(result.error, architectWire) });
      return;
    }
    appStore.confirmUserMessage(projectId, messageId, result.data.turn_id);
    appStore.showArchitectReply(projectId, result.data.reply, result.data.turn_id);
    if (result.data.draft_updated) await onDraftUpdated();
  }
  const submitRef = useRef(submit);
  submitRef.current = submit;

  /**
   * "Tentar de novo" do aviso da queda (spec 127 T01): o servidor manda ao Arquiteto a
   * continuação interna ("continue de onde parou"); nada é digitado nem aparece como mensagem.
   */
  async function continueTurn(): Promise<void> {
    if (sending || chatBlocked) return;
    setTurnEndedWhileSending(false);
    setSending(true);
    const result = await postChatContinue(projectId);
    setSending(false);
    void refreshProjectState(projectId);
    if (!result.ok) {
      // Queda do POST: o turno segue no servidor e o fim dele recarrega o histórico.
      if (isConnectionLost(result.error)) return;
      if (noticeRecorded(result.error)) {
        void loadChatHistory(projectId);
        return;
      }
      appStore.appendMessage(projectId, { role: "system", text: chatFailureMessage(result.error, architectWire) });
      return;
    }
    appStore.showArchitectReply(projectId, result.data.reply, result.data.turn_id);
    if (result.data.draft_updated) await onDraftUpdated();
  }

  // Pedidos de outras janelas (o "Pedir correção ao Arquiteto" do Plano), um por vez.
  const queued = useChatRequestCount(projectId);
  useEffect(() => {
    if (queued === 0 || sending || chatBlocked) return;
    const next = takeChatRequest(projectId);
    if (next !== null) void submitRef.current(next.text, null);
  }, [queued, sending, chatBlocked, projectId]);

  async function send(): Promise<void> {
    const text = draft.trim();
    const withImages = images.items.length > 0;
    if ((text === "" && !withImages) || sending || preparing || chatBlocked) return;
    let names: string[] = [];
    if (withImages) {
      setPreparing(true);
      const done = await images.waitAll();
      setPreparing(false);
      if (done.some((item) => item.status !== "ready")) {
        images.setNotice(translateNow("attachments.failed"));
        return;
      }
      names = done.flatMap((item) => (item.name === undefined ? [] : [item.name]));
    }
    setDraft("");
    images.clear();
    await submit(text === "" ? translateNow("attachments.imageOnly") : text, null, false, names);
  }

  function onPaste(event: ReactClipboardEvent<HTMLTextAreaElement>): void {
    const files = imageFiles(event.clipboardData.items);
    if (files.length === 0) return;
    event.preventDefault();
    if (!chatBlocked) images.add(files);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) return;
    event.preventDefault();
    void send();
  }

  // --- Arrastar e soltar imagens: os eventos param aqui (não chegam ao desktop) ---
  const hasFiles = (event: ReactDragEvent): boolean => Array.from(event.dataTransfer?.types ?? []).includes("Files");
  const onDragEnter = (event: ReactDragEvent<HTMLDivElement>): void => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    dragDepth.current += 1;
    setDragging(true);
  };
  const onDragOver = (event: ReactDragEvent<HTMLDivElement>): void => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = chatBlocked ? "none" : "copy";
  };
  const onDragLeave = (event: ReactDragEvent<HTMLDivElement>): void => {
    if (!hasFiles(event)) return;
    event.stopPropagation();
    dragDepth.current = Math.max(0, dragDepth.current - 1);
    if (dragDepth.current === 0) setDragging(false);
  };
  const onDrop = (event: ReactDragEvent<HTMLDivElement>): void => {
    if (!hasFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    dragDepth.current = 0;
    setDragging(false);
    if (chatBlocked) return;
    const files = imageFiles(event.dataTransfer.files);
    if (files.length > 0) images.add(files);
  };

  // --- Linha do tempo ---
  const resultItem =
    showResult && spec !== null ? (
      <li key={`result:${projectId}:${spec.id}`}>
        <ResultMessage projectId={projectId} specId={spec.id} reportPending={reportPending} onReportAt={onReportAt} />
      </li>
    ) : null;
  const now = Date.now();
  const listItems: ReactNode[] = [];
  // Separador de dia ("Hoje", "Ontem", data) DENTRO do item do primeiro com hora de cada dia: a
  // lista só tem `listitem` como filho (ARIA 1.2).
  let lastDay: string | null = null;
  const daySeparator = (at: number): ReactNode => {
    if (!hasTime(at)) return null;
    const key = dayKey(at);
    if (key === lastDay) return null;
    lastDay = key;
    const label = dayLabel(at, now);
    return (
      <div role="separator" aria-label={label} className="nd-day-sep" data-day-separator="">
        <span aria-hidden="true">{label}</span>
      </div>
    );
  };
  // Rodapé "Plano pronto · Abrir" na última mensagem do Arquiteto (sem ela, um cartão próprio).
  const planFooter = planReady !== null ? <PlanReadyFooter spec={planReady} onOpen={openPlan} /> : null;
  timeline.forEach((item, index) => {
    const resultHere = index === resultIndex && resultItem !== null;
    if (resultHere) listItems.push(resultItem);
    const day = daySeparator(timeOf(item));
    if (item.kind === "permission") {
      listItems.push(
        <li key={`permission:${item.entry.requestId}`} data-action-key={`permission:${item.entry.requestId}`} className="nd-timeline-item">
          {day}
          <PermissionMessage projectId={projectId} entry={item.entry} />
        </li>,
      );
      return;
    }
    const before = timeline[index - 1];
    const previous = !resultHere && before !== undefined && before.kind === "message" ? before.item : undefined;
    const message = item.item;
    listItems.push(
      <li
        key={message.id}
        data-action-key={`choices:${message.id}`}
        data-grouped={sameSource(previous, message) && day === null ? "true" : undefined}
        className="nd-timeline-item"
      >
        {day}
        <Bubble
          message={message}
          grouped={sameSource(previous, message) && day === null}
          projectId={projectId}
          footer={message.id === lastArchitectId ? planFooter : null}
          activeSpecId={specId}
          onResend={(m) => {
            if (!chatBlocked) void submit(m.text, m.id, false, m.attachments ?? []);
          }}
          resendDisabled={sending || chatBlocked}
          onRetryTurn={message.id === retryNoticeId ? () => void continueTurn() : undefined}
          retryDisabled={sending || chatBlocked || thinking}
          choices={
            message.role === "architect"
              ? {
                  interactive: message.id === lastArchitectId,
                  ...answerAfter(shown, message.id, message.text),
                  disabled: sending || chatBlocked,
                  onSubmit: (text) => void submit(text, null, true),
                }
              : undefined
          }
        />
      </li>,
    );
  });
  if (resultIndex >= timeline.length && resultItem !== null) listItems.push(resultItem);
  if (pausedRun && spec !== null) {
    listItems.push(
      <li key={`paused:${projectId}:${spec.id}`}>
        <PausedRunCard projectId={projectId} spec={spec} onResumed={onDraftUpdated} />
      </li>,
    );
  }
  const pin = usePendingPin(pending);
  const composerRef = useKeyboardReveal();

  const empty = timeline.length === 0 && planReady === null && !showResult && !pausedRun;
  const uploading = images.items.some((item) => item.status === "uploading");
  const canSend = !chatBlocked && !sending && !preparing && (draft.trim() !== "" || images.items.length > 0);
  const choicesOpen = lastArchitectId !== null && parseChoices(shown.find((m) => m.id === lastArchitectId)?.text ?? "").questions !== null;

  return (
    <div
      className="nd-conversation"
      data-conversation=""
      data-dragging={dragging ? "true" : undefined}
      onDragEnter={onDragEnter}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      <FinalVerificationNotice projectId={projectId} variant="card" />
      {dialogError && planReady === null ? (
        <p role="alert" className="nd-strip" data-tone="rose">
          <span>{t("conversation.dialogLoadFailed")}</span>
          <button type="button" className="nd-btn nd-btn-sm" onClick={() => onRetryDialog?.()}>
            <Icon name="refresh" size={14} />
            {t("common.tryAgain")}
          </button>
        </p>
      ) : null}
      <AutoScrollBox
        key={projectId}
        items={shown}
        tailKey={`${pausedRun ? 1 : 0}:${planReady !== null ? 1 : 0}:${pendingKey(pending)}:${resolved.length}:${choicesOpen ? 1 : 0}`}
        pinKey={sentCount + pin}
        label={t("conversation.historyLabel")}
        className="nd-conversation-scroll nd-app-scroll"
        scrollAttrs={SCROLL_ATTRS}
        {...(onScrollChange !== undefined ? { restore: { initial: initialScroll, onSave: onScrollChange } } : {})}
      >
        <div className="nd-conversation-feed">
          {empty && pending.length === 0 ? (
            <p className="nd-muted nd-conversation-empty">{t("conversation.empty")}</p>
          ) : null}
          <ul aria-label={t("conversation.messagesLabel")} className="nd-timeline">
            {listItems}
          </ul>
          <ActivityBubbles activities={bubbles} quiet={thinking} />
          {thinking ? (
            <p role="status" className="nd-sr-only">
              {pending.length > 0 ? t("conversation.thinkingPermission") : t("conversation.thinking")}
            </p>
          ) : null}
          {tracking ? (
            <p role="status" className="nd-muted nd-row">
              <span aria-hidden="true" className="nd-dot" data-tone="violet" data-pulse="true" />
              {t("chat.tracking")}
            </p>
          ) : null}
          {planFooter !== null && lastArchitectId === null ? <div className="nd-chat-card">{planFooter}</div> : null}
        </div>
      </AutoScrollBox>

      <div ref={composerRef} data-keyboard-area className="nd-composer-dock">
        {chatBlocked && chatLevel !== null ? <ReadinessNotice title={t("conversation.blockedTitle")} missing={chatLevel.missing} /> : null}
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void send();
          }}
        >
          <AttachmentPreview items={images.items} onRemove={images.remove} />
          {images.notice !== null ? (
            <p role="alert" className="nd-dialog-warning">
              {images.notice}
            </p>
          ) : null}
          {uploading || preparing ? (
            <p role="status" className="nd-sr-only">
              {t("conversation.uploadingImages")}
            </p>
          ) : null}
          <div className="nd-composer" data-disabled={chatBlocked ? "true" : undefined}>
            <button
              type="button"
              className="nd-wbtn nd-composer-btn"
              aria-label={t("attachments.attach")}
              title={t("attachments.attach")}
              disabled={chatBlocked}
              onClick={() => fileRef.current?.click()}
            >
              <Icon name="paperclip" size={16} />
            </button>
            <input
              ref={fileRef}
              type="file"
              accept="image/*"
              multiple
              hidden
              data-testid="chat-attach-input"
              onChange={(event) => {
                const files = Array.from(event.target.files ?? []);
                event.target.value = "";
                images.add(files);
              }}
            />
            <textarea
              id={inputId}
              rows={1}
              value={draft}
              ref={areaRef}
              disabled={chatBlocked}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
              aria-describedby={hintId}
              aria-label={t("conversation.inputLabel")}
              placeholder={chatBlocked ? t("conversation.placeholderBlocked") : t("conversation.placeholder")}
              className="nd-composer-input"
            />
            <span id={hintId} className="nd-sr-only">
              {t("conversation.inputHint")}
            </span>
            <button type="submit" aria-label={t("conversation.send")} disabled={!canSend} className="nd-btn nd-composer-send" data-variant={canSend ? "primary" : undefined}>
              <Icon name="send" size={16} />
            </button>
          </div>
        </form>
        <RoleConnectionFooter projectId={projectId} pausedSpecId={pausedRun && spec !== null ? spec.id : null} />
      </div>
      {dragging ? (
        <div className="nd-drop-overlay" aria-hidden="true">
          <Icon name="image" size={20} />
          <span>{chatBlocked ? t("conversation.dropBlocked") : t("conversation.dropHint")}</span>
        </div>
      ) : null}
    </div>
  );
}

/** Revisão do plano em uma palavra para o rodapé ("revisado" ou os apontamentos). */
function planReviewNote(spec: SpecView): string | null {
  const review = spec.plan_review;
  if (review === undefined || review.status !== "ok") return null;
  const issues = review.issues.length;
  if (issues === 0) return translateNow("planReady.reviewed");
  return tpNow("planReady.issues", issues);
}

/**
 * Rodapé de ação "Plano pronto · N tarefas · revisado · Abrir" (095/T04, `Main.dc.html`): o plano
 * completo fica na janela Plano.
 */
function PlanReadyFooter({ spec, onOpen }: { spec: SpecView; onOpen: () => void }) {
  const { t, tp } = useT();
  const note = planReviewNote(spec);
  const meta = [tp("plan.taskCount", spec.tasks.length), ...(note !== null ? [note] : [])].join(" · ");
  return (
    <div className="nd-chat-card-foot nd-plan-ready" data-plan-ready="">
      <Icon name="plan" size={14} className="nd-plan-ready-icon" />
      <span className="nd-strong">{t("planReady.title")}</span>
      <span className="nd-plan-ready-meta">{meta}</span>
      <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" aria-label={t("planReady.openLabel")} onClick={onOpen}>
        {t("planReady.open")}
        <Icon name="arrow-right" size={14} />
      </button>
    </div>
  );
}
