/*
 * Janela App (`DEFAULT_APPS.app.Content`, em `memo`): SÓ o navegador do app do projeto (spec 095,
 * T03, decisões 6 e 10). O controle da execução e o console ficam na janela Execução
 * (`apps/execution`); as duas leem a MESMA sessão (`runSession.ts`, fonte única por projeto e
 * especificação).
 * - ferramentas na barra de título (slot `<WindowTools>`): voltar, recarregar, endereço
 *   centralizado (sem serifa), menu de tamanho e "Abrir em nova aba". Na janela estreita
 *   (celular), o endereço e um menu com o resto;
 * - conteúdo: o iframe ou um estado vazio (C-10: parado, subindo, sem especificação concluída,
 *   bloqueado pela prontidão, outra especificação no ar, encerrado, embed bloqueado e porta sem
 *   endereço), cada um com título, uma frase, uma ação primária e "Abrir Execução". Sem barra de
 *   ação acima do conteúdo (decisão 10): "Iniciar o app" fica no centro do estado vazio; ao
 *   clicar, abre a Execução (com a confirmação de sempre) e o App passa a mostrar "subindo" até
 *   a página aparecer. O detalhe técnico fica só na Execução;
 * - o iframe só embute pelas guardas de `embedDecision`. Com o veredito `embeddable` indeterminado
 *   (o app ainda não respondeu), a sessão relê o estado até o app responder, e então o iframe
 *   recarrega uma vez;
 * - porta talvez não liberada na rede (095/T09): com a página do Nants aberta fora do loopback e a
 *   página do app no iframe, uma linha discreta abaixo do palco ("Página em branco neste
 *   aparelho?", genérica, com a faixa de portas de visualização da 097), Recarregar e "Já
 *   liberei" (grava a faixa no estado da interface global e silencia a ajuda enquanto a faixa for
 *   a mesma). Não bloqueia nem atrasa o iframe (o navegador não consegue sondar a porta; ver
 *   `appModel.ts`);
 * - tamanhos Livre, Desktop 1440, Tablet 768 e Celular 390 (escala por `transform` para caber; o
 *   escolhido fica em `selection.appViewport`);
 * - ciclo: o iframe nunca é remontado ao focar, mover, maximizar ou minimizar a janela (o `src`
 *   não muda); durante os gestos de janela fica com `pointer-events: none`. Montada já invisível
 *   (minimizada), a janela não faz nem a consulta inicial do estado: só quando fica visível.
 * - spec 130 (mockup aprovado na T01): a forma segue o que o programa faz. Página: o navegador
 *   acima. Rodando sem página: o registro ao vivo, a porta e o Parar. Terminou: a saída, o código e
 *   os arquivos criados ou mudados. O ambiente não subiu: o fim do registro e "Pedir ao Arquiteto
 *   para corrigir". Não roda no sandbox: a explicação, o "Como usar" e as provas. O Console do
 *   projeto (padrão para qualquer projeto) abre pelo "Abrir o Console" e pelo próximo passo
 *   `open_console` do servidor (`RunViews.tsx`, `ConsolePanel.tsx`).
 * - spec 131 T05: sem `run_command` (ou com a decisão `nothing_to_run`), o App abre no Console, com
 *   o "Como usar" e as provas, sem "Iniciar" nem o erro.
 * Todo texto do servidor é renderizado como texto React.
 */
import "./app.css";
import "./run.css";
import { memo, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { appExitedText } from "../../copy";
import type { LanRange } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { rangeReleased } from "../../desktop/network";
import { useNetworkView } from "../../desktop/useNetwork";
import { PopupMenu } from "../../desktop/PopupMenu";
import { useShell, useSpace } from "../../desktop/shellContext";
import type { AppViewport, UiSelection } from "../../desktop/uiState";
import type { ResizeObserverFactory } from "../../desktop/useContainerSize";
import { useGestureActive } from "../../desktop/useWindowGesture";
import { WindowTools } from "../../desktop/Window";
import { useWindowHost } from "../../desktop/windowHost";
import { useWindowVisibility } from "../../desktop/useWindowVisibility";
import { useT } from "../../i18n/react";
import type { AppContentProps } from "../registry";
import { gateBlocked, readinessItemLabel } from "../shared/readiness";
import "../shared/apps.css";
import {
  APP_EMPTY_TITLES,
  APP_VIEWPORTS,
  EMBED_BLOCK_MESSAGES,
  EMBED_RETRY_LIMIT,
  IFRAME_SANDBOX,
  appEmptyKind,
  embedDecision,
  embedNote,
  embedProbePending,
  shownPort,
  showAppLiveToast,
  frameGeometry,
  frameStyle,
  isLoopbackHost,
  portEmbeddable,
  reachHintText,
  reachRangeHint,
  reachUnverifiedNote,
  reloadOnEmbedVerdict,
  safeHref,
  urlPort,
  type AppEmptyKind,
} from "./appModel";
import { canStart, consoleOnly, reachEnvironment, runProblemText, useRunSession } from "./runSession";
import { ConsolePanel } from "./ConsolePanel";
import { FailureView, FinishedView, RunningView, SandboxView } from "./RunViews";
import { guideRunDeclared, guideRunsHere, useAppGuide } from "./useAppGuide";
import { finishedRun, liveMode } from "./appModel";

const defaultObserver: ResizeObserverFactory | null =
  typeof ResizeObserver === "function" ? (callback) => new ResizeObserver((entries) => callback(entries)) : null;

/** Largura do elemento (observador injetável pelo `WindowHost`; jsdom mede 0). */
function useElementWidth(ref: { current: HTMLElement | null }): number {
  const host = useWindowHost();
  const factory = host.createObserver === undefined ? defaultObserver : host.createObserver;
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const element = ref.current;
    if (element === null) return undefined;
    const measured = element.getBoundingClientRect().width;
    if (measured > 0) setWidth(measured);
    if (factory === null) return undefined;
    const observer = factory((entries) => {
      const last = entries[entries.length - 1];
      if (last !== undefined) setWidth(last.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, factory]);
  return width;
}

// --- Ferramentas da barra de título ---

interface ToolbarProps {
  url: string | null;
  href: string | null;
  canNavigate: boolean;
  viewport: AppViewport;
  onViewport: (viewport: AppViewport) => void;
  onBack: () => void;
  onReload: () => void;
  compact: boolean;
}

/** Menu de tamanho da tela (barra larga). */
function SizeMenu({ viewport, onViewport }: { viewport: AppViewport; onViewport: (viewport: AppViewport) => void }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const current = APP_VIEWPORTS.find((v) => v.id === viewport) ?? APP_VIEWPORTS[0];
  const close = (returnFocus: boolean): void => {
    setOpen(false);
    if (returnFocus) buttonRef.current?.focus({ preventScroll: true });
  };
  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className="nd-btn nd-btn-sm nd-app-size-btn"
        data-variant="ghost"
        aria-label={t("app.sizeLabel", { size: current?.label ?? "" })}
        aria-haspopup="menu"
        aria-expanded={open}
        data-app-size-menu=""
        onClick={() => setOpen((v) => !v)}
      >
        {current !== undefined ? <Icon name={current.icon} size={14} /> : null}
        <span className="nd-app-size-label">{current?.label}</span>
      </button>
      {open ? (
        <PopupMenu label={t("app.size")} anchor={buttonRef} onClose={close} width={220}>
          {APP_VIEWPORTS.map((option) => (
            <button
              key={option.id}
              type="button"
              role="menuitemradio"
              aria-checked={viewport === option.id}
              className="nd-menu-item"
              data-viewport={option.id}
              onClick={() => {
                close(true);
                onViewport(option.id);
              }}
            >
              <Icon name={option.icon} size={16} />
              <span className="nd-menu-text">{option.label}</span>
              {viewport === option.id ? <Icon name="check" size={14} /> : null}
            </button>
          ))}
        </PopupMenu>
      ) : null}
    </>
  );
}

/** Menu do navegador na barra compacta: Voltar, Recarregar, Abrir em nova aba e os tamanhos. */
function BrowserMenu({ href, canNavigate, viewport, onViewport, onBack, onReload }: Omit<ToolbarProps, "url" | "compact">) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const close = (returnFocus: boolean): void => {
    setOpen(false);
    if (returnFocus) buttonRef.current?.focus({ preventScroll: true });
  };
  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className="nd-btn nd-iconbtn nd-btn-sm"
        data-variant="ghost"
        aria-label={t("app.browserActions")}
        aria-haspopup="menu"
        aria-expanded={open}
        data-app-menu=""
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="more" size={16} />
      </button>
      {open ? (
        <PopupMenu label={t("app.browser")} anchor={buttonRef} onClose={close} width={240}>
          <button
            type="button"
            role="menuitem"
            className="nd-menu-item"
            disabled={!canNavigate}
            onClick={() => {
              close(true);
              onBack();
            }}
          >
            <Icon name="arrow-left" size={16} />
            <span className="nd-menu-text">{t("common.back")}</span>
          </button>
          <button
            type="button"
            role="menuitem"
            className="nd-menu-item"
            disabled={!canNavigate}
            onClick={() => {
              close(true);
              onReload();
            }}
          >
            <Icon name="refresh" size={16} />
            <span className="nd-menu-text">{t("app.reload")}</span>
          </button>
          {href !== null ? (
            <a role="menuitem" className="nd-menu-item" href={href} target="_blank" rel="noopener noreferrer" data-app-open="" onClick={() => close(false)}>
              <Icon name="external" size={16} />
              <span className="nd-menu-text">{t("app.openNewTab")}</span>
            </a>
          ) : null}
          <div className="nd-menu-sep" role="separator" />
          <div className="nd-menu-label">{t("app.size")}</div>
          {APP_VIEWPORTS.map((option) => (
            <button
              key={option.id}
              type="button"
              role="menuitemradio"
              aria-checked={viewport === option.id}
              className="nd-menu-item"
              data-viewport={option.id}
              onClick={() => {
                close(true);
                onViewport(option.id);
              }}
            >
              <Icon name={option.icon} size={16} />
              <span className="nd-menu-text">{option.label}</span>
              {viewport === option.id ? <Icon name="check" size={14} /> : null}
            </button>
          ))}
        </PopupMenu>
      ) : null}
    </>
  );
}

/** Controles do navegador (no slot da barra de título; fora de uma janela, no lugar). */
function AppToolbar({ url, href, canNavigate, viewport, onViewport, onBack, onReload, compact }: ToolbarProps) {
  const { t } = useT();
  const address = (
    <input
      className="nd-input nd-app-address"
      readOnly
      value={url ?? ""}
      placeholder={t("app.addressPlaceholder")}
      aria-label={t("app.address")}
      data-app-address=""
    />
  );
  return (
    <WindowTools inlineOutsideWindow>
      <div className="nd-app-bar" role="toolbar" aria-label={t("app.browser")} data-compact={compact ? "true" : undefined}>
        {compact ? (
          <>
            {address}
            <BrowserMenu href={href} canNavigate={canNavigate} viewport={viewport} onViewport={onViewport} onBack={onBack} onReload={onReload} />
          </>
        ) : (
          <>
            <button type="button" className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" aria-label={t("common.back")} disabled={!canNavigate} onClick={onBack}>
              <Icon name="arrow-left" size={14} />
            </button>
            <button type="button" className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" aria-label={t("app.reload")} disabled={!canNavigate} onClick={onReload}>
              <Icon name="refresh" size={14} />
            </button>
            <span className="nd-app-address-wrap">{address}</span>
            <SizeMenu viewport={viewport} onViewport={onViewport} />
            {href !== null ? (
              <a className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" href={href} target="_blank" rel="noopener noreferrer" aria-label={t("app.openNewTab")} data-app-open="">
                <Icon name="external" size={14} />
              </a>
            ) : null}
          </>
        )}
      </div>
    </WindowTools>
  );
}

// --- Estados vazios (C-10) ---

interface EmptyAction {
  label: string;
  /** Rótulo acessível quando difere do texto ("Configurar: Docker daemon"). */
  ariaLabel?: string;
  icon?: "play" | "settings" | "stop" | "external";
  onClick?: () => void;
  href?: string;
  disabled?: boolean;
  danger?: boolean;
}

interface AppEmptyProps {
  kind: AppEmptyKind;
  text: string;
  /** Ação primária; sem ela, "Abrir Execução" é a primária. */
  action: EmptyAction | null;
  /** Progresso indeterminado (subindo), sem console. */
  progress?: boolean;
  /** Atributo `data-embed-blocked` (o motivo da guarda). */
  embedBlocked?: string;
  extra?: ReactNode;
  /** Ação secundária ao lado das outras (spec 130: "Abrir o Console"). */
  secondary?: ReactNode;
}

/** Estado vazio do App: ícone, título 17/600, uma frase, a ação primária e "Abrir Execução". */
function AppEmpty({ kind, text, action, progress = false, embedBlocked, extra, secondary }: AppEmptyProps) {
  const shell = useShell();
  const { t } = useT();
  const textId = useId();
  const titleId = useId();
  const openExecution = (
    <button
      type="button"
      className="nd-btn"
      data-variant={action === null ? "primary" : undefined}
      data-open-execution=""
      onClick={() => shell.openWindow("execution")}
    >
      <Icon name="execution" size={14} />
      {t("app.openRun")}
    </button>
  );
  let primary: ReactNode = null;
  if (action !== null && action.href !== undefined) {
    primary = (
      <a className="nd-btn" data-variant="primary" href={action.href} target="_blank" rel="noopener noreferrer" data-app-empty-action="">
        {action.icon !== undefined ? <Icon name={action.icon} size={14} /> : null}
        {action.label}
      </a>
    );
  } else if (action !== null) {
    primary = (
      <button
        type="button"
        className="nd-btn"
        data-variant={action.danger === true ? "danger" : "primary"}
        aria-label={action.ariaLabel}
        aria-describedby={action.disabled === true ? textId : undefined}
        disabled={action.disabled === true}
        data-app-empty-action=""
        onClick={action.onClick}
      >
        {action.icon !== undefined ? <Icon name={action.icon} size={14} /> : null}
        {action.label}
      </button>
    );
  }
  return (
    <section className="nd-app-empty" aria-labelledby={titleId} data-app-empty={kind} data-embed-blocked={embedBlocked}>
      <span className="nd-app-empty-icon" aria-hidden="true">
        <Icon name={kind === "embed_blocked" ? "globe" : "app"} size={20} />
      </span>
      <h2 id={titleId} className="nd-app-empty-title">
        {APP_EMPTY_TITLES[kind]}
      </h2>
      <p id={textId} className="nd-app-empty-text">
        {text}
      </p>
      {progress ? <div className="nd-app-progress" role="progressbar" aria-label={t("app.progress")} data-app-progress="" /> : null}
      {extra}
      <div className="nd-app-empty-actions">
        {primary}
        {openExecution}
        {secondary}
      </div>
    </section>
  );
}

interface ReachHintProps {
  port: string;
  /** Faixa de portas de visualização em uso (spec 097); `null` = servidor antigo (ajuda pela porta). */
  range: LanRange | null;
  onReload: () => void;
  /** "Já liberei": marca a faixa como liberada (só com a faixa). */
  onReleased: () => void;
}

/**
 * Ajuda discreta abaixo do palco (095/T09; faixa na 097): "Página em branco neste aparelho?",
 * Recarregar e, com a faixa, "Já liberei". Genérica de propósito: nada de comandos nem detalhes de
 * sistema aqui (eles ficam em Configurações, só no WSL); e nunca afirma que a porta está
 * inalcançável.
 */
function ReachHint({ port, range, onReload, onReleased }: ReachHintProps) {
  const { t } = useT();
  return (
    <div className="nd-app-reach-hint" data-reach-hint="">
      <span>
        {range !== null ? `${reachRangeHint(range)} ${reachUnverifiedNote()}` : reachHintText(port)}
      </span>
      <div className="nd-app-reach-actions">
        <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" data-reach-reload="" onClick={onReload}>
          <Icon name="refresh" size={14} />
          {t("app.reload")}
        </button>
        {range !== null ? (
          <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" data-reach-released="" onClick={onReleased}>
            <Icon name="check" size={14} />
            {t("app.reach.released")}
          </button>
        ) : null}
      </div>
    </div>
  );
}

// --- Navegador ---

interface BrowserProps {
  /** Endereço da porta mostrada; `null` = sem página (mostra `empty`). */
  url: string | null;
  embeddable: boolean | null;
  viewport: AppViewport;
  onViewport: (viewport: AppViewport) => void;
  reloadKey: number;
  onReload: () => void;
  /** Janela estreita (celular): endereço e um menu com o resto. */
  compact?: boolean;
  /** O servidor ainda confere se o app responde (`embeddable === null`, releitura em curso). */
  probing?: boolean;
  /** Estado vazio sem página. */
  empty?: ReactNode;
  /** Abaixo do palco, só com a página no iframe (a ajuda da porta na rede, 095/T09). */
  hint?: ReactNode;
}

/**
 * Ferramentas (na barra de título) e o palco: o iframe, a explicação do embed bloqueado ou o
 * estado vazio. O iframe é posicionado de forma absoluta no palco (altura sempre resolvida pelo
 * palco, sem depender da cadeia de alturas percentuais no Safari do iPhone).
 */
export function AppBrowser({
  url,
  embeddable,
  viewport,
  onViewport,
  reloadKey,
  onReload,
  compact = false,
  probing = false,
  empty = null,
  hint = null,
}: BrowserProps) {
  const { t } = useT();
  const decision = url === null ? null : embedDecision(url, embeddable, { hostname: window.location.hostname, origin: window.location.origin });
  const href = url === null ? null : safeHref(url);
  const gesture = useGestureActive();
  const stageRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const areaWidth = useElementWidth(stageRef);
  const geometry = frameGeometry(viewport, areaWidth);
  const style = frameStyle(geometry);
  const [homeKey, setHomeKey] = useState(0);
  const embedding = decision !== null && decision.embed;
  // Voltar: o histórico do iframe só é acessível na mesma origem; o app tem outra (outra porta),
  // então volta à página inicial do app.
  const back = (): void => {
    try {
      const history = frameRef.current?.contentWindow?.history;
      if (history !== undefined) {
        history.back();
        return;
      }
    } catch {
      // Outra origem: o histórico não é acessível.
    }
    setHomeKey((n) => n + 1);
  };
  return (
    <div className="nd-app-browser" data-app-browser="">
      <AppToolbar
        url={url}
        href={href}
        canNavigate={embedding}
        viewport={viewport}
        onViewport={onViewport}
        onBack={back}
        onReload={onReload}
        compact={compact}
      />
      {decision !== null && decision.embed && !decision.confirmed ? (
        <div role="note" className="nd-strip nd-app-note" data-tone="amber" data-embed-note={probing ? "probing" : "unconfirmed"}>
          <span>{embedNote(probing)}</span>
          {/* Na barra larga o link já está na barra: aqui só na compacta (que o guarda no menu). */}
          {!probing && compact && href !== null ? (
            <a className="nd-btn nd-btn-sm nd-app-open-inline" href={href} target="_blank" rel="noopener noreferrer" data-app-open-inline="">
              <Icon name="external" size={14} />
              {t("app.openNewTab")}
            </a>
          ) : null}
        </div>
      ) : null}
      <div ref={stageRef} className="nd-app-stage" data-app-stage="" data-viewport={viewport} data-scale={geometry.scale}>
        {url !== null && decision !== null && decision.embed ? (
          <iframe
            ref={frameRef}
            key={`${reloadKey}:${homeKey}`}
            className="nd-app-frame"
            data-app-frame=""
            title={t("app.frameTitle")}
            src={url}
            sandbox={IFRAME_SANDBOX}
            style={{ ...style, pointerEvents: gesture ? "none" : undefined }}
          />
        ) : decision !== null && !decision.embed ? (
          <AppEmpty
            kind="embed_blocked"
            text={EMBED_BLOCK_MESSAGES[decision.reason]}
            embedBlocked={decision.reason}
            action={href !== null ? { label: t("app.openNewTab"), icon: "external", href } : null}
          />
        ) : (
          empty
        )}
      </div>
      {embedding ? hint : null}
    </div>
  );
}

interface RunnerProps {
  projectId: string;
  /** Especificação concluída do espaço (a que "Iniciar" envia); `null` = nenhuma ainda. */
  specId: string | null;
  size: AppContentProps["size"];
  viewport: AppViewport;
  setSelection: (patch: Partial<UiSelection>) => void;
}

export function AppRunner({ projectId, specId, size, viewport, setSelection }: RunnerProps) {
  const shell = useShell();
  const { t } = useT();
  const visible = useWindowVisibility("app");
  // O registro ao vivo só é lido quando o App o mostra (rodando sem página, spec 130).
  const [wantLogs, setWantLogs] = useState(false);
  const run = useRunSession(projectId, specId, {
    visible,
    logs: wantLogs,
    onLive: () => showAppLiveToast(shell.showToast),
  });
  const { snap, actions } = run;
  const { phase } = snap;
  const mode = liveMode(phase);
  useEffect(() => setWantLogs(mode === "running" && !snap.consoleOpen), [mode, snap.consoleOpen]);
  const guide = useAppGuide(projectId, specId, visible);
  const network = useNetworkView(snap.network);
  const [reloadKey, setReloadKey] = useState(0);
  const port = shownPort(phase, snap.shownUrl);
  const shownEmbeddable = port === null ? undefined : portEmbeddable(phase, port);
  const probing = embedProbePending(phase) && snap.embedRetries < EMBED_RETRY_LIMIT;

  // O app passou a responder depois de o iframe abrir sem resposta: recarrega uma vez.
  const lastEmbeddable = useRef(shownEmbeddable);
  useEffect(() => {
    if (reloadOnEmbedVerdict(lastEmbeddable.current, shownEmbeddable)) setReloadKey((n) => n + 1);
    lastEmbeddable.current = shownEmbeddable;
  }, [shownEmbeddable]);

  const reload = (): void => {
    setReloadKey((n) => n + 1);
    actions.reload();
  };

  // "Iniciar o app": abre a Execução (com a confirmação de sempre); o App passa a "subindo".
  const requestStart = (): void => {
    actions.requestStart();
    shell.openWindow("execution");
  };

  const blocked = gateBlocked(shell.readiness, "execute");
  const missing = shell.readiness?.execute.missing ?? [];
  const decision = port === null ? null : embedDecision(port.url, portEmbeddable(phase, port), { hostname: window.location.hostname, origin: window.location.origin });

  const openConsoleButton =
    specId !== null ? (
      <button type="button" className="nd-btn" data-open-console="" onClick={actions.openConsole}>
        <Icon name="terminal" size={14} />
        {t("app.openConsole")}
      </button>
    ) : null;
  const kind = appEmptyKind({ phase, confirming: snap.confirming, specId, blocked, embed: decision });

  let empty: ReactNode = null;
  if (kind === "stopped") {
    empty = (
      <AppEmpty
        kind="stopped"
        text={snap.problem === null ? t("app.text.stopped") : runProblemText(snap.problem)}
        action={{ label: t("app.start"), icon: "play", onClick: requestStart }}
        secondary={openConsoleButton}
      />
    );
  } else if (kind === "starting") {
    empty = <AppEmpty kind="starting" text={t(snap.confirming ? "app.text.confirming" : "app.text.starting")} action={null} progress />;
  } else if (kind === "no_spec") {
    empty = <AppEmpty kind="no_spec" text={t("app.waiting")} action={{ label: t("app.start"), icon: "play", disabled: true }} />;
  } else if (kind === "blocked") {
    const first = missing[0];
    const labels = missing.map(readinessItemLabel).join(", ");
    empty = (
      <AppEmpty
        kind="blocked"
        text={t("app.blockedText", { items: labels })}
        action={
          first === undefined
            ? null
            : {
                label: t("readiness.configure"),
                ariaLabel: t("readiness.configureItem", { label: readinessItemLabel(first) }),
                icon: "settings",
                onClick: () => shell.openSettings(first.settings_tab),
              }
        }
      />
    );
  } else if (kind === "other") {
    empty = (
      <AppEmpty
        kind="other"
        text={t("app.text.other")}
        action={{ label: t(snap.stopping ? "run.stopping" : "run.stop"), icon: "stop", danger: true, disabled: snap.stopping, onClick: actions.stop }}
      />
    );
  } else if (kind === "exited" && phase.kind === "exited") {
    empty = (
      <AppEmpty
        kind="exited"
        text={appExitedText(phase.code, phase.reason)}
        action={{ label: t("app.runAgain"), icon: "play", onClick: requestStart, disabled: blocked || specId === null }}
        secondary={openConsoleButton}
      />
    );
  } else if (kind === "no_address") {
    empty = <AppEmpty kind="no_address" text={t("app.text.noAddress")} action={null} />;
  }

  // Página do Nants aberta pela rede (fora do loopback): ajuda discreta sobre a faixa de portas de
  // visualização (095/T09, 097). Some quando a faixa em uso foi marcada como liberada.
  const remote = !isLoopbackHost(reachEnvironment.pageHostname());
  const range = network?.lan_view_ports ?? null;
  const released = rangeReleased(shell.lanReleased, range);
  const hintPort = port === null ? null : (urlPort(port.url) ?? String(port.published));
  const hint =
    remote && hintPort !== null && !released ? (
      <ReachHint port={hintPort} range={range} onReload={reload} onReleased={() => shell.setLanReleased(range)} />
    ) : null;

  // Spec 130: as formas além da página, pelo comportamento do programa.
  const startable = specId !== null && !blocked && canStart(snap);
  const finished = finishedRun(phase);
  const failure =
    snap.failure !== null && snap.failure.code !== "app_exec_docker_failed" && (specId === null || snap.failure.spec_id === specId) ? snap.failure : null;
  const idle = (phase.kind === "stopped" || (phase.kind === "exited" && finished === null)) && !snap.confirming;
  const sandbox = guide?.status === "ok" && !guide.guide.runsHere;
  // Spec 131 T05: sem `run_command`, o App abre no Console, sem "Iniciar" nem o erro. O que não roda
  // no sandbox (spec 130) tem precedência: a explicação, que já oferece "Abrir o Console".
  const noRun = specId !== null && guideRunsHere(guide) && consoleOnly(snap, guideRunDeclared(guide));
  const inConsole = (snap.consoleOpen || noRun) && specId !== null;
  let view: ReactNode = null;
  if (inConsole && noRun) {
    view = (
      <ConsolePanel
        projectId={projectId}
        specId={specId}
        compact={size === "narrow"}
        visible={visible}
        guide={guide}
        run={run}
        headAction={null}
        noRun={{ decision: guide?.status === "ok" ? guide.guide.nothingToRun : null }}
      />
    );
  } else if (inConsole) {
    view = (
      <ConsolePanel
        projectId={projectId}
        specId={specId}
        compact={size === "narrow"}
        visible={visible}
        guide={guide}
        run={run}
        headAction={
          <>
            {phase.kind === "stopped" && startable ? (
              <button type="button" className="nd-btn" data-console-start="" onClick={requestStart}>
                <Icon name="play" size={14} />
                {t("app.start")}
              </button>
            ) : null}
            <button type="button" className="nd-btn" data-variant="ghost" data-console-back="" onClick={actions.closeConsole}>
              <Icon name="arrow-left" size={14} />
              {t(phase.kind === "running" ? "console.backToApp" : "console.back")}
            </button>
          </>
        }
      />
    );
  } else if (phase.kind === "running" && mode === "running") {
    view = <RunningView phase={phase} logs={snap.logs} stopping={snap.stopping} onStop={actions.stop} />;
  } else if (phase.kind === "exited" && finished !== null) {
    view = <FinishedView projectId={projectId} phase={phase} canRun={startable} onRunAgain={requestStart} onOpenConsole={actions.openConsole} />;
  } else if (failure !== null && idle) {
    view = <FailureView failure={failure} run={run} canRetry={startable} onRetry={requestStart} />;
  } else if (specId !== null && guide?.status === "loading" && phase.kind === "stopped" && !snap.confirming) {
    // Spec 131 T05: enquanto o guia não responde, nem "Iniciar" nem o Console (sem piscar); se ele
    // falhar, vale o estado parado de sempre.
    view = (
      <section className="nd-runview" data-runview="loading" aria-busy="true">
        <p className="nd-runview-muted" role="status">
          {t("runview.loadingGuide")}
        </p>
      </section>
    );
  } else if (sandbox && phase.kind === "stopped" && !snap.confirming) {
    view = <SandboxView projectId={projectId} specId={specId} guide={guide} onOpenConsole={actions.openConsole} />;
  }
  return (
    <div className="nd-app nd-app-scroll" data-app="" data-size={size} data-app-view={view === null ? undefined : inConsole ? "console" : "run"}>
      {view ?? (
        <AppBrowser
          url={port?.url ?? null}
          embeddable={port === null ? null : portEmbeddable(phase, port)}
          viewport={viewport}
          onViewport={(next) => setSelection({ appViewport: next })}
          reloadKey={reloadKey}
          onReload={reload}
          compact={size === "narrow"}
          probing={probing}
          empty={empty}
          hint={hint}
        />
      )}
    </div>
  );
}

function AppContentView({ size }: AppContentProps) {
  const space = useSpace();
  if (space === null) return null;
  const { data, selection } = space;
  const spec = data.activeSpec;
  const specId = spec !== null && spec.status === "COMPLETED" ? spec.id : null;
  return (
    <AppRunner
      key={`${data.projectId}:${specId ?? ""}`}
      projectId={data.projectId}
      specId={specId}
      size={size}
      viewport={selection.appViewport ?? "free"}
      setSelection={space.setSelection}
    />
  );
}

export const AppContent = memo(AppContentView);
