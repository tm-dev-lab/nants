/*
 * Conteúdo da janela Conversa no desktop (`DEFAULT_APPS.conversation.Content`, em `memo`): lê o
 * espaço por `useSpace()` e as fatias de alta frequência pelos seletores próprios (mensagens,
 * atividades e permissões resolvidas), para o streaming só re-renderizar esta janela.
 * A rolagem é restaurada do ui-state (`selection.conversationScroll`) e gravada com atraso.
 */
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { useSpace } from "../../desktop/shellContext";
import { useSpaceActivities, useSpaceMessages, useSpaceResolvedPermissions } from "../../desktop/useSpaceData";
import type { AppContentProps } from "../registry";
import type { ScrollPosition } from "./AutoScrollBox";
import { Conversation, scrollFromSelection } from "./Conversation";

/** Atraso para gravar a rolagem no ui-state (o sync ainda limita a 1 PUT/s). */
const SCROLL_SAVE_DELAY_MS = 600;

/** Grava a posição com atraso e só quando muda; o fim vira `null` no ui-state. */
function useScrollSaver(save: (value: number | null) => void, current: number | null): (position: ScrollPosition) => void {
  const lastRef = useRef<number | null>(current);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingRef = useRef<number | null | undefined>(undefined);
  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(
    () => () => {
      if (timerRef.current !== null) clearTimeout(timerRef.current);
      const value = pendingRef.current;
      if (value !== undefined && value !== lastRef.current) saveRef.current(value);
    },
    [],
  );
  return useCallback((position: ScrollPosition) => {
    const value = position.atBottom ? null : Math.round(position.top);
    pendingRef.current = value;
    if (timerRef.current !== null) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      pendingRef.current = undefined;
      if (value === lastRef.current) return;
      lastRef.current = value;
      saveRef.current(value);
    }, SCROLL_SAVE_DELAY_MS);
  }, []);
}

function ConversationContentView(_props: AppContentProps) {
  const space = useSpace();
  const projectId = space?.data.projectId ?? "";
  const messages = useSpaceMessages(projectId);
  const activities = useSpaceActivities(projectId);
  const resolved = useSpaceResolvedPermissions(projectId);
  const savedScroll = space?.selection.conversationScroll ?? null;
  const [initialScroll] = useState(() => scrollFromSelection(savedScroll));
  const setSelection = space?.setSelection;
  const save = useCallback((value: number | null) => setSelection?.({ conversationScroll: value }), [setSelection]);
  const onScrollChange = useScrollSaver(save, savedScroll);
  if (space === null) return null;
  const data = space.data;
  return (
    <Conversation
      projectId={data.projectId}
      messages={messages}
      spec={data.activeSpec}
      planReady={data.planReady}
      onDraftUpdated={data.reload}
      pending={data.permissions}
      resolved={resolved}
      activities={activities}
      dialogError={data.dialogError}
      onRetryDialog={data.retryDialog}
      reportPending={data.state?.report_pending === true}
      pausedRun={data.paused || data.stalled}
      tracking={data.tracking}
      onTrackTurn={data.trackTurn}
      initialScroll={initialScroll}
      onScrollChange={onScrollChange}
      request={space.architectRequest}
      onRequestConsumed={space.consumeArchitectRequest}
    />
  );
}

export const ConversationContent = memo(ConversationContentView);
