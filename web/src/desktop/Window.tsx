/*
 * Moldura de janela do desktop (spec 093, decisões 3 e 4):
 * - `role="region"` com `aria-labelledby` no título e `aria-describedby` no subtítulo vivo;
 * - barra de título (`[data-window-titlebar]`, focável) com ícone, título, subtítulo e os botões
 *   menu, minimizar, maximizar/restaurar e fechar (ícone + `aria-label`); arrasta para mover,
 *   duplo clique maximiza/restaura; Shift+F10/tecla Menu abrem o menu da janela;
 * - alças de redimensionar na borda direita, na inferior e no canto (≥ 10 px de área útil);
 * - conteúdo (`[data-window-content]`) com `useContainerSize` (`data-size`) e estado vazio padrão;
 * - animações FLIP (`animationFor`) ao maximizar, restaurar, encaixar, minimizar até o ícone do
 *   dock e restaurar a partir dele. O estado nunca espera a animação; nenhuma depois de um gesto;
 * - minimizada = `hidden` + `inert` (durante a animação de saída, só `inert`);
 * - no modo celular (`host.mobile`, T10), tela cheia sem gestos, sem alças e sem menu: a barra de
 *   título só tem Fechar (que volta ao Alternador);
 * - barra de título de 44 px (`TITLEBAR_HEIGHT` = `--titlebar-h`, 095/T02, C-06) com o slot de
 *   ferramentas `[data-window-tools]` (C-07) entre o subtítulo e os controles: abas e controles do
 *   navegador ficam DENTRO da barra. O app põe as ferramentas por `tools` ou por `<WindowTools>`
 *   (portal no slot). O arrasto não começa no slot e os atalhos da barra não valem lá dentro; no
 *   celular o slot é uma linha logo abaixo da barra, rolável na horizontal.
 */
import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FocusEvent as ReactFocusEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useT } from "../i18n/react";
import { ANIMATION_MS, animationFor, playAnimation, waitForAnimations } from "./animation";
import { EmptyState, type EmptyAction } from "./EmptyState";
import { dockButtonId, windowHiddenAttrs, windowTitleBarId, windowTitleId } from "./focus";
import { Icon } from "./Icon";
import { TITLEBAR_HEIGHT } from "./metrics";
import { matchShortcut, shortcutById, shortcutContext } from "./shortcuts";
import { useContainerSize, type ContainerSize } from "./useContainerSize";
import { useWindowGesture, type GestureCommit } from "./useWindowGesture";
import { useWindowHost } from "./windowHost";
import { WindowMenu } from "./WindowMenu";
import {
  keyboardGestureStep,
  menuCommandAction,
  startKeyboardGesture,
  type KeyboardGesture,
  type WindowMenuItem,
} from "./windowMenu";
import { WINDOW_META, windowSubtitleId } from "./windowMeta";
import { effectiveRect, type Rect, type WindowKind, type WindowMode, type WindowState, type Workspace } from "./windows";

/** Área útil das alças de redimensionar (px): borda e canto. */
export const RESIZE_EDGE_PX = 12;
export const RESIZE_CORNER_PX = 18;
/** Altura da barra de título (px): a mesma fonte do `--titlebar-h` do CSS (`metrics.ts`). */
const TITLE_BAR_PX = TITLEBAR_HEIGHT;
/** Largura do menu da janela (px). */
const MENU_WIDTH = 268;
/** Altura que o menu tenta ter abaixo da barra (px). */
const MENU_HEIGHT = 560;

/** Quem rola o conteúdo: o app (área própria, `nd-app-scroll`) ou a janela. */
export type WindowScroll = "app" | "window";

export interface WindowEmpty {
  message: string;
  action?: EmptyAction;
}

export interface WindowProps {
  kind: WindowKind;
  win: WindowState;
  workspace: Workspace;
  /** Janela em foco (a visível mais à frente). */
  active: boolean;
  /** Subtítulo de estado vivo (ex.: "4 tarefas · 2 concluídas"). */
  subtitle?: string;
  /** Estado vazio (padrão: o do tipo de janela, sem ação). */
  empty?: WindowEmpty;
  /** Conteúdo; vazio (`null`/`undefined`/`false`) mostra o estado vazio. */
  children?: ReactNode | ((size: ContainerSize) => ReactNode);
  /** `"app"`: o conteúdo da janela não rola (o app tem a própria rolagem). Padrão: `"window"`. */
  scroll?: WindowScroll;
  /** Ferramentas na barra de título (abas, controles do navegador): o slot `[data-window-tools]`. */
  tools?: ReactNode;
}

/**
 * O slot de ferramentas da janela em volta (para `<WindowTools>`): `null` fora de uma janela;
 * `{ slot: null }` dentro de uma janela cujo slot ainda não montou.
 */
const WindowToolsSlot = createContext<{ slot: HTMLElement | null } | null>(null);

/**
 * Põe as ferramentas de um app no slot da barra de título da janela que o contém (C-07). Dentro
 * de uma janela, antes do slot montar, não desenha nada. Fora de uma janela, não desenha nada, ou
 * desenha no lugar com `inlineOutsideWindow` (o conteúdo montado sozinho, como nos testes).
 */
export function WindowTools({ children, inlineOutsideWindow = false }: { children: ReactNode; inlineOutsideWindow?: boolean }) {
  const context = useContext(WindowToolsSlot);
  if (context === null) return inlineOutsideWindow ? <>{children}</> : null;
  return context.slot === null ? null : createPortal(children, context.slot);
}

const menuShortcut = shortcutById("windowMenu");

function rectOf(element: Element | null): Rect | null {
  if (element === null) return null;
  const r = element.getBoundingClientRect();
  return { x: r.left, y: r.top, w: r.width, h: r.height };
}

const canAnimate = (): boolean => typeof HTMLElement !== "undefined" && typeof HTMLElement.prototype.animate === "function";

function WindowView({ kind, win, workspace, active, subtitle, empty, children, scroll = "window", tools }: WindowProps) {
  const host = useWindowHost();
  // Spec 101: título, controles e anúncios no idioma ativo (a moldura re-renderiza na troca).
  const { t } = useT();
  const meta = WINDOW_META[kind];
  const rootRef = useRef<HTMLElement | null>(null);
  const titleBarRef = useRef<HTMLDivElement | null>(null);
  const menuButtonRef = useRef<HTMLButtonElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const [toolsSlot, setToolsSlot] = useState<HTMLDivElement | null>(null);
  const toolsContext = useMemo(() => ({ slot: toolsSlot }), [toolsSlot]);
  const size = useContainerSize(contentRef, host.createObserver === undefined ? {} : { createObserver: host.createObserver });
  const rect = effectiveRect(win, workspace);
  const maximized = win.mode === "maximized";
  const mobile = host.mobile === true;

  // --- Animação de saída (minimizar): a janela continua desenhada (inerte) até o fim. ---
  const [exiting, setExiting] = useState(false);
  const [seenMode, setSeenMode] = useState<WindowMode>(win.mode);
  if (seenMode !== win.mode) {
    setSeenMode(win.mode);
    setExiting(win.mode === "minimized" && seenMode !== "minimized" && canAnimate() && !host.reducedMotion());
  }

  /** Geometria visível antes de um commit de gesto (a animação parte dela, não do estado antigo). */
  const visualFrom = useRef<Rect | null>(null);
  /** O próximo commit não anima (a janela já está no lugar: fim de gesto ou modo teclado). */
  const skipAnimation = useRef(false);
  const previous = useRef<{ rect: Rect; mode: WindowMode; ws: Workspace } | null>(null);

  useLayoutEffect(() => {
    const before = previous.current;
    previous.current = { rect, mode: win.mode, ws: workspace };
    const element = rootRef.current;
    if (before === null || element === null) return;
    const reduced = host.reducedMotion();
    const dock = rectOf(document.getElementById(dockButtonId(kind)));
    if (win.mode === "minimized" && before.mode !== "minimized") {
      const from = rectOf(element);
      const animation = from !== null && dock !== null ? animationFor(from, dock, reduced, "minimize") : null;
      playAnimation(element, animation);
      let done = false;
      const finish = (): void => {
        if (done) return;
        done = true;
        setExiting(false);
      };
      void waitForAnimations(element).then(finish);
      // Teto: o estado já mudou; a moldura nunca fica presa esperando a animação.
      const timer = setTimeout(finish, ANIMATION_MS.minimize + 120);
      return () => clearTimeout(timer);
    }
    if (before.mode === "minimized" && win.mode !== "minimized") {
      const to = rectOf(element);
      playAnimation(element, dock !== null && to !== null ? animationFor(dock, to, reduced, "restore") : null);
      return undefined;
    }
    // Mudou a área de trabalho (resize do navegador): a janela só acompanha, sem animar.
    const workspaceChanged = before.ws.w !== workspace.w || before.ws.h !== workspace.h;
    if (skipAnimation.current || workspaceChanged) {
      skipAnimation.current = false;
      visualFrom.current = null;
      return undefined;
    }
    const from = visualFrom.current ?? before.rect;
    visualFrom.current = null;
    playAnimation(element, animationFor(from, rect, reduced, "geometry"));
    return undefined;
  }, [rect.x, rect.y, rect.w, rect.h, win.mode, workspace.w, workspace.h]);

  // --- Gestos (mover pela barra, redimensionar pelas alças) ---
  const onCommit = useCallback(
    ({ rect: next, snap }: GestureCommit) => {
      visualFrom.current = next;
      if (snap !== null) host.act({ type: "snap", kind, target: snap });
      else {
        skipAnimation.current = true;
        host.act({ type: "setRect", kind, rect: next });
      }
    },
    [host, kind],
  );
  const gesture = useWindowGesture({
    windowRef: rootRef,
    rect,
    workspace,
    workspaceOrigin: host.workspaceOrigin,
    enabled: win.mode === "normal" && !mobile,
    onCommit,
    onStart: (mode) => host.gestureStart(kind, mode),
    ...(host.scheduler !== undefined ? { scheduler: host.scheduler } : {}),
  });
  useEffect(() => {
    host.snapPreview(kind, gesture.snapPreview);
  }, [host, kind, gesture.snapPreview]);

  /**
   * Devolve a geometria do estado ao estilo inline. A prévia do gesto (e do modo teclado) limpa
   * `transform`/`width`/`height`; se o valor do estado não mudou (cancelar, ou mover sem
   * redimensionar), o React não o escreveria de novo e a janela ficaria sem largura/altura.
   */
  const rectRef = useRef(rect);
  rectRef.current = rect;
  const restoreGeometry = useCallback(() => {
    const element = rootRef.current;
    if (element === null) return;
    const r = rectRef.current;
    element.style.transform = "";
    element.style.left = `${r.x}px`;
    element.style.top = `${r.y}px`;
    element.style.width = `${r.w}px`;
    element.style.height = `${r.h}px`;
  }, []);
  useLayoutEffect(() => {
    if (!gesture.dragging) restoreGeometry();
  }, [gesture.dragging, restoreGeometry]);

  // --- Menu da janela ---
  const [menu, setMenu] = useState<{ returnTo: "button" | "titleBar" } | null>(null);
  const menuId = `window-${kind}-menu`;
  const closeMenu = useCallback(
    (returnFocus: boolean) => {
      setMenu((current) => {
        if (current !== null && returnFocus) {
          const target = current.returnTo === "button" ? menuButtonRef.current : titleBarRef.current;
          queueMicrotask(() => target?.focus({ preventScroll: true }));
        }
        return null;
      });
    },
    [],
  );

  // --- Mover/redimensionar pelo teclado ---
  const keyboard = useRef<KeyboardGesture | null>(null);
  const clearInline = restoreGeometry;
  const applyKeyboardPreview = (g: KeyboardGesture): void => {
    const element = rootRef.current;
    if (element === null) return;
    if (g.mode === "move") element.style.transform = `translate(${g.rect.x - g.origin.x}px, ${g.rect.y - g.origin.y}px)`;
    else {
      element.style.width = `${g.rect.w}px`;
      element.style.height = `${g.rect.h}px`;
    }
  };
  const cancelKeyboard = (announce: boolean): void => {
    if (keyboard.current === null) return;
    keyboard.current = null;
    clearInline();
    if (announce) host.announce(t("announce.moveCanceled", { title: meta.title }));
  };

  const chooseMenuItem = (item: WindowMenuItem): void => {
    setMenu(null);
    const command = item.command;
    if (command.type === "keyboardMove" || command.type === "keyboardResize") {
      const mode = command.type === "keyboardMove" ? "move" : "resize";
      keyboard.current = startKeyboardGesture(kind, mode, { x: win.x, y: win.y, w: win.w, h: win.h });
      titleBarRef.current?.focus({ preventScroll: true });
      host.announce(t(mode === "move" ? "announce.keyboardMove" : "announce.keyboardResize", { title: meta.title }));
      return;
    }
    const action = menuCommandAction(kind, command);
    if (action !== null) host.act(action);
  };

  const onTitleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    const g = keyboard.current;
    if (g !== null) {
      const result = keyboardGestureStep(g, event.key, workspace);
      if (result === null) return;
      event.preventDefault();
      event.stopPropagation();
      if (result.type === "update") {
        keyboard.current = result.gesture;
        applyKeyboardPreview(result.gesture);
      } else if (result.type === "commit") {
        keyboard.current = null;
        clearInline();
        skipAnimation.current = true;
        host.act(result.action);
        host.announce(t("announce.positionConfirmed", { title: meta.title }));
      } else cancelKeyboard(true);
      return;
    }
    const match = matchShortcut(event.nativeEvent, shortcutContext(event.target as Element));
    if (match?.id === "windowMenu" && !mobile) {
      event.preventDefault();
      setMenu({ returnTo: "titleBar" });
    }
  };

  const onTitleDoubleClick = (event: ReactMouseEvent<HTMLDivElement>): void => {
    if (mobile || (event.target instanceof Element && event.target.closest("button, [data-window-tools]") !== null)) return;
    host.act({ type: "toggleMaximize", kind });
  };

  // --- Foco: focusin traz para a frente e é lembrado para restaurar ---
  const onFocus = (event: ReactFocusEvent<HTMLElement>): void => {
    if (event.target instanceof HTMLElement) host.rememberFocus(kind, event.target);
    if (!active && win.mode !== "minimized") host.act({ type: "focus", kind });
  };
  const onBlur = (event: ReactFocusEvent<HTMLElement>): void => {
    const next = event.relatedTarget;
    if (next instanceof Node && rootRef.current?.contains(next) === true) return;
    cancelKeyboard(false);
  };
  const onPointerDownCapture = (): void => {
    if (!active && win.mode !== "minimized") host.act({ type: "focus", kind });
  };

  // --- Conteúdo ---
  const rendered = typeof children === "function" ? children(size) : children;
  const isEmpty = rendered === null || rendered === undefined || rendered === false;
  const emptyState = empty ?? { message: meta.empty.message };

  const { hidden, inert } = windowHiddenAttrs(win);
  const titleId = windowTitleId(kind);
  const subtitleId = windowSubtitleId(kind);
  const hasSubtitle = subtitle !== undefined && subtitle !== "";

  // Desktop: dentro da barra de título. Celular: uma linha de titânio logo abaixo dela (C-07).
  const toolsElement = (
    <div
      ref={setToolsSlot}
      className={mobile ? "nd-window-tools nd-chrome" : "nd-window-tools"}
      data-window-tools=""
      data-row={mobile ? "true" : undefined}
    >
      {tools}
    </div>
  );

  const menuStyle = (() => {
    if (host.menuLayer === null) return { right: 8, top: TITLE_BAR_PX };
    const left = Math.max(8, Math.min(rect.x + rect.w - MENU_WIDTH - 8, workspace.w - MENU_WIDTH - 8));
    const top = Math.max(8, Math.min(rect.y + TITLE_BAR_PX, workspace.h - MENU_HEIGHT - 8));
    return { left, top, width: MENU_WIDTH, maxHeight: Math.max(160, workspace.h - top - 8) };
  })();
  const menuElement =
    menu === null || mobile ? null : (
      <WindowMenu
        id={menuId}
        label={t("window.menuOf", { title: meta.title })}
        maximized={maximized}
        style={menuStyle}
        onChoose={chooseMenuItem}
        onClose={closeMenu}
        opener={() => menuButtonRef.current}
      />
    );

  return (
    <section
      ref={rootRef}
      role="region"
      aria-labelledby={titleId}
      aria-describedby={hasSubtitle ? subtitleId : undefined}
      className="nd-window"
      data-window={kind}
      data-mode={win.mode}
      data-active={active ? "true" : "false"}
      data-size={size}
      data-mobile={mobile ? "true" : undefined}
      hidden={hidden && !exiting}
      inert={inert}
      style={{ left: rect.x, top: rect.y, width: rect.w, height: rect.h, zIndex: win.z }}
      onFocus={onFocus}
      onBlur={onBlur}
      onPointerDownCapture={onPointerDownCapture}
    >
      <div
        ref={titleBarRef}
        id={windowTitleBarId(kind)}
        className="nd-titlebar nd-chrome"
        data-window-titlebar=""
        role="group"
        aria-labelledby={titleId}
        aria-describedby={hasSubtitle ? subtitleId : undefined}
        aria-keyshortcuts={menuShortcut?.aria}
        tabIndex={0}
        onPointerDown={gesture.onMovePointerDown}
        onDoubleClick={onTitleDoubleClick}
        onKeyDown={onTitleKeyDown}
      >
        <span className="nd-titlebar-icon" style={{ color: `var(--icon-${meta.tone})` }}>
          <Icon name={meta.icon} size={16} />
        </span>
        <span id={titleId} className="nd-title">
          {meta.title}
        </span>
        <span id={subtitleId} className="nd-subtitle">
          {hasSubtitle ? subtitle : ""}
        </span>
        {mobile ? null : toolsElement}
        {mobile ? (
          <button
            type="button"
            className="nd-wbtn"
            data-variant="close"
            aria-label={t("windowControls.close")}
            onClick={() => host.act({ type: "close", kind })}
          >
            <Icon name="close" size={18} />
          </button>
        ) : (
          <>
            <button
              ref={menuButtonRef}
              type="button"
              className="nd-wbtn"
              aria-label={t("window.menu")}
              aria-haspopup="menu"
              aria-expanded={menu !== null}
              aria-controls={menu !== null ? menuId : undefined}
              onClick={() => setMenu((current) => (current === null ? { returnTo: "button" } : null))}
            >
              <Icon name="more" size={16} />
            </button>
            <button type="button" className="nd-wbtn" aria-label={t("windowControls.minimize")} onClick={() => host.act({ type: "minimize", kind })}>
              <Icon name="minimize" size={14} />
            </button>
            <button
              type="button"
              className="nd-wbtn"
              aria-label={t(maximized ? "windowControls.restore" : "windowControls.maximize")}
              onClick={() => host.act({ type: "toggleMaximize", kind })}
            >
              <Icon name={maximized ? "restore" : "maximize"} size={14} />
            </button>
            <button
              type="button"
              className="nd-wbtn"
              data-variant="close"
              aria-label={t("windowControls.close")}
              onClick={() => host.act({ type: "close", kind })}
            >
              <Icon name="close" size={14} />
            </button>
          </>
        )}
      </div>
      {mobile ? toolsElement : null}
      <div ref={contentRef} className="nd-content" data-window-content="" data-scroll={scroll}>
        <WindowToolsSlot.Provider value={toolsContext}>
          {isEmpty ? <EmptyState icon={meta.icon} message={emptyState.message} {...(emptyState.action !== undefined ? { action: emptyState.action } : {})} /> : rendered}
        </WindowToolsSlot.Provider>
      </div>
      {win.mode === "normal" && !mobile ? (
        <>
          <div
            className="nd-handle"
            data-handle="e"
            aria-hidden="true"
            style={{ width: RESIZE_EDGE_PX, bottom: RESIZE_CORNER_PX }}
            onPointerDown={gesture.onResizePointerDown("resize-e")}
          />
          <div
            className="nd-handle"
            data-handle="s"
            aria-hidden="true"
            style={{ height: RESIZE_EDGE_PX, right: RESIZE_CORNER_PX }}
            onPointerDown={gesture.onResizePointerDown("resize-s")}
          />
          <div
            className="nd-handle"
            data-handle="se"
            aria-hidden="true"
            style={{ width: RESIZE_CORNER_PX, height: RESIZE_CORNER_PX }}
            onPointerDown={gesture.onResizePointerDown("resize-se")}
          >
            <Icon name="resize" size={14} />
          </div>
        </>
      ) : null}
      {menuElement !== null && host.menuLayer !== null ? createPortal(menuElement, host.menuLayer) : menuElement}
    </section>
  );
}

/** Moldura memoizada: o conteúdo só re-renderiza quando as props mudam. */
export const Window = memo(WindowView);
