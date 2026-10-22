/*
 * Dados de um espaço (spec 093). Chamado UMA vez pelo
 * shell, para o espaço ativo; as janelas leem o resultado pelo contexto do espaço
 * (`useSpace()`), nunca disparam a própria carga. Assim, com N janelas abertas, um evento gera
 * exatamente um GET de estado.
 *
 * O que faz:
 * - estado do projeto: carga ao abrir e a cada `revisions`/`liveRevisions` (1 GET por evento);
 * - histórico do Arquiteto e atividades: uma carga por abertura do espaço;
 * - diálogo dos agentes da spec ativa: uma carga por (projeto, spec), com "Tentar de novo";
 * - erros de carga com "Tentar de novo"; "carregando"; "projeto não encontrado";
 * - pedidos órfãos: as permissões pendentes enquanto o espaço não está pronto;
 * - poda das atividades a cada 500 ms (`useActivityPrune`, montado à parte pelo shell);
 * - rastreio do turno do Arquiteto depois que o POST do chat caiu (consulta o estado a cada
 *   10 s até o turno terminar) e a rede de segurança de 210 s do turno remoto;
 * - etapa do plano (`planStage`, spec 094 decisão 3) e o acompanhamento da revisão do plano:
 *   consulta o estado a cada 20 s enquanto a revisão da versão atual está `pending`/`running`,
 *   com a janela Plano aberta, fechada ou minimizada (o poll é do espaço, não da janela).
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { refreshActivities } from "../activityRehydrate";
import type { ProjectEntry, ProjectState, SpecView } from "../api/types";
import { loadChatHistory } from "../chatHistory";
import { t } from "../i18n";
import { activeSpecIdOf, loadDialog } from "../dialogHistory";
import { isExecutionMode, isStalledRun, phaseForProject, pickActiveSpec, planStage, reviewWaiting, type PlanStage } from "../phase";
import { refreshProjectState } from "../projectState";
import {
  appStore,
  useAppStore,
  type ActivityEntry,
  type ChatMessage,
  type PendingPermission,
  type ProjectPhase,
  type ResolvedPermission,
} from "../store";
import { categoryOf, type LayoutCategory } from "./presets";

export const ACTIVITY_PRUNE_MS = 500;
/** Consulta ao estado enquanto se acompanha um turno depois da queda do POST. */
export const TRACKING_POLL_MS = 10_000;
/** Sem `chat_turn_ended` (evento perdido), o turno remoto expira depois do limite do servidor + folga. */
export const REMOTE_TURN_EXPIRY_MS = 210_000;
/** Intervalo de reconsulta do estado enquanto a revisão da versão atual do plano não termina. */
export const PLAN_REVIEW_RECHECK_MS = 20_000;
/** Janela para casar a mensagem otimista com a linha do histórico (relógios de cliente e servidor). */
const HISTORY_MATCH_SLACK_MS = 5 * 60_000;

const EMPTY_MESSAGES: readonly ChatMessage[] = [];
const EMPTY_PERMISSIONS: readonly PendingPermission[] = [];
const EMPTY_RESOLVED: readonly ResolvedPermission[] = [];
const EMPTY_ACTIVITIES: readonly ActivityEntry[] = [];

type SpaceStatus = "loading" | "ready" | "not_found";

/** Execução ativa da spec (o "Parar" aparece). */
function hasActiveRun(state: ProjectState, specId: string): boolean {
  return state.run !== null && state.run.spec_id === specId;
}

/** Execução pausada: spec IN_PROGRESS, sem run e com tarefa BLOCKED. */
function isPausedRun(state: ProjectState, spec: SpecView | null): boolean {
  if (spec === null || spec.status !== "IN_PROGRESS") return false;
  if (state.running || state.run !== null) return false;
  return spec.tasks.some((task) => task.status === "BLOCKED");
}

/**
 * O espaço está pronto para as janelas (estado carregado e config ou erro de config resolvidos).
 * Fora disso, as permissões pendentes são "órfãs" (ver `orphanPermissions`).
 */
function spaceReadyFor(config: unknown, configError: string | null, state: ProjectState | null): boolean {
  return state !== null && (config !== null || configError !== null);
}

/** Estado e derivações do espaço, para as janelas e a barra superior. */
export interface SpaceData {
  projectId: string;
  status: SpaceStatus;
  /** Nome exibido (do estado ou da config). */
  name: string;
  project: ProjectEntry | null;
  state: ProjectState | null;
  /** Fase exibida (`phaseForProject`) e a categoria de layout dela. */
  phase: ProjectPhase;
  category: LayoutCategory;
  activeSpec: SpecView | null;
  /** Modo execução (spec rodando ou concluída). */
  execution: boolean;
  /** Etapa do plano da spec ativa (`planStage`): planejando, revisando, aguardando ou `null`. */
  planStage: PlanStage | null;
  /** Plano pronto para aprovar (a spec, só na etapa `awaiting`), ou `null`. */
  planReady: SpecView | null;
  /** Execução ativa da spec ativa (o "Parar" aparece). */
  running: boolean;
  paused: boolean;
  stalled: boolean;
  architectTurnActive: boolean;
  /**
   * Permissões pendentes do espaço. Mensagens, atividades e permissões resolvidas NÃO ficam aqui
   * (mudam a cada evento do streaming): cada janela as lê pelo próprio seletor,
   * `useSpaceMessages`, `useSpaceActivities` e `useSpaceResolvedPermissions`.
   */
  permissions: readonly PendingPermission[];
  /** Permissões pendentes enquanto o espaço não está pronto (decidíveis no "Precisa de você"). */
  orphanPermissions: readonly PendingPermission[];
  configError: string | null;
  /** Falha do GET de estado (texto fixo do cliente), com `reload` como "Tentar de novo". */
  loadError: string | null;
  /** Falha da carga do diálogo da spec ativa, com `retryDialog`. */
  dialogError: boolean;
  /** Acompanhando um turno do Arquiteto depois da queda do POST. */
  tracking: boolean;
  reload: () => Promise<void>;
  retryDialog: () => void;
  /** O POST do chat caiu: acompanha o turno no servidor até o fim (Conversa, T06). */
  trackTurn: (messageId: number | null) => void;
}

/** Acompanhamento do turno do Arquiteto depois que o POST do chat caiu. */
interface TurnTracking {
  messageId: number | null;
  /** Estado no início: só um estado lido depois da queda decide que o turno acabou. */
  base: ProjectState | undefined;
  sawTurn: boolean;
  finishing: boolean;
}

/**
 * Fim do acompanhamento: recarrega o histórico e decide o destino da
 * mensagem deste envio (confirmada, duplicada no histórico ou não enviada).
 */
async function finishTracking(projectId: string, tracking: TurnTracking): Promise<void> {
  const id = tracking.messageId;
  const before = id === null ? undefined : appStore.getState().messages[projectId]?.find((m) => m.id === id);
  if (before?.turnId !== undefined) appStore.confirmUserMessage(projectId, before.id, before.turnId);
  await loadChatHistory(projectId);
  if (id === null) return;
  const own = appStore.getState().messages[projectId]?.find((m) => m.id === id);
  if (own === undefined) return;
  if (own.confirmed === true) {
    appStore.settleUserMessage(projectId, id);
    return;
  }
  const inHistory = (appStore.getState().messages[projectId] ?? []).some(
    (m) => m.origin === "history" && m.role === "user" && m.text === own.text && m.at >= own.at - HISTORY_MATCH_SLACK_MS,
  );
  if (inHistory) {
    appStore.removeMessage(projectId, id);
    return;
  }
  appStore.setUserMessageUnsent(projectId, id, t("chat.lostUnsent"));
}

export function useSpaceData(projectId: string): SpaceData {
  const config = useAppStore((s) => s.config);
  const configError = useAppStore((s) => s.configError);
  const state = useAppStore((s) => s.projectStates[projectId] ?? null);
  const storedPhase = useAppStore((s) => s.phases[projectId]);
  const remoteTurn = useAppStore((s) => s.architectTurns[projectId]);
  const permissions = useAppStore((s) => s.permissions[projectId] ?? EMPTY_PERMISSIONS);
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  const liveRevision = useAppStore((s) => s.liveRevisions[projectId] ?? 0);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [dialogError, setDialogError] = useState(false);
  const [tracking, setTracking] = useState<TurnTracking | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    const result = await refreshProjectState(projectId);
    if (result === null) return; // resposta superada por um GET mais novo
    setLoadError(result.ok ? null : result.error.message);
  }, [projectId]);

  useEffect(() => {
    appStore.setState({ activeProjectId: projectId });
  }, [projectId]);

  // Histórico do Arquiteto e atividades: uma carga por abertura do espaço.
  useEffect(() => {
    void loadChatHistory(projectId);
    void refreshActivities(projectId);
  }, [projectId]);

  // Diálogo dos agentes da spec ativa: carga completa uma vez por (projeto, spec).
  const activeSpecId = state === null ? null : (pickActiveSpec(state.specs)?.id ?? null);
  const loadDialogFor = useCallback(
    async (specId: string): Promise<void> => {
      setDialogError(false);
      const outcome = await loadDialog(projectId, specId);
      if (outcome === "stale" || activeSpecIdOf(projectId) !== specId) return;
      setDialogError(outcome === "failed");
    },
    [projectId],
  );
  useEffect(() => {
    if (activeSpecId !== null) void loadDialogFor(activeSpecId);
  }, [loadDialogFor, activeSpecId]);
  const retryDialog = useCallback(() => {
    const specId = activeSpecIdOf(projectId);
    if (specId !== null) void loadDialogFor(specId);
  }, [loadDialogFor, projectId]);

  // Estado: ao abrir e a cada aviso de mudança (stream global) ou recarga ao vivo.
  useEffect(() => {
    void reload();
  }, [reload, revision, liveRevision]);

  // Rastreio do turno depois da queda do POST: termina quando o estado (lido depois da queda) ou
  // o stream mostram que não há turno ativo; aí o histórico traz a resposta.
  useEffect(() => {
    if (tracking === null || tracking.finishing) return;
    if (remoteTurn !== undefined) {
      if (!tracking.sawTurn) setTracking({ ...tracking, sawTurn: true });
      return;
    }
    if (!tracking.sawTurn && state === (tracking.base ?? null)) return;
    if (appStore.getState().architectTurns[projectId] !== undefined) return;
    setTracking({ ...tracking, finishing: true });
    void finishTracking(projectId, tracking).then(() => setTracking(null));
  }, [tracking, remoteTurn, state, projectId]);
  const trackingActive = tracking !== null;
  useEffect(() => {
    if (!trackingActive) return undefined;
    const timer = setInterval(() => void refreshProjectState(projectId), TRACKING_POLL_MS);
    return () => clearInterval(timer);
  }, [trackingActive, projectId]);
  const trackTurn = useCallback(
    (messageId: number | null) => {
      setTracking({ messageId, base: appStore.getState().projectStates[projectId], sawTurn: false, finishing: false });
    },
    [projectId],
  );

  // Revisão do plano em curso: reconsulta o estado a cada 20 s até ela terminar, aberta ou não
  // a janela Plano (o `sdd_draft_updated` do fim da revisão também recarrega).
  const activeSpecForReview = state === null ? null : pickActiveSpec(state.specs);
  const reviewPending =
    state !== null &&
    !state.running &&
    activeSpecForReview?.status === "AWAITING_APPROVAL" &&
    reviewWaiting(activeSpecForReview.plan_review);
  useEffect(() => {
    if (!reviewPending) return undefined;
    const timer = setInterval(() => void refreshProjectState(projectId), PLAN_REVIEW_RECHECK_MS);
    return () => clearInterval(timer);
  }, [reviewPending, projectId]);

  // Rede de segurança do turno remoto.
  const remoteTurnId = remoteTurn?.turnId;
  useEffect(() => {
    if (remoteTurnId === undefined) return undefined;
    const timer = setTimeout(() => appStore.endArchitectTurn(projectId, remoteTurnId), REMOTE_TURN_EXPIRY_MS);
    return () => clearTimeout(timer);
  }, [projectId, remoteTurnId]);

  return useMemo<SpaceData>(() => {
    const project = config?.projects.find((p) => p.id === projectId) ?? null;
    const active = state === null ? null : pickActiveSpec(state.specs);
    const architectTurnActive = remoteTurn !== undefined;
    const phase = phaseForProject(storedPhase, state, architectTurnActive);
    const stage = planStage(state, active, architectTurnActive);
    const ready = spaceReadyFor(config, configError, state);
    let status: SpaceStatus = "ready";
    if (config === null && configError === null) status = "loading";
    else if (project === null && state === null) status = config !== null ? "not_found" : "loading";
    else if (state === null) status = "loading";
    return {
      projectId,
      status,
      name: state?.name ?? project?.name ?? "",
      project,
      state,
      phase,
      category: categoryOf(phase),
      activeSpec: active,
      execution: state !== null && active !== null && isExecutionMode(state, active),
      planStage: stage,
      planReady: stage === "awaiting" ? active : null,
      running: state !== null && active !== null && hasActiveRun(state, active.id),
      paused: state !== null && isPausedRun(state, active),
      stalled: state !== null && isStalledRun(state, active),
      architectTurnActive,
      permissions,
      orphanPermissions: ready ? EMPTY_PERMISSIONS : permissions,
      configError,
      loadError,
      dialogError,
      tracking: tracking !== null,
      reload,
      retryDialog,
      trackTurn,
    };
  }, [
    config,
    configError,
    state,
    storedPhase,
    remoteTurn,
    permissions,
    loadError,
    dialogError,
    tracking,
    reload,
    retryDialog,
    trackTurn,
    projectId,
  ]);
}

/** Mensagens do espaço (Conversa): seletor próprio, para só quem as mostra re-renderizar. */
export function useSpaceMessages(projectId: string): readonly ChatMessage[] {
  return useAppStore((s) => s.messages[projectId] ?? EMPTY_MESSAGES);
}

/** Atividades ao vivo do espaço (Atividade, bolhas da Conversa). */
export function useSpaceActivities(projectId: string): readonly ActivityEntry[] {
  return useAppStore((s) => s.activities[projectId] ?? EMPTY_ACTIVITIES);
}

/** Permissões já resolvidas do espaço (linha do tempo da Conversa). */
export function useSpaceResolvedPermissions(projectId: string): readonly ResolvedPermission[] {
  return useAppStore((s) => s.resolvedPermissions[projectId] ?? EMPTY_RESOLVED);
}

/**
 * Poda das atividades (carência pós-`end`, inatividade) a cada 500 ms enquanto houver alguma.
 * Fica fora de `useSpaceData`: só assina a QUANTIDADE de atividades, e o shell a monta num
 * componente próprio (`SpaceServices`), para o streaming não re-renderizar o desktop.
 */
export function useActivityPrune(projectId: string): void {
  const activityCount = useAppStore((s) => s.activities[projectId]?.length ?? 0);
  useEffect(() => {
    if (activityCount === 0) return undefined;
    const timer = setInterval(() => appStore.pruneActivities(projectId, Date.now()), ACTIVITY_PRUNE_MS);
    return () => clearInterval(timer);
  }, [projectId, activityCount]);
}
