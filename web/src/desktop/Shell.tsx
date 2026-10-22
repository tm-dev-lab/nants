/*
 * Shell do desktop (spec 093, T05; decisões 1, 2, 7, 8, 11, 15 e 16): monta o Desktop com a barra
 * superior, os espaços (um por projeto, URL `/?space=<id>`), os dados do espaço ativo
 * (`useSpaceData`, uma vez), os serviços da aba (SSE global, atenção, prontidão), o lançador,
 * o diálogo Novo projeto, os Primeiros passos e as boas-vindas.
 *
 * Só as janelas do espaço ativo ficam montadas: trocar de espaço desmonta a sessão anterior e
 * monta a nova, que restaura o arranjo da categoria da fase e a seleção salvos no ui-state.
 *
 * Abaixo de 1024 px (T10, decisão 17), o mesmo shell entra no modo celular: barra compacta, uma
 * janela por vez, abas por fase e o Alternador. O celular só grava a seção `mobile` do ui-state
 * do projeto (a vista e a própria seleção); o layout e a seleção do desktop ficam intactos.
 */
import "./theme/shell.css";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
  type ReactNode,
} from "react";
import { approveSpec } from "../api/client";
import { t, tp } from "../i18n";
import { useLanguage } from "../i18n/react";
import type { LanRange, Project, ProjectEntry } from "../api/types";
import { renderAttentionPanel as renderDefaultAttentionPanel } from "../apps/attention/AttentionPanel";
import { attentionFor } from "../apps/attention/attentionModel";
import { PermissionToasts } from "../apps/attention/PermissionToasts";
import { DEFAULT_APPS, type AppRegistry, type AppSummary, type AppSummaryInput } from "../apps/registry";
import { PHASE_LABELS, phaseForProject } from "../phase";
import { appStore, useAppStore, type ChatMessage } from "../store";
import { planApprovable } from "../apps/plan/summary";
import { PLAN_ACTIONS_WAITING_REASON } from "../copy";
import { gateBlocked, gateLevel, prereqFailureMessage, readinessItemLabel } from "../apps/shared/readiness";
import { attentionCount as countAttention, setupIncomplete, type Attention, type Readiness, type SettingsWindowTab } from "./api";
import { useAttentionSignal } from "./attention";
import { Desktop, useDesktopReducer, type MobileControl, type WindowSpec } from "./Desktop";
import { mergedSelection } from "./mobile";
import { useMobileView } from "./useMobileView";
import { useMobileMode } from "./viewport";
import { Launcher } from "./Launcher";
import type { LauncherItem } from "./launcher";
import { NewProjectDialog } from "./NewProjectDialog";
import { dockSlotAria } from "./shortcuts";
import {
  ShellContext,
  SpaceContext,
  type ArchitectRequest,
  type SettingsRequest,
  type ShellApi,
  type SpaceContextValue,
  type ToastOptions,
} from "./shellContext";
import { replaceSpace, useSpaceRoute } from "./spaceRoute";
import { localRemovalNotice, nextSpaceAfterRemoval, removedNotice, subscribeSpaceRemovals } from "../apps/project/projectRemoval";
import { Icon } from "./Icon";
import { resumeProblem } from "./topbar/RunPill";
import type { LayoutCommand } from "./topbar/LayoutMenu";
import { TopBar } from "./topbar/TopBar";
import { getSection, parseSelection, UI_SECTIONS, type UiSelection, type UiStateDeps } from "./uiState";
import type { ContainerSize, ResizeObserverFactory } from "./useContainerSize";
import { useDesktopRuntime, type DesktopRuntime } from "./useDesktopRuntime";
import { usePhaseLayout } from "./usePhaseLayout";
import { useActivityPrune, useSpaceData, type SpaceData } from "./useSpaceData";
import { useUiSection, useUiState, type UiStateHandle } from "./useUiState";
import { networkStateValue, parseNetworkState } from "./network";
import type { FrameScheduler } from "./useWindowGesture";
import { usePageVisible, isWindowVisible } from "./useWindowVisibility";
import { useWindowHost } from "./windowHost";
import { DOCK_ORDER, WINDOW_META } from "./windowMeta";
import { workspaceForViewport } from "./metrics";
import { canUndo, MIN_WINDOW_H, WINDOW_KINDS, type Layout, type WindowAction, type WindowKind, type Workspace } from "./windows";
import { SpaceLoading, SpaceNotFound, Welcome } from "./Welcome";

/** Tempo do aviso (com "Desfazer") na tela. */
const TOAST_MS = 8_000;

const EMPTY_LAYOUT: Layout = { windows: {} };
function initialWorkspace(): Workspace {
  if (typeof window === "undefined") return { w: 1280, h: 680 };
  const ws = workspaceForViewport(window.innerWidth, window.innerHeight);
  return { w: ws.w, h: Math.max(MIN_WINDOW_H, ws.h) };
}

/** Área de trabalho medida no DOM (a primeira aplicação do layout vem antes do re-render). */
function measureWorkspace(): Workspace | null {
  const el = typeof document === "undefined" ? null : document.querySelector<HTMLElement>("[data-workarea]");
  const r = el?.getBoundingClientRect();
  return r !== undefined && r.width > 0 && r.height > 0 ? { w: r.width, h: r.height } : null;
}

interface ShellProps {
  /** Janelas registradas (padrão: `DEFAULT_APPS`). */
  apps?: AppRegistry;
  /** Painel do sino "Precisa de você" (T06). */
  renderAttentionPanel?: (props: { onClose: () => void }) => ReactNode;
  /** Testes: dependências do ui-state, frequência do `/api/attention`, observador e agendador. */
  uiStateDeps?: UiStateDeps;
  attentionIntervalMs?: number;
  createObserver?: ResizeObserverFactory | null;
  scheduler?: FrameScheduler;
}

interface Toast {
  id: number;
  message: string;
  undo: boolean;
  /** Título do aviso (aviso com título, como "App no ar"). */
  title?: string;
  /** `previous` do gerenciador logo depois da mudança (o "Desfazer" vale enquanto for o mesmo). */
  anchor?: Layout | null;
}

/** O que a sessão (com ou sem espaço) recebe do shell. */
interface Shared {
  props: ShellProps;
  runtime: DesktopRuntime;
  globalState: UiStateHandle;
  spaceId: string | null;
  switchSpace: (id: string | null) => void;
  newProjectOpen: boolean;
  setNewProjectOpen: (open: boolean) => void;
  launcherOpen: boolean;
  setLauncherOpen: (open: boolean) => void;
  bellOpen: boolean;
  setBellOpen: (open: boolean | ((v: boolean) => boolean)) => void;
  toast: Toast | null;
  setToast: (toast: Toast | null | ((t: Toast | null) => Toast | null)) => void;
  setupAutoOpened: MutableRefObject<boolean>;
  /** Janela a abrir quando o próximo espaço estiver pronto (`goTo` para outro projeto). */
  pendingOpen: MutableRefObject<{ projectId: string; kind: WindowKind } | null>;
  settingsRequest: SettingsRequest | null;
  setSettingsRequest: (request: SettingsRequest | null | ((r: SettingsRequest | null) => SettingsRequest | null)) => void;
  /** Modo celular (largura < 1024 px). */
  mobile: boolean;
}

/**
 * Por que "Aprovar o plano" (lançador) não vale agora, ou `null`: a mesma etapa do Plano
 * (`planStage`: revisando = "aguardando o Revisor") e a prontidão (`execute`).
 */
function approvePlanBlocker(space: SpaceData, readiness: Readiness | null): string | null {
  if (space.planStage === "reviewing") return PLAN_ACTIONS_WAITING_REASON;
  if (!planApprovable(space)) return t("launcher.noPlanAwaiting");
  if (gateBlocked(readiness, "execute")) return missingText(readiness);
  return null;
}

/** "falta configurar: A, B" (nível `execute` da prontidão). */
function missingText(readiness: Readiness | null): string {
  const labels = (gateLevel(readiness, "execute")?.missing ?? []).map(readinessItemLabel);
  return labels.length > 0 ? t("launcher.missingSetup", { items: labels.join(", ") }) : t("launcher.missingSetupNants");
}

export function Shell(props: ShellProps) {
  const runtime = useDesktopRuntime(props.attentionIntervalMs !== undefined ? { attentionIntervalMs: props.attentionIntervalMs } : {});
  const [spaceId, switchSpace] = useSpaceRoute();
  const globalState = useUiState({ kind: "global" }, props.uiStateDeps);
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const [launcherOpen, setLauncherOpen] = useState(false);
  const [bellOpen, setBellOpen] = useState(false);
  const [toast, setToast] = useState<Toast | null>(null);
  const setupAutoOpened = useRef(false);
  const pendingOpen = useRef<{ projectId: string; kind: WindowKind } | null>(null);
  const [settingsRequest, setSettingsRequest] = useState<SettingsRequest | null>(null);
  const mobile = useMobileMode();

  // Título e favicon de atenção: a mesma contagem do sino (todos os projetos).
  useAttentionSignal(countAttention(runtime.attention));

  useEffect(() => {
    if (toast === null) return undefined;
    const timer = setTimeout(() => setToast((t) => (t?.id === toast.id ? null : t)), TOAST_MS);
    return () => clearTimeout(timer);
  }, [toast?.id]);

  // Projeto removido (nesta aba ou em outra): o espaço dele fecha, aparece o aviso e o desktop
  // vai para outro espaço ou para as boas-vindas (sem deixar o removido no histórico).
  const spaceRef = useRef(spaceId);
  spaceRef.current = spaceId;
  const refreshAttention = runtime.refreshAttention;
  useEffect(
    () =>
      subscribeSpaceRemovals((removal) => {
        refreshAttention();
        if (removal.projectId !== spaceRef.current) return;
        pendingOpen.current = null;
        replaceSpace(nextSpaceAfterRemoval(appStore.getState().config?.projects ?? [], removal.projectId));
        toastSeq += 1;
        setToast({ id: toastSeq, message: removal.remote ? removedNotice() : localRemovalNotice(removal.name), undo: false });
      }),
    [refreshAttention],
  );

  // Boas-vindas: nenhum projeto ativo no store.
  useEffect(() => {
    if (spaceId === null) appStore.setState({ activeProjectId: null });
  }, [spaceId]);

  const shared: Shared = {
    props,
    runtime,
    globalState,
    spaceId,
    switchSpace,
    newProjectOpen,
    setNewProjectOpen,
    launcherOpen,
    setLauncherOpen,
    bellOpen,
    setBellOpen,
    toast,
    setToast,
    setupAutoOpened,
    pendingOpen,
    settingsRequest,
    setSettingsRequest,
    mobile,
  };
  return spaceId === null ? <HomeSession shared={shared} /> : <SpaceSession key={spaceId} spaceId={spaceId} shared={shared} />;
}

function SpaceSession({ spaceId, shared }: { spaceId: string; shared: Shared }) {
  const space = useSpaceData(spaceId);
  const ui = useUiState({ kind: "project", projectId: spaceId }, shared.props.uiStateDeps);
  return (
    <>
      <SpaceServices projectId={spaceId} />
      <SessionDesktop shared={shared} space={space} projectUi={ui} />
    </>
  );
}

/** Serviços do espaço que assinam o streaming (poda das atividades), fora da árvore do desktop. */
function SpaceServices({ projectId }: { projectId: string }) {
  useActivityPrune(projectId);
  return null;
}

const EMPTY_PROJECTS: readonly ProjectEntry[] = [];

/**
 * Especificação das janelas com identidade estável: o `content` de cada tipo depende só do
 * registro (`apps`); subtítulo, selo e vazio são recalculados a partir dos dados, mas o objeto
 * da janela só muda quando algum deles muda de valor. Assim a moldura `Window` (memo) não
 * re-renderiza à toa.
 */
function useWindowSpecs(apps: AppRegistry, input: AppSummaryInput): Partial<Record<WindowKind, WindowSpec>> {
  const contents = useMemo(() => {
    const out: Partial<Record<WindowKind, (size: ContainerSize) => ReactNode>> = {};
    for (const kind of WINDOW_KINDS) {
      const Content = apps[kind]?.Content;
      if (Content !== undefined) out[kind] = (size) => <Content size={size} />;
    }
    return out;
  }, [apps]);
  const summaries = useMemo(() => {
    const out: Partial<Record<WindowKind, AppSummary>> = {};
    for (const kind of WINDOW_KINDS) {
      const summarize = apps[kind]?.summarize;
      if (summarize !== undefined) out[kind] = summarize(input);
    }
    return out;
  }, [apps, input]);
  // A ação do vazio muda de closure a cada cálculo: a janela chama sempre a mais recente.
  const actions = useRef<Partial<Record<WindowKind, () => void>>>({});
  for (const kind of WINDOW_KINDS) actions.current[kind] = summaries[kind]?.empty?.action?.onClick;
  const key = JSON.stringify(
    WINDOW_KINDS.map((kind) => {
      const s = summaries[kind];
      return s === undefined ? null : [s.subtitle, s.badge?.count, s.badge?.label, s.empty?.message, s.empty?.action?.label];
    }),
  );
  return useMemo(() => {
    const out: Partial<Record<WindowKind, WindowSpec>> = {};
    for (const kind of WINDOW_KINDS) {
      const summary = summaries[kind];
      const content = contents[kind];
      if (summary === undefined && content === undefined) continue;
      const empty = summary?.empty;
      const scroll = apps[kind]?.scroll;
      out[kind] = {
        ...(scroll !== undefined ? { scroll } : {}),
        ...(summary?.subtitle !== undefined ? { subtitle: summary.subtitle } : {}),
        ...(summary?.badge !== undefined ? { badge: { count: summary.badge.count, label: summary.badge.label } } : {}),
        ...(empty !== undefined
          ? {
              empty:
                empty.action === undefined
                  ? { message: empty.message }
                  : { message: empty.message, action: { label: empty.action.label, onClick: () => actions.current[kind]?.() } },
            }
          : {}),
        ...(content !== undefined ? { content } : {}),
      };
    }
    return out;
    // `key` resume os valores de `summaries`; o objeto só muda quando eles mudam.
  }, [apps, contents, key]);
}

function HomeSession({ shared }: { shared: Shared }) {
  return <SessionDesktop shared={shared} space={null} projectUi={null} />;
}

/** Guarda o `act` do Desktop (abrir com foco e anúncio) para o shell. */
function HostBridge({ target }: { target: MutableRefObject<((action: WindowAction) => void) | null> }) {
  const host = useWindowHost();
  useEffect(() => {
    target.current = host.act;
    return () => {
      if (target.current === host.act) target.current = null;
    };
  }, [host, target]);
  return null;
}

let toastSeq = 0;
let requestSeq = 0;
let settingsSeq = 0;

const NO_MESSAGES: readonly ChatMessage[] = [];

/** Maior `seq` ao vivo das mensagens (0 sem nenhuma). */
function latestSeq(messages: readonly ChatMessage[]): number {
  let max = 0;
  for (const m of messages) if (m.seq !== undefined && m.seq > max) max = m.seq;
  return max;
}

/**
 * Mensagens novas da Conversa chegadas com a janela fora de vista (selo do dock). Assina só a CONTAGEM: o streaming não re-renderiza o desktop.
 */
function useConversationUnseen(projectId: string | null, visible: boolean): number {
  const seenRef = useRef<{ projectId: string | null; seq: number } | null>(null);
  const wasVisible = useRef(visible);
  if (seenRef.current === null || seenRef.current.projectId !== projectId || visible || wasVisible.current) {
    const messages = projectId === null ? NO_MESSAGES : (appStore.getState().messages[projectId] ?? NO_MESSAGES);
    seenRef.current = { projectId, seq: latestSeq(messages) };
  }
  wasVisible.current = visible;
  const seen = seenRef.current.seq;
  return useAppStore((s) => {
    if (visible || projectId === null) return 0;
    let count = 0;
    for (const m of s.messages[projectId] ?? NO_MESSAGES) if (m.seq !== undefined && m.seq > seen) count += 1;
    return count;
  });
}

function SessionDesktop({ shared, space, projectUi }: { shared: Shared; space: SpaceData | null; projectUi: UiStateHandle | null }) {
  const { props, runtime, spaceId, switchSpace } = shared;
  // Spec 101: a moldura (lançador, avisos, resumos das janelas) segue o idioma ativo.
  const lang = useLanguage();
  const apps = props.apps ?? DEFAULT_APPS;
  const config = useAppStore((s) => s.config);
  const projects = config?.projects ?? EMPTY_PROJECTS;
  const [state, dispatch] = useDesktopReducer(EMPTY_LAYOUT, initialWorkspace());
  const hostAct = useRef<((action: WindowAction) => void) | null>(null);
  const [stopSignal, setStopSignal] = useState(0);
  const [resumeSignal, setResumeSignal] = useState(0);
  const [architectRequest, setArchitectRequest] = useState<ArchitectRequest | null>(null);
  const mobile = shared.mobile;
  const mobileRef = useRef(mobile);
  mobileRef.current = mobile;

  // Celular: o cliente do ui-state do projeto só grava a seção `mobile` (antes dos efeitos que
  // gravam; girar o aparelho troca o modo sem recriar o cliente).
  useLayoutEffect(() => {
    projectUi?.sync.setMobileOnly(mobile);
  }, [projectUi, mobile]);

  const phaseLayout = usePhaseLayout({
    ui: projectUi,
    state,
    dispatch,
    phase: space?.phase ?? "idle",
    ready: space !== null && space.status === "ready",
    measure: measureWorkspace,
    mobile,
  });
  const mobileView = useMobileView(projectUi, phaseLayout.category, mobile);
  const mobileViewRef = useRef(mobileView);
  mobileViewRef.current = mobileView;

  // --- Seleção salva no ui-state do projeto ---
  // No celular, a seleção lida é a do desktop com a do celular por cima, e as mudanças vão só
  // para a seção `mobile`: a seleção do desktop não muda.
  const rawSelection = projectUi === null ? undefined : getSection(projectUi.snapshot.data, UI_SECTIONS.selection);
  const mobileSelection = mobile ? mobileView.selection : null;
  const selection = useMemo(
    () => parseSelection(mobileSelection === null ? rawSelection : mergedSelection(rawSelection, mobileSelection)),
    [rawSelection, mobileSelection],
  );
  const desktopSelectionRef = useRef(rawSelection);
  desktopSelectionRef.current = rawSelection;
  const setSelection = useCallback(
    (patch: Partial<UiSelection>) => {
      if (mobileRef.current) {
        mobileViewRef.current.setSelection(patch);
        return;
      }
      projectUi?.set(UI_SECTIONS.selection, { ...parseSelection(desktopSelectionRef.current), ...patch });
    },
    [projectUi],
  );

  // --- Ações do shell ---
  const showToast = useCallback(
    (message: string, options?: ToastOptions) => {
      toastSeq += 1;
      const title = options?.title;
      shared.setToast({ id: toastSeq, message, undo: options?.undo === true, ...(title !== undefined ? { title } : {}) });
    },
    [shared.setToast],
  );
  const openWindow = useCallback(
    (kind: WindowKind) => {
      const act = hostAct.current;
      if (act !== null) act({ type: "open", kind });
      else dispatch({ type: "open", kind });
    },
    [dispatch],
  );
  const openSetup = useCallback(() => openWindow("setup"), [openWindow]);
  const goTo = useCallback(
    (projectId: string, kind: WindowKind) => {
      if (projectId === spaceId) {
        openWindow(kind);
        return;
      }
      shared.pendingOpen.current = { projectId, kind };
      switchSpace(projectId);
    },
    [spaceId, openWindow, switchSpace, shared.pendingOpen],
  );
  const openSettings = useCallback(
    (tab?: SettingsWindowTab) => {
      settingsSeq += 1;
      shared.setSettingsRequest({ id: settingsSeq, tab: tab ?? "connections" });
      openWindow("settings");
    },
    [openWindow, shared.setSettingsRequest],
  );
  const consumeSettingsRequest = useCallback(
    (id: number) => shared.setSettingsRequest((r) => (r?.id === id ? null : r)),
    [shared.setSettingsRequest],
  );
  const openNewProject = useCallback(() => shared.setNewProjectOpen(true), [shared.setNewProjectOpen]);
  const openLauncher = useCallback(() => shared.setLauncherOpen(true), [shared.setLauncherOpen]);

  const attention: Attention | null = runtime.attention;
  const attentionTotal = countAttention(attention);
  const incomplete = setupIncomplete(runtime.readiness) || attention?.setup_incomplete === true;

  // Acesso pela rede (spec 097): a faixa marcada como liberada, no estado da interface global.
  const globalState = shared.globalState;
  const network = useUiSection(globalState, UI_SECTIONS.network, parseNetworkState);
  const setLanReleased = useCallback(
    (range: LanRange | null) => {
      globalState.set(UI_SECTIONS.network, networkStateValue(range));
    },
    [globalState],
  );

  const shellApi = useMemo<ShellApi>(
    () => ({
      spaceId,
      switchSpace,
      openWindow,
      openNewProject,
      openSetup,
      openLauncher,
      attention,
      attentionCount: attentionTotal,
      refreshAttention: runtime.refreshAttention,
      readiness: runtime.readiness,
      reloadReadiness: runtime.reloadReadiness,
      recheckMachine: runtime.recheckMachine,
      showToast,
      goTo,
      openSettings,
      settingsRequest: shared.settingsRequest,
      consumeSettingsRequest,
      lanReleased: network.released,
      setLanReleased,
    }),
    [
      spaceId,
      switchSpace,
      openWindow,
      openNewProject,
      openSetup,
      openLauncher,
      attention,
      attentionTotal,
      runtime,
      showToast,
      goTo,
      openSettings,
      shared.settingsRequest,
      consumeSettingsRequest,
      network.released,
      setLanReleased,
    ],
  );

  const consumeArchitectRequest = useCallback((id: number) => {
    setArchitectRequest((r) => (r?.id === id ? null : r));
  }, []);
  const spaceValue = useMemo<SpaceContextValue | null>(
    () => (space === null ? null : { data: space, selection, setSelection, architectRequest, consumeArchitectRequest }),
    [space, selection, setSelection, architectRequest, consumeArchitectRequest],
  );

  // --- Primeiros passos: abre sozinha quando o chat ainda não está pronto (uma vez por aba) ---
  const readiness = runtime.readiness;
  const layoutReady = space === null || phaseLayout.initialized;
  useEffect(() => {
    if (readiness === null || readiness.chat.ok || !layoutReady || shared.setupAutoOpened.current) return;
    shared.setupAutoOpened.current = true;
    // Celular: a janela vira a vista atual (uma por vez).
    if (mobileRef.current) {
      mobileViewRef.current.setView("setup");
      return;
    }
    // Sem roubar o foco: só aparece na frente.
    dispatch({ type: "open", kind: "setup" });
  }, [readiness, layoutReady, dispatch, shared.setupAutoOpened]);

  // --- Aviso com "Desfazer": vale enquanto a mudança anunciada for a última ---
  const toast = shared.toast;
  useEffect(() => {
    if (toast !== null && toast.undo && toast.anchor === undefined) {
      shared.setToast((t) => (t?.id === toast.id ? { ...t, anchor: state.previous } : t));
    }
  }, [toast, state.previous, shared.setToast]);
  const undoVisible = toast !== null && toast.undo && toast.anchor !== undefined && toast.anchor !== null && state.previous === toast.anchor && canUndo(state);

  const phaseLayoutRef = useRef(phaseLayout);
  phaseLayoutRef.current = phaseLayout;
  const onLayoutCommand = useCallback(
    (command: LayoutCommand) => {
      const result = phaseLayoutRef.current.command(command);
      showToast(result.message, { undo: result.undoable });
    },
    [showToast],
  );

  // --- Abrir a janela pedida por `goTo` quando o novo espaço estiver pronto ---
  const layoutInitialized = phaseLayout.initialized;
  useEffect(() => {
    const pending = shared.pendingOpen.current;
    if (pending === null || space === null || pending.projectId !== space.projectId || !layoutInitialized) return;
    shared.pendingOpen.current = null;
    openWindow(pending.kind);
  }, [space, layoutInitialized, openWindow, shared.pendingOpen]);

  // --- Plano pronto: na TRANSIÇÃO para a etapa "Aguardando" (`planReady`, de `planStage`), abre a
  // janela Plano sem roubar o foco (só aparece na frente); planejando ou revisando, nunca ---
  const planReadyId = space?.planReady?.id ?? null;
  const planSeen = useRef<string | null | undefined>(undefined);
  useEffect(() => {
    if (space === null || space.status !== "ready" || !layoutInitialized) return;
    const previous = planSeen.current;
    planSeen.current = planReadyId;
    if (previous === undefined || planReadyId === null || planReadyId === previous) return;
    // Celular: não troca a tela do usuário; o Plano já é uma aba do planejamento (e há o sino).
    if (mobileRef.current) return;
    dispatch({ type: "open", kind: "plan" });
  }, [space, planReadyId, layoutInitialized, dispatch]);

  // --- Janelas do registro ---
  const pageVisible = usePageVisible();
  const conversationVisible = mobile ? pageVisible && mobileView.view === "conversation" : isWindowVisible(state.layout, "conversation", pageVisible);
  const conversationUnseen = useConversationUnseen(space?.projectId ?? null, conversationVisible);
  const spaceAttention = attentionFor(attention, space?.projectId ?? null);
  const summaryInput = useMemo<AppSummaryInput>(
    () => ({ space, readiness, projectCount: projects.length, attention: spaceAttention, conversationUnseen }),
    // `lang`: os resumos (subtítulos, selos, estados vazios) são refeitos no idioma novo.
    [space, readiness, projects.length, spaceAttention, conversationUnseen, lang],
  );
  const windows = useWindowSpecs(apps, summaryInput);

  const dockKinds = useMemo<WindowKind[] | undefined>(() => {
    if (space !== null) return undefined;
    // Boas-vindas: só as janelas globais.
    return state.layout.windows.setup?.open === true ? ["settings", "setup"] : ["settings"];
  }, [space, state.layout.windows.setup?.open]);
  const effectiveDock = dockKinds ?? DOCK_ORDER.filter((k) => k !== "setup" || state.layout.windows.setup?.open === true);

  // --- Lançador ---
  const goToAttention = useCallback(() => {
    const first = attention?.projects[0];
    if (first !== undefined && first.project_id !== spaceId) switchSpace(first.project_id);
    shared.setBellOpen(true);
  }, [attention, spaceId, switchSpace, shared.setBellOpen]);
  const approvePlan = useCallback(async () => {
    if (space === null || space.planReady === null || approvePlanBlocker(space, readiness) !== null) return;
    const result = await approveSpec(space.projectId, space.planReady.id);
    if (result.ok) {
      showToast(t("toast.planApproved"));
      await space.reload();
    } else {
      // 412: relê a prontidão (o item passa a mostrar o que falta) e usa os rótulos dela.
      const prereq = await prereqFailureMessage(result.error, runtime.reloadReadiness, readiness);
      showToast(prereq ?? resumeProblem(result.error));
    }
  }, [space, readiness, showToast, runtime.reloadReadiness]);
  const launcherItems = useCallback(
    (query: string): LauncherItem[] => {
      const items: LauncherItem[] = [
        { id: "new-project", group: "actions", label: t("common.newProject"), icon: "plus", run: openNewProject },
        { id: "open-settings", group: "actions", label: t("launcher.openSettings"), icon: "settings", run: () => openWindow("settings") },
        { id: "open-setup", group: "actions", label: t("window.gettingStarted"), icon: "setup", run: openSetup },
      ];
      // Só com o plano revisando (desabilitado) ou aguardando; planejando, sem o item.
      if (space !== null && (space.planStage === "reviewing" || space.planStage === "awaiting")) {
        const reason = approvePlanBlocker(space, readiness);
        items.push({
          id: "approve-plan",
          group: "actions",
          label: t("launcher.approvePlan"),
          icon: "check",
          ...(reason !== null ? { disabled: reason } : {}),
          run: () => void approvePlan(),
        });
      }
      if (space?.running === true) {
        items.push({ id: "stop-run", group: "actions", label: t("launcher.stopRun"), icon: "stop", run: () => setStopSignal((n) => n + 1) });
      } else if (space !== null && (space.paused || space.stalled)) {
        items.push({
          id: "resume-run",
          group: "actions",
          label: space.stalled ? t("launcher.continueRun") : t("launcher.resumeRun"),
          icon: "play",
          ...(gateBlocked(readiness, "execute") ? { disabled: missingText(readiness) } : {}),
          run: () => setResumeSignal((n) => n + 1),
        });
      }
      if (attentionTotal > 0) {
        items.push({ id: "attention", group: "actions", label: t("launcher.goToAttention"), icon: "bell", hint: String(attentionTotal), run: goToAttention });
      }
      const text = query.trim();
      if (space !== null && text !== "") {
        items.push({
          id: "ask-architect",
          group: "actions",
          label: t("launcher.askArchitect", { text }),
          icon: "conversation",
          always: true,
          run: () => {
            requestSeq += 1;
            setArchitectRequest({ id: requestSeq, text });
            openWindow("conversation");
          },
        });
      }
      effectiveDock.forEach((kind, index) => {
        const win = state.layout.windows[kind];
        const status = win?.open !== true ? undefined : win.mode === "minimized" ? t("dock.state.minimized") : t("dock.state.open");
        const slot = index + 1;
        const aria = dockSlotAria(slot);
        items.push({
          id: `window-${kind}`,
          group: "windows",
          label: WINDOW_META[kind].title,
          icon: WINDOW_META[kind].icon,
          ...(windows[kind]?.subtitle !== undefined ? { hint: windows[kind]?.subtitle } : status !== undefined ? { hint: status } : {}),
          ...(aria !== undefined ? { keys: aria, aria } : {}),
          run: () => openWindow(kind),
        });
      });
      // Fases lidas na hora (sem assinar o store inteiro: eventos de outros projetos não
      // re-renderizam o desktop).
      const store = appStore.getState();
      for (const p of projects) {
        if (p.id === spaceId) continue;
        const phase = phaseForProject(store.phases[p.id], store.projectStates[p.id] ?? null, store.architectTurns[p.id] !== undefined);
        items.push({ id: `project-${p.id}`, group: "projects", label: p.name, icon: "project", hint: PHASE_LABELS[phase], run: () => switchSpace(p.id) });
      }
      return items;
    },
    [space, readiness, attentionTotal, effectiveDock, state.layout, windows, projects, spaceId, openNewProject, openWindow, openSetup, approvePlan, goToAttention, switchSpace, lang],
  );

  // --- Novo projeto ---
  const onCreated = useCallback(
    (project: Project) => {
      switchSpace(project.id);
      void runtime.reloadReadiness();
      runtime.refreshAttention();
      showToast(t("toast.projectCreated", { name: project.name }));
    },
    [switchSpace, runtime, showToast],
  );

  // --- Conteúdo central sem janelas ---
  let emptyContent: ReactNode | undefined;
  if (space === null) {
    emptyContent = <Welcome projects={projects} onNewProject={openNewProject} onOpenSetup={openSetup} onOpenProject={switchSpace} />;
  } else if (space.status === "not_found") {
    emptyContent = <SpaceNotFound onBack={() => switchSpace(null)} />;
  } else if (space.status === "loading" || !phaseLayout.initialized) {
    emptyContent = <SpaceLoading />;
  }
  // Celular sem espaço: as boas-vindas são a aba Início; outra vista mostra a janela escolhida.
  if (mobile && space === null && mobileView.view !== "home") emptyContent = undefined;

  const mobileControl = useMemo<MobileControl | undefined>(
    () =>
      mobile
        ? { view: mobileView.view, tabs: mobileView.tabs, closeView: mobileView.closeView, onView: mobileView.setView }
        : undefined,
    [mobile, mobileView.view, mobileView.tabs, mobileView.closeView, mobileView.setView],
  );

  const layoutControl = useMemo(
    () => (space !== null && space.status === "ready" ? { hasMine: phaseLayout.layouts[phaseLayout.category].mine !== null, onCommand: onLayoutCommand } : undefined),
    [space?.status, phaseLayout.layouts, phaseLayout.category, onLayoutCommand],
  );
  const toggleBell = useCallback(() => shared.setBellOpen((v) => !v), [shared.setBellOpen]);
  const topBar = (
    <TopBar
      spaceId={spaceId}
      space={space}
      projects={projects}
      attention={attention}
      attentionCount={attentionTotal}
      setupIncomplete={incomplete}
      compact={mobile}
      {...(layoutControl !== undefined && !mobile ? { layout: layoutControl } : {})}
      bellOpen={shared.bellOpen}
      stopSignal={stopSignal}
      resumeSignal={resumeSignal}
      onSwitchSpace={switchSpace}
      onNewProject={openNewProject}
      onOpenSetup={openSetup}
      onOpenLauncher={openLauncher}
      onToggleBell={toggleBell}
    />
  );

  const overlay = (
    <>
      <HostBridge target={hostAct} />
      {space !== null && space.loadError !== null && space.status !== "not_found" ? (
        <div className="nd-bar-alert nd-overlay" role="alert" data-bar-alert="load">
          <Icon name="alert" size={16} className="nd-bar-alert-icon" />
          <span className="nd-bar-alert-text">{space.loadError}</span>
          <button type="button" className="nd-btn" onClick={() => void space.reload()}>
            {t("common.tryAgain")}
          </button>
        </div>
      ) : null}
      {space !== null && space.orphanPermissions.length > 0 ? (
        <div className="nd-bar-note nd-overlay" role="status" data-orphan-permissions="">
          <Icon name="bell" size={14} />
          {tp("shell.orphanPermissions", space.orphanPermissions.length)}
        </div>
      ) : null}
      {shared.bellOpen ? (props.renderAttentionPanel ?? renderDefaultAttentionPanel)({ onClose: () => shared.setBellOpen(false) }) : null}
      <PermissionToasts />
      {shared.launcherOpen ? <Launcher items={launcherItems} onClose={() => shared.setLauncherOpen(false)} /> : null}
      {shared.newProjectOpen ? <NewProjectDialog onClose={() => shared.setNewProjectOpen(false)} onCreated={onCreated} /> : null}
      {toast !== null ? (
        <div className="nd-toast nd-overlay" role="status" data-toast="" data-titled={toast.title !== undefined ? "true" : undefined}>
          {toast.title !== undefined ? (
            <>
              <Icon name="check" size={18} className="nd-toast-icon" />
              <span className="nd-toast-text">
                <span className="nd-toast-title">{toast.title}</span>
                <span className="nd-toast-body">{toast.message}</span>
              </span>
            </>
          ) : (
            <span>{toast.message}</span>
          )}
          {undoVisible ? (
            <button
              type="button"
              className="nd-btn"
              data-undo=""
              onClick={() => {
                const act = hostAct.current;
                if (act !== null) act({ type: "undo" });
                else dispatch({ type: "undo" });
                shared.setToast(null);
              }}
            >
              <Icon name="undo" size={14} />
              {t("common.undo")}
            </button>
          ) : null}
          <button type="button" className="nd-wbtn" aria-label={t("toast.close")} onClick={() => shared.setToast(null)}>
            <Icon name="close" size={14} />
          </button>
        </div>
      ) : null}
    </>
  );

  return (
    <ShellContext.Provider value={shellApi}>
      <SpaceContext.Provider value={spaceValue}>
        <Desktop
          state={state}
          dispatch={dispatch}
          globalState={shared.globalState}
          windows={windows}
          {...(dockKinds !== undefined ? { dockKinds } : {})}
          topBar={topBar}
          onOpenLauncher={openLauncher}
          {...(emptyContent !== undefined ? { emptyContent } : {})}
          overlay={overlay}
          {...(props.createObserver !== undefined ? { createObserver: props.createObserver } : {})}
          {...(props.scheduler !== undefined ? { scheduler: props.scheduler } : {})}
          {...(mobileControl !== undefined ? { mobile: mobileControl } : {})}
        />
      </SpaceContext.Provider>
    </ShellContext.Provider>
  );
}
