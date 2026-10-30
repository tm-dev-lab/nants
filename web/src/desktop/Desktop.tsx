/*
 * Desktop (spec 093, decisões 4, 9, 13 e 15): papel de parede, barra superior (o conteúdo vem
 * da T05), área de trabalho com as janelas do espaço ativo, região `role="status"` fixa
 * para os anúncios do gerenciador, dicas contextuais, folha de atalhos e Personalizar. No começo
 * da barra fica o botão N, que abre o menu do N (spec 137, `NMenu.tsx`) com a grade dos mesmos
 * `kinds` em ordem (`APP_ORDER`). Sem dock (137/T02): a área de trabalho vai até a borda de baixo.
 *
 * Controlado: o estado do gerenciador (`DesktopState`) e o `dispatch` vêm de fora
 * (`useDesktopReducer`), para o shell trocar de espaço e de layout por fase. A aparência e as
 * dicas ficam no ui-state global (`appearance`, `hints`).
 *
 * As janelas são desenhadas sempre na ordem de `WINDOW_KINDS`: o DOM nunca é reordenado (só o
 * `z-index` muda), para um iframe não recarregar.
 *
 * Modo celular (`mobile`, T10; decisão 17): o mesmo Desktop, com a barra compacta (conteúdo do
 * shell), uma janela por vez em tela cheia (arranjo derivado por `mobileLayout`, sem tocar no
 * arranjo do desktop), o Alternador em lista e as abas embaixo. As janelas continuam no
 * mesmo lugar da árvore: trocar de modo não as remonta.
 */
import "./theme/desktop.css";
import "./theme/mobile.css";
import { localized, t, type Lang } from "../i18n";
import { useT } from "../i18n/react";
import { chooseLanguage } from "../i18n/uiStateBinding";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type CSSProperties,
  type DragEvent as ReactDragEvent,
  type ReactNode,
} from "react";
import { prefersReducedMotion, waitForAnimations } from "./animation";
import type { AppBadge } from "./appState";
import { applyFocus, createFocusMemory, cycleWindow, focusAfterHide, focusAfterRestore, type FocusTarget } from "./focus";
import { snapAnnouncement, snapPreviewRect, type GestureMode } from "./gestures";
import { DesktopLayerContext } from "./layer";
import { HINTS, markHintSeen, parseHints, shouldShowHint, type HintId } from "./hints";
import { Icon } from "./Icon";
import { mobileLayout, viewWindow, type MobileView } from "./mobile";
import { MobileSwitcher } from "./MobileSwitcher";
import { mobileTabId, MobileTabs } from "./MobileTabs";
import { NButton, NMenu, pendingCount, type NMenuCloseReason } from "./NMenu";
import { Personalize } from "./Personalize";
import { ShortcutSheet } from "./ShortcutSheet";
import { matchShortcut, shortcutById, shortcutContext } from "./shortcuts";
import { applyThemeMeta, parseAppearance, useResolvedTheme, type Appearance } from "./theme/appearance";
import { themeVariables, wallpaperStyle } from "./theme/tokens";
import { UI_SECTIONS } from "./uiState";
import type { ResizeObserverFactory, ContainerSize } from "./useContainerSize";
import { useUiSection, type UiStateHandle } from "./useUiState";
import { useGestureActive, type FrameScheduler } from "./useWindowGesture";
import { WindowLayoutContext } from "./useWindowVisibility";
import { AppearanceContext, type AppearanceApi } from "./theme/appearanceContext";
import { Window, type WindowEmpty, type WindowScroll } from "./Window";
import { WindowHostContext, type WindowHost } from "./windowHost";
import { useAppHeight, useKeyboardMode } from "./viewport";
import { APP_ORDER, WINDOW_META } from "./windowMeta";
import {
  canUndo,
  createDesktopState,
  displayedLayout,
  isWindowKind,
  shownByZ,
  topWindow,
  WINDOW_KINDS,
  windowsReducer,
  type DesktopState,
  type Layout,
  type SnapTarget,
  type WindowAction,
  type WindowKind,
  type Workspace,
} from "./windows";

/** O que o shell fornece para cada janela. */
export interface WindowSpec {
  /** Subtítulo de estado vivo (barra de título e dica no menu do N). */
  subtitle?: string;
  /** Selo do app no menu do N e no celular (pendências, "N novas"). */
  badge?: AppBadge;
  /** Estado vazio (padrão: o do tipo de janela, com a ação padrão). */
  empty?: WindowEmpty;
  /** Conteúdo; ausente mostra o estado vazio. */
  content?: ReactNode | ((size: ContainerSize) => ReactNode);
  /** Quem rola o conteúdo (`AppDefinition.scroll`; padrão: a janela). */
  scroll?: WindowScroll;
  /** Ferramentas na barra de título (slot `[data-window-tools]`, 095/T02, C-07). */
  tools?: ReactNode;
}

/** Modo celular (decisão 17): a vista atual, as abas e a troca de vista (estado no shell). */
export interface MobileControl {
  /** O que ocupa a tela: uma janela, o Alternador ou o início (boas-vindas). */
  view: MobileView;
  /** Abas da barra inferior. */
  tabs: readonly MobileView[];
  /** Para onde vai ao fechar a janela atual. */
  closeView: MobileView;
  onView: (view: MobileView) => void;
}

export interface DesktopProps {
  state: DesktopState;
  dispatch: (action: WindowAction) => void;
  /** ui-state global (`appearance` e `hints`). */
  globalState: UiStateHandle;
  windows?: Partial<Record<WindowKind, WindowSpec>>;
  /** Ordem dos apps (padrão: `APP_ORDER`, com Primeiros passos só quando aberta). */
  appKinds?: readonly WindowKind[];
  /** Conteúdo da barra superior (marca, espaços, pílula... na T05). */
  topBar?: ReactNode;
  /** Ctrl+K. */
  onOpenLauncher?: () => void;
  /** Conteúdo da área de trabalho sem janelas (boas-vindas, carregando, não encontrado). */
  emptyContent?: ReactNode;
  /** Diálogos, lançador e avisos do shell (desenhados acima da área de trabalho). */
  overlay?: ReactNode;
  /** Testes: observador de tamanho das janelas e agendador de quadros dos gestos. */
  createObserver?: ResizeObserverFactory | null;
  scheduler?: FrameScheduler;
  /** Modo celular (ausente = desktop). */
  mobile?: MobileControl;
  /** Versão do Nants, no topo do menu do N. */
  version?: string;
  /** Pedido de abrir o menu do N (o lançador incrementa; spec 137). */
  nMenuSignal?: number;
}

/** Área do celular antes da primeira medição (jsdom não mede): a janela menos barra e abas. */
function initialMobileWorkspace(): Workspace {
  if (typeof window === "undefined") return { w: 390, h: 640 };
  return { w: Math.max(1, window.innerWidth), h: Math.max(1, window.innerHeight - 160) };
}

/** Estado do gerenciador com o reducer puro da T03. */
export function useDesktopReducer(initial: Layout, workspace: Workspace): [DesktopState, (action: WindowAction) => void] {
  return useReducer(windowsReducer, undefined, () => createDesktopState(initial, workspace));
}

/** Tempo que uma dica fica visível sem ser dispensada (ms). */
const HINT_TIMEOUT_MS = 10_000;

const SIZE_LABELS = localized({ compact: "announce.size.compact", medium: "announce.size.medium", wide: "announce.size.wide" });

function defaultAppKinds(layout: Layout): WindowKind[] {
  return APP_ORDER.filter((kind) => kind !== "setup" || layout.windows.setup?.open === true);
}

/**
 * Arquivo solto fora de uma zona que aceita (a Conversa para os eventos dela, com
 * `stopPropagation`): o navegador não pode abrir o arquivo nem sair da página. `dragover` e `drop` têm `preventDefault`, e o cursor
 * mostra que ali não se solta nada.
 */
function guardFileDrop(event: ReactDragEvent<HTMLElement>): void {
  // Só arquivos: arrastar texto para um campo continua com o comportamento do navegador.
  const types = Array.from(event.dataTransfer?.types ?? []);
  if (!types.includes("Files")) return;
  event.preventDefault();
  if (event.type === "dragover") {
    try {
      event.dataTransfer.dropEffect = "none";
    } catch {
      // Alguns navegadores não deixam trocar o efeito fora do evento: o preventDefault basta.
    }
  }
}

export function Desktop({
  state,
  dispatch,
  globalState,
  windows,
  appKinds,
  topBar,
  onOpenLauncher,
  emptyContent,
  overlay,
  createObserver,
  scheduler,
  mobile,
  version,
  nMenuSignal,
}: DesktopProps) {
  const mobileOn = mobile !== undefined;
  // Spec 101: a moldura re-renderiza na troca de idioma (rótulos, estados vazios, anúncios).
  const { t: tr, lang } = useT();
  // Celular: `--app-height` (viewport visual) e o modo "teclado aberto" (barra e abas somem).
  useAppHeight();
  useKeyboardMode();
  const appearance = useUiSection(globalState, UI_SECTIONS.appearance, parseAppearance);
  const hints = useUiSection(globalState, UI_SECTIONS.hints, parseHints);
  const theme = useResolvedTheme(appearance.theme);
  const variables = useMemo(() => themeVariables(theme) as CSSProperties, [theme]);
  // Barra do navegador (celular) e controles nativos acompanham o tema efetivo.
  useEffect(() => applyThemeMeta(theme), [theme]);
  const gestureActive = useGestureActive();

  const layout = state.layout;
  const kinds = useMemo(() => appKinds ?? defaultAppKinds(layout), [appKinds, layout]);

  // --- Celular: janela da vista em tela cheia; as visitadas continuam montadas (ocultas) ---
  const [mobileWorkspace, setMobileWorkspace] = useState<Workspace>(initialMobileWorkspace);
  const [mobileMounted, setMobileMounted] = useState<readonly WindowKind[]>([]);
  // A janela da vista não aparece enquanto o shell mostra outro conteúdo (carregando, boas-vindas).
  const mobileActive = mobile === undefined || emptyContent !== undefined ? null : viewWindow(mobile.view);
  if (mobileActive !== null && !mobileMounted.includes(mobileActive)) {
    setMobileMounted([...mobileMounted, mobileActive]);
  }
  // A última janela mostrada em tela cheia: a "vista atual" marcada no Alternador (094/T08, A-12),
  // só enquanto ela continua aberta.
  const [mobileLast, setMobileLast] = useState<WindowKind | null>(null);
  if (mobileActive !== null && mobileActive !== mobileLast) setMobileLast(mobileActive);
  // Desktop: o arranjo exibido é o salvo ajustado à área atual (mudar o tamanho do navegador não grava nada).
  const desktopWorkspace = state.workspace;
  const shownLayout = useMemo(
    () =>
      mobileOn
        ? mobileLayout(layout, mobileActive, mobileMounted, mobileWorkspace)
        : displayedLayout({ layout, workspace: desktopWorkspace }),
    [mobileOn, layout, mobileActive, mobileMounted, mobileWorkspace, desktopWorkspace],
  );
  const workspace = mobileOn ? mobileWorkspace : state.workspace;
  const top = mobileOn ? mobileActive : topWindow(layout);

  // Refs com o último valor (os callbacks do host são estáveis).
  const stateRef = useRef(state);
  stateRef.current = state;
  const dispatchRef = useRef(dispatch);
  dispatchRef.current = dispatch;
  const kindsRef = useRef(kinds);
  kindsRef.current = kinds;
  const hintsRef = useRef(hints);
  hintsRef.current = hints;
  const mobileRef = useRef(mobile);
  mobileRef.current = mobile;

  const rootRef = useRef<HTMLDivElement | null>(null);
  const workRef = useRef<HTMLDivElement | null>(null);
  const [menuLayer, setMenuLayer] = useState<HTMLDivElement | null>(null);
  const [overlayLayer, setOverlayLayer] = useState<HTMLDivElement | null>(null);
  const focusMemory = useMemo(() => createFocusMemory(), []);
  const pendingFocus = useRef<FocusTarget | null>(null);

  // --- Anúncios (role="status" fixo) ---
  const [announcement, setAnnouncement] = useState("");
  const announce = useCallback((message: string) => {
    // A mesma frase de novo ainda precisa ser lida: um espaço fino a diferencia.
    setAnnouncement((current) => (current === message ? `${message} ` : message));
  }, []);
  // Minimizar ou fechar a última janela: o aviso da área vazia completa o anúncio da ação.
  const hideMessage = useRef<string | null>(null);
  const wasEmpty = useRef(false);

  // --- Dicas ---
  const [activeHint, setActiveHint] = useState<HintId | null>(null);
  const showHint = useCallback(
    (id: HintId) => {
      const current = hintsRef.current;
      if (!shouldShowHint(current, id)) return;
      setActiveHint(id);
      globalState.set(UI_SECTIONS.hints, markHintSeen(current, id));
    },
    [globalState],
  );
  useEffect(() => {
    if (activeHint === null) return undefined;
    const timer = setTimeout(() => setActiveHint(null), HINT_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [activeHint]);

  /**
   * Celular: as ações do gerenciador viram trocas de vista. Abrir, focar ou restaurar mostra a
   * janela em tela cheia; fechar ou minimizar volta ao Alternador (ou ao início). Gestos,
   * encaixe, tamanhos e desfazer não existem no celular e não mexem no arranjo do desktop.
   */
  const mobileAct = useCallback(
    (control: MobileControl, action: WindowAction) => {
      if (!("kind" in action)) return;
      const kind = action.kind;
      const title = WINDOW_META[kind].title;
      switch (action.type) {
        case "open":
        case "focus":
        case "restore":
          if (control.view === kind) return;
          control.onView(kind);
          pendingFocus.current = focusAfterRestore(kind, focusMemory);
          announce(t("announce.opened", { title }));
          return;
        case "close":
        case "minimize": {
          setMobileMounted((current) => current.filter((k) => k !== kind));
          if (control.view !== kind) return;
          if (action.type === "close") focusMemory.forget(kind);
          control.onView(control.closeView);
          const tab = document.getElementById(mobileTabId(control.closeView));
          pendingFocus.current = tab === null ? null : { type: "element", element: tab };
          announce(t("announce.closed", { title }));
          return;
        }
        default:
      }
    },
    [announce, focusMemory],
  );

  // --- Ações com foco e anúncio ---
  const act = useCallback(
    (action: WindowAction) => {
      const control = mobileRef.current;
      if (control !== undefined) {
        mobileAct(control, action);
        return;
      }
      const before = stateRef.current.layout;
      dispatchRef.current(action);
      if (action.type === "undo") {
        announce(t("announce.undo"));
        return;
      }
      if (!("kind" in action)) return;
      const kind = action.kind;
      const title = WINDOW_META[kind].title;
      const win = before.windows[kind];
      switch (action.type) {
        case "minimize":
          if (win?.open !== true || win.mode === "minimized") return;
          pendingFocus.current = focusAfterHide();
          hideMessage.current = t("announce.minimized", { title });
          announce(hideMessage.current);
          showHint("menu");
          return;
        case "close":
          if (win?.open !== true) return;
          pendingFocus.current = focusAfterHide();
          focusMemory.forget(kind);
          hideMessage.current = t("announce.closed", { title });
          announce(hideMessage.current);
          return;
        case "restore":
        case "focus":
        case "open":
          if (win?.open === true && win.mode === "minimized") {
            pendingFocus.current = focusAfterRestore(kind, focusMemory);
            announce(t("announce.restored", { title }));
          } else if (win?.open !== true && action.type === "open") {
            pendingFocus.current = { type: "titleBar", kind };
            announce(t("announce.opened", { title }));
          }
          return;
        case "toggleMaximize":
          if (win?.open !== true || win.mode === "minimized") return;
          announce(t(win.mode === "maximized" ? "announce.restored" : "announce.maximized", { title }));
          return;
        case "snap":
          announce(snapAnnouncement(title, action.target));
          return;
        case "size":
          announce(t("announce.size", { title, size: SIZE_LABELS[action.size] }));
          return;
        default:
      }
    },
    [announce, focusMemory, showHint, mobileAct],
  );

  useEffect(() => {
    const target = pendingFocus.current;
    if (target === null) return;
    pendingFocus.current = null;
    applyFocus(target);
  }, [shownLayout]);

  /** Menu do N e Alt+N: abre, restaura, foca; a janela em foco é minimizada (só pela grade do menu). */
  const activate = useCallback(
    (kind: WindowKind, fromGrid: boolean) => {
      const current = stateRef.current.layout;
      const win = current.windows[kind];
      if (win === undefined || !win.open) {
        act({ type: "open", kind });
        return;
      }
      if (win.mode === "minimized") {
        act({ type: "restore", kind });
        return;
      }
      if (fromGrid && topWindow(current) === kind) {
        act({ type: "minimize", kind });
        return;
      }
      act({ type: "focus", kind });
      pendingFocus.current = focusAfterRestore(kind, focusMemory);
      // Já na frente: o layout não muda; aplica o foco agora.
      if (topWindow(current) === kind) {
        const target = pendingFocus.current;
        pendingFocus.current = null;
        applyFocus(target);
      }
    },
    [act, focusMemory],
  );

  // --- Prévia de encaixe ---
  const [snapPreview, setSnapPreview] = useState<{ kind: WindowKind; target: SnapTarget } | null>(null);
  const onSnapPreview = useCallback((kind: WindowKind, target: SnapTarget | null) => {
    setSnapPreview((current) => {
      if (target === null) return current?.kind === kind ? null : current;
      return { kind, target };
    });
  }, []);
  const onGestureStart = useCallback(
    (_kind: WindowKind, mode: GestureMode) => {
      if (mode === "move") showHint("snap");
    },
    [showHint],
  );

  // --- Área de trabalho: mede e informa o gerenciador ---
  // No celular, a medida fica só aqui (tela cheia): o arranjo do desktop não é renormalizado
  // para a tela pequena. Trocar de modo mede de novo.
  useLayoutEffect(() => {
    const element = workRef.current;
    if (element === null) return undefined;
    const measure = (): void => {
      const r = element.getBoundingClientRect();
      if (r.width <= 0 || r.height <= 0) return;
      if (mobileRef.current !== undefined) {
        const w = Math.floor(r.width);
        const h = Math.floor(r.height);
        setMobileWorkspace((current) => (current.w === w && current.h === h ? current : { w, h }));
        return;
      }
      const ws = stateRef.current.workspace;
      if (Math.floor(r.width) === ws.w && Math.floor(r.height) === ws.h) return;
      dispatchRef.current({ type: "setWorkspace", workspace: { w: r.width, h: r.height } });
    };
    measure();
    window.addEventListener("resize", measure);
    const observer = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    observer?.observe(element);
    return () => {
      window.removeEventListener("resize", measure);
      observer?.disconnect();
    };
  }, [mobileOn]);

  const host = useMemo<WindowHost>(
    () => ({
      act,
      announce,
      rememberFocus: (kind, element) => focusMemory.remember(kind, element),
      snapPreview: onSnapPreview,
      gestureStart: onGestureStart,
      workspaceOrigin: () => {
        const r = workRef.current?.getBoundingClientRect();
        return { x: r?.left ?? 0, y: r?.top ?? 0 };
      },
      menuLayer,
      // Celular: troca de vista sem animação (uma janela por vez, sem o N para onde ir).
      reducedMotion: () => mobileOn || prefersReducedMotion(),
      ...(createObserver !== undefined ? { createObserver } : {}),
      ...(scheduler !== undefined ? { scheduler } : {}),
      ...(mobileOn ? { mobile: true } : {}),
    }),
    [act, announce, focusMemory, onSnapPreview, onGestureStart, menuLayer, createObserver, scheduler, mobileOn],
  );

  // --- Folha de atalhos e Personalizar ---
  const [sheetOpen, setSheetOpen] = useState(false);
  const [personalizeOpen, setPersonalizeOpen] = useState(false);
  const returnFocus = useRef<HTMLElement | null>(null);
  const personalizeButton = useRef<HTMLButtonElement | null>(null);
  const openSheet = useCallback(() => {
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setPersonalizeOpen(false);
    setSheetOpen(true);
  }, []);
  const closeSheet = useCallback(() => {
    setSheetOpen(false);
    const target = returnFocus.current;
    returnFocus.current = null;
    if (target?.isConnected === true) target.focus({ preventScroll: true });
  }, []);
  const closePersonalize = useCallback(() => {
    setPersonalizeOpen(false);
    personalizeButton.current?.focus({ preventScroll: true });
  }, []);
  // --- Menu do N (spec 137): sempre começa fechado; "closing" é a animação de subida ---
  const [nMenu, setNMenu] = useState<"closed" | "open" | "closing">("closed");
  const nButtonRef = useRef<HTMLButtonElement | null>(null);
  const nPanelRef = useRef<HTMLDivElement | null>(null);
  const openNMenu = useCallback(() => {
    setSheetOpen(false);
    setPersonalizeOpen(false);
    setNMenu("open");
  }, []);
  const closeNMenu = useCallback((reason: NMenuCloseReason | "choose") => {
    const panel = nPanelRef.current;
    // Sem animação (movimento reduzido ou sem `getAnimations`), sai na hora.
    const animated = panel !== null && typeof panel.getAnimations === "function" && !prefersReducedMotion();
    setNMenu(animated ? "closing" : "closed");
    // Pelo teclado ou pelo véu, o foco volta ao N; ao escolher, vai para a janela.
    if (reason === "key" || reason === "scrim") nButtonRef.current?.focus({ preventScroll: true });
  }, []);
  useEffect(() => {
    if (nMenu !== "closing") return undefined;
    const panel = nPanelRef.current;
    let live = true;
    const done = (): void => {
      if (live) setNMenu((current) => (current === "closing" ? "closed" : current));
    };
    if (panel === null) done();
    else void waitForAnimations(panel).then(done);
    return () => {
      live = false;
    };
  }, [nMenu]);
  const toggleNMenu = useCallback(() => {
    if (nMenu === "open") closeNMenu("key");
    else openNMenu();
  }, [nMenu, closeNMenu, openNMenu]);
  // No celular não há menu do N (a barra de baixo é própria): trocar de modo o fecha.
  useEffect(() => {
    if (mobileOn) setNMenu("closed");
  }, [mobileOn]);
  // O lançador pede o menu (item "Abrir o menu do Nants").
  const nSignalSeen = useRef(nMenuSignal);
  useEffect(() => {
    if (nMenuSignal === nSignalSeen.current) return;
    nSignalSeen.current = nMenuSignal;
    if (mobileRef.current === undefined) openNMenu();
  }, [nMenuSignal, openNMenu]);

  const changeAppearance = useCallback(
    (next: Appearance) => {
      globalState.set(UI_SECTIONS.appearance, next);
    },
    [globalState],
  );

  // Idioma (spec 101): vale na hora e fica na seção global `language` do servidor.
  const changeLanguage = useCallback((next: Lang) => chooseLanguage(next, globalState), [globalState]);

  // Aparência para as janelas (aba Aparência das Configurações, tema do CodeMirror).
  const appearanceApi = useMemo<AppearanceApi>(
    () => ({ appearance, theme, setAppearance: changeAppearance, setLanguage: changeLanguage }),
    [appearance, theme, changeAppearance, changeLanguage],
  );

  // --- Atalhos globais (tabela única) ---
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.defaultPrevented) return;
      const root = rootRef.current;
      const target = event.target;
      if (root === null || !(target instanceof Node) || (!root.contains(target) && target !== document.body)) return;
      // Com um modal aberto (diálogo do shell ou lançador), nenhum atalho do desktop age por trás
      // dele: o foco fica no modal e o Esc é dele.
      if (root.querySelector('[aria-modal="true"]') !== null) return;
      const active = document.activeElement;
      if (event.key === "Escape") {
        // Esc nunca fecha a janela: dentro do conteúdo, devolve o foco à barra de título.
        const content = active?.closest("[data-window-content]");
        const kind = content?.closest("[data-window]")?.getAttribute("data-window");
        if (isWindowKind(kind)) {
          event.preventDefault();
          applyFocus({ type: "titleBar", kind, ring: true });
        }
        return;
      }
      const match = matchShortcut(event, shortcutContext(active));
      if (match === null) return;
      // Celular: só o lançador (teclado externo); janelas, menu do N e desfazer não se aplicam.
      if (mobileRef.current !== undefined && match.id !== "launcher") return;
      const current = stateRef.current;
      switch (match.id) {
        case "launcher":
          if (onOpenLauncher === undefined) return;
          event.preventDefault();
          onOpenLauncher();
          return;
        case "nextWindow":
        case "previousWindow": {
          const inside = active?.closest("[data-window]")?.getAttribute("data-window");
          const from = isWindowKind(inside) ? inside : topWindow(current.layout);
          const next = cycleWindow(current.layout, from, match.id === "nextWindow" ? 1 : -1, kindsRef.current);
          if (next === null) return;
          event.preventDefault();
          act({ type: "focus", kind: next });
          if (topWindow(current.layout) === next) applyFocus({ type: "titleBar", kind: next, ring: true });
          else pendingFocus.current = { type: "titleBar", kind: next, ring: true };
          return;
        }
        case "appSlot": {
          const kind = kindsRef.current[(match.slot ?? 0) - 1];
          if (kind === undefined) return;
          event.preventDefault();
          activate(kind, false);
          return;
        }
        case "nMenu":
          event.preventDefault();
          openNMenu();
          return;
        case "shortcutSheet":
          event.preventDefault();
          openSheet();
          return;
        case "undoLayout":
          if (!canUndo(current)) return;
          event.preventDefault();
          act({ type: "undo" });
          return;
        case "windowMenu":
          return;
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [act, activate, onOpenLauncher, openSheet, openNMenu]);

  // --- Janelas ---
  const openKind = useCallback((kind: WindowKind) => act({ type: "open", kind }), [act]);
  const defaultEmpties = useMemo(() => {
    const out: Partial<Record<WindowKind, WindowEmpty>> = {};
    for (const kind of WINDOW_KINDS) {
      const meta = WINDOW_META[kind].empty;
      const action = meta.action;
      out[kind] =
        action === undefined
          ? { message: meta.message }
          : { message: meta.message, action: { label: action.label, onClick: () => openKind(action.open) } };
    }
    return out;
    // `lang`: as frases são lidas no idioma ativo; trocar o idioma refaz os estados vazios.
  }, [openKind, lang]);

  const subtitles = useMemo(() => {
    const out: Partial<Record<WindowKind, string>> = {};
    for (const kind of WINDOW_KINDS) {
      const subtitle = windows?.[kind]?.subtitle;
      if (subtitle !== undefined) out[kind] = subtitle;
    }
    return out;
  }, [windows]);
  const badges = useMemo(() => {
    const out: Partial<Record<WindowKind, AppBadge>> = {};
    for (const kind of WINDOW_KINDS) {
      const badge = windows?.[kind]?.badge;
      if (badge !== undefined) out[kind] = badge;
    }
    return out;
  }, [windows]);

  const shown = shownByZ(shownLayout);
  // Sem janelas à vista e com projeto aberto: só o papel de parede (spec 098).
  const desktopEmpty = shown.length === 0 && emptyContent === undefined && !mobileOn;
  // O aviso vai só para o leitor de tela, pelo `role="status"` fixo (nada visível).
  const emptyMessage = tr("announce.noWindows", { keys: shortcutById("launcher")?.keys ?? tr("announce.launcherFallback") });
  useEffect(() => {
    const action = hideMessage.current;
    hideMessage.current = null;
    if (desktopEmpty && !wasEmpty.current) announce(action !== null ? `${action}. ${emptyMessage}` : emptyMessage);
    wasEmpty.current = desktopEmpty;
  }, [desktopEmpty, shownLayout, emptyMessage, announce]);
  const previewWin = snapPreview === null ? undefined : shownLayout.windows[snapPreview.kind];
  const previewRect =
    snapPreview !== null && previewWin !== undefined && !mobileOn ? snapPreviewRect(snapPreview.target, previewWin, state.workspace) : null;
  const mountedKinds = useMemo(
    () => new Set(WINDOW_KINDS.filter((k) => shownLayout.windows[k]?.open === true)),
    [shownLayout],
  );
  /** Alternador: abre a janela em tela cheia (o foco vai para ela). */
  const openFromSwitcher = useCallback((kind: WindowKind) => act({ type: "open", kind }), [act]);
  /** Aba: troca a vista; o foco fica na aba. */
  const selectTab = useCallback((view: MobileView) => {
    const control = mobileRef.current;
    if (control === undefined || control.view === view) return;
    control.onView(view);
  }, []);
  const sheetShortcut = shortcutById("shortcutSheet");
  const pending = useMemo(() => pendingCount(kinds, badges), [kinds, badges]);
  /** Item da grade: abre, foca, restaura ou minimiza a que está em foco. */
  const onMenuActivate = useCallback(
    (kind: WindowKind) => {
      closeNMenu("choose");
      activate(kind, true);
    },
    [closeNMenu, activate],
  );
  /** Janela aberta ou Alt+1…9 com o menu aberto: abre, foca ou restaura. */
  const onMenuShow = useCallback(
    (kind: WindowKind) => {
      closeNMenu("choose");
      activate(kind, false);
    },
    [closeNMenu, activate],
  );

  const tree = (
    <WindowLayoutContext.Provider value={shownLayout}>
      <WindowHostContext.Provider value={host}>
        <div
          ref={rootRef}
          className="nd-root"
          data-theme={theme}
          data-theme-preference={appearance.theme}
          data-wallpaper={appearance.wallpaper}
          data-gesture={gestureActive ? "true" : undefined}
          data-mobile={mobileOn ? "true" : undefined}
          style={variables}
          onDragOver={guardFileDrop}
          onDrop={guardFileDrop}
        >
          <div className="nd-wallpaper" aria-hidden="true" data-desktop-wallpaper="" style={wallpaperStyle(appearance.wallpaper, theme)} />
          <header className={mobileOn ? "nd-topbar nd-chrome nd-keyboard-hide" : "nd-topbar nd-chrome"}>
            {mobileOn ? null : <NButton buttonRef={nButtonRef} open={nMenu === "open"} pending={pending} onToggle={toggleNMenu} />}
            <div className="nd-topbar-slot">{topBar}</div>
            {mobileOn ? null : (
              <>
                <button
                  type="button"
                  className="nd-btn nd-iconbtn"
                  data-variant="ghost"
                  aria-label={tr("desktop.shortcuts")}
                  aria-keyshortcuts={sheetShortcut?.aria}
                  aria-haspopup="dialog"
                  aria-expanded={sheetOpen}
                  onClick={() => (sheetOpen ? closeSheet() : openSheet())}
                >
                  <Icon name="keyboard" size={16} />
                </button>
                <button
                  ref={personalizeButton}
                  type="button"
                  className="nd-btn nd-iconbtn"
                  data-variant="ghost"
                  aria-label={tr("desktop.personalize")}
                  aria-haspopup="dialog"
                  aria-expanded={personalizeOpen}
                  onClick={() => {
                    setSheetOpen(false);
                    setPersonalizeOpen((open) => !open);
                  }}
                >
                  <Icon name="palette" size={16} />
                </button>
              </>
            )}
          </header>
          <main ref={workRef} className="nd-workarea" aria-label={tr("desktop.workarea")} data-workarea="">
            {WINDOW_KINDS.map((kind) => {
              const win = shownLayout.windows[kind];
              if (win?.open !== true) return null;
              const spec = windows?.[kind];
              return (
                <Window
                  key={kind}
                  kind={kind}
                  win={win}
                  workspace={workspace}
                  active={top === kind}
                  {...(spec?.subtitle !== undefined ? { subtitle: spec.subtitle } : {})}
                  {...(spec?.scroll !== undefined ? { scroll: spec.scroll } : {})}
                  {...(spec?.tools !== undefined ? { tools: spec.tools } : {})}
                  empty={spec?.empty ?? defaultEmpties[kind]}
                >
                  {spec?.content}
                </Window>
              );
            })}
            {previewRect !== null ? (
              <div
                className="nd-snap-preview"
                data-snap-preview={snapPreview?.target}
                aria-hidden="true"
                style={{ left: previewRect.x, top: previewRect.y, width: previewRect.w, height: previewRect.h }}
              />
            ) : null}
            {shown.length === 0 && emptyContent !== undefined ? emptyContent : null}
            {mobile !== undefined && emptyContent === undefined && mobile.view === "switcher" ? (
              <MobileSwitcher kinds={kinds} open={mountedKinds} subtitles={subtitles} badges={badges} current={mobileLast !== null && mountedKinds.has(mobileLast) ? mobileLast : null} onOpen={openFromSwitcher} />
            ) : null}
            <div ref={setMenuLayer} className="nd-menu-layer" data-menu-layer="" />
          </main>
          {mobile !== undefined ? (
            <MobileTabs tabs={mobile.tabs} view={mobile.view} badges={badges} onSelect={selectTab} />
          ) : null}
          {nMenu !== "closed" && !mobileOn ? (
            <NMenu
              kinds={kinds}
              layout={layout}
              subtitles={subtitles}
              badges={badges}
              {...(version !== undefined ? { version } : {})}
              closing={nMenu === "closing"}
              panelRef={nPanelRef}
              buttonRef={nButtonRef}
              onActivate={onMenuActivate}
              onShow={onMenuShow}
              onClose={closeNMenu}
            />
          ) : null}
          {activeHint !== null && !mobileOn ? (
            <div className="nd-hint nd-overlay" role="note" data-hint={activeHint}>
              <Icon name="lightbulb" size={16} className="nd-hint-icon" />
              <span>{HINTS[activeHint]}</span>
              <button type="button" className="nd-wbtn" aria-label={tr("desktop.dismissHint")} onClick={() => setActiveHint(null)}>
                <Icon name="close" size={14} />
              </button>
            </div>
          ) : null}
          {overlay}
          <div ref={setOverlayLayer} className="nd-overlay-layer" data-overlay-layer="" />
          {/* Folha de atalhos e Personalizar são do desktop (no celular, a Aparência fica nas Configurações). */}
          {sheetOpen && !mobileOn ? <ShortcutSheet onClose={closeSheet} /> : null}
          {personalizeOpen && !mobileOn ? (
            <Personalize appearance={appearance} theme={theme} onChange={changeAppearance} onClose={closePersonalize} />
          ) : null}
          <div role="status" aria-live="polite" aria-atomic="true" className="nd-sr-only" data-desktop-status="">
            {announcement}
          </div>
        </div>
      </WindowHostContext.Provider>
    </WindowLayoutContext.Provider>
  );
  // A camada de sobreposição vale para a barra superior, as janelas e o `overlay` do shell.
  return (
    <AppearanceContext.Provider value={appearanceApi}>
      <DesktopLayerContext.Provider value={overlayLayer}>{tree}</DesktopLayerContext.Provider>
    </AppearanceContext.Provider>
  );
}
