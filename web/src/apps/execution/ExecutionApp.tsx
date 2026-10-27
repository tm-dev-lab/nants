/*
 * Janela Execução (`DEFAULT_APPS.execution.Content`, spec 095, T03, decisão 6; artboard
 * `reference/App.dc.html`): o controle da execução do app do projeto e o console. Lê a MESMA
 * sessão da janela App (`runSession.ts`, fonte única por projeto e especificação).
 * - cartão: a especificação, "docker compose up · porta N", a pílula de estado (Parada, Subindo,
 *   No ar, Saiu, outra especificação) e Iniciar ou Parar. Cada início exige confirmação humana
 *   (diálogo com o comando exato e a pasta); só "Executar" envia `{confirm: true, spec_id}`;
 * - portas publicadas como links (nova aba, `noopener noreferrer`) e avisos de porta;
 * - prontidão `execute` incompleta: Iniciar desabilitado com o que falta; 412 `prereq_missing`:
 *   a lista do que falta com a prontidão relida; outras falhas no idioma ativo (pelo `code`), com os detalhes
 *   técnicos recolhidos;
 * - console no resto da janela, com rolagem própria e "Copiar registro". A consulta do registro
 *   só acontece com esta janela visível (uma ao ficar visível e a cada 2 s com o app no ar);
 * - a dica "Quando a porta responder, a janela App mostra a página sozinha.";
 * - quando o app passa a responder (uma vez por partida), o aviso "App no ar" e, se a janela App
 *   estiver fechada, ela abre com a página.
 * - spec 130: a pílula diz a forma (no ar com página, rodando sem página, terminou com o código);
 *   "Abrir o Console" mostra o Console do projeto na janela App; com a falha do ambiente guardada no
 *   servidor, "Pedir ao Arquiteto para corrigir" leva à Conversa.
 * Todo texto do servidor é renderizado como texto React.
 */
import "./execution.css";
import { memo, useContext, useEffect, useId, useRef, useState } from "react";
import { appExitedText } from "../../copy";
import { isLoopbackHost } from "../app/appModel";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useShell, useSpace } from "../../desktop/shellContext";
import { useWindowVisibility, WindowLayoutContext } from "../../desktop/useWindowVisibility";
import { AutoScrollBox, type ScrollItem } from "../conversation/AutoScrollBox";
import { copyText } from "../conversation/ResultMessage";
import {
  DOCKER_RUN_COMMAND,
  finishedRun,
  firstPort,
  hostPort,
  liveMode,
  runStateLabel,
  safeHref,
  reachPortNote,
  reachRangeNote,
  showAppLiveToast,
  urlPort,
  type RunPhase,
} from "../app/appModel";
import { FixButton } from "../app/RunViews";
import { rangeReleased } from "../../desktop/network";
import { useNetworkView } from "../../desktop/useNetwork";
import type { AppPort } from "../../api/client";
import { noticeText } from "../../i18n/serverCodes";
import { useT } from "../../i18n/react";
import { canStart, reachEnvironment, runProblemText, useRunSession } from "../app/runSession";
import type { AppContentProps } from "../registry";
import { gateBlocked } from "../shared/readiness";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import "../shared/apps.css";

const NO_ITEMS: readonly ScrollItem[] = [];

const PHASE_TONE: Readonly<Record<RunPhase["kind"], string | undefined>> = {
  stopped: undefined,
  starting: "amber",
  running: "green",
  other: "amber",
  exited: "rose",
};

/** Tom da pílula: rodando sem página em ciano; "terminou" em verde (código 0) ou rosa. */
function phaseTone(phase: RunPhase): string | undefined {
  if (liveMode(phase) === "running") return "cyan";
  const finished = finishedRun(phase);
  if (finished !== null) return finished.exit_code === 0 ? "green" : "rose";
  return PHASE_TONE[phase.kind];
}

/** Pílula de estado: ponto (decorativo) + rótulo; o pulso respeita `prefers-reduced-motion`. */
function StatePill({ phase }: { phase: RunPhase }) {
  return (
    <span
      role="status"
      className="nd-pill nd-exec-pill"
      data-tone={phaseTone(phase)}
      data-app-state={phase.kind}
      data-app-mode={liveMode(phase) ?? (finishedRun(phase) !== null ? "finished" : undefined)}
    >
      <span aria-hidden="true" className="nd-exec-dot" data-pulse={phase.kind === "starting" ? "true" : undefined} />
      {runStateLabel(phase)}
    </span>
  );
}

interface ConfirmProps {
  projectPath: string;
  onCancel: () => void;
  onConfirm: () => void;
}

function ConfirmRunDialog({ projectPath, onCancel, onConfirm }: ConfirmProps) {
  const { t } = useT();
  const titleId = useId();
  const descId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <Dialog titleId={titleId} title={t("exec.confirm.title")} describedById={descId} onClose={onCancel} width={500} initialFocus={cancelRef}>
      <div id={descId} className="nd-dialog-text">
        <p>{t("exec.confirm.text")}</p>
        <code className="nd-code nd-mono nd-exec-command" data-run-command="">
          {DOCKER_RUN_COMMAND}
        </code>
        {projectPath !== "" ? <p className="nd-muted nd-mono nd-break">{projectPath}</p> : null}
      </div>
      <div className="nd-dialog-actions">
        <button ref={cancelRef} type="button" className="nd-btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button type="button" className="nd-btn" data-variant="primary" onClick={onConfirm}>
          <Icon name="play" size={14} />
          {t("exec.confirm.run")}
        </button>
      </div>
    </Dialog>
  );
}

interface PanelProps {
  projectId: string;
  /** Especificação concluída do espaço (a que "Executar" envia); `null` = nenhuma ainda. */
  specId: string | null;
  projectPath: string;
  size: AppContentProps["size"];
}

/**
 * Uma porta publicada (095/T07, decisão 6): o endereço, o botão "Abrir no App" (abre e foca a janela
 * App nessa porta) e, abaixo, a linha discreta "Prefere o navegador do sistema? Abrir em nova aba",
 * só com `http:`/`https:` (`safeHref`), em nova aba e sem `opener`/`referrer`. Com a página do
 * Nants aberta pela rede (fora do loopback, 095/T09), uma ajuda curta: as portas de visualização
 * (a faixa da 097) podem não estar liberadas na rede. Só ajuda, nunca a afirmação de que está
 * inalcançável. O endereço é um só (decisão 4): o host da página e a porta publicada da faixa.
 */
function PortRow({ port, note, onOpenInApp }: { port: AppPort; note: string | null; onOpenInApp: () => void }) {
  const { t } = useT();
  const nameId = useId();
  const href = safeHref(port.url);
  return (
    <li className="nd-exec-port-row" data-exec-port={port.url}>
      <div className="nd-exec-port-main">
        <span id={nameId} className="nd-exec-port" title={port.url}>
          <Icon name="link" size={14} />
          {port.service} · {hostPort(port.url)}
        </span>
        <button type="button" className="nd-btn nd-btn-sm" aria-describedby={nameId} data-open-in-app="" onClick={onOpenInApp}>
          <Icon name="app" size={14} />
          {t("exec.openInApp")}
        </button>
      </div>
      {note !== null ? (
        <p className="nd-exec-port-hint" data-port-hint="">
          {note}
        </p>
      ) : null}
      {href !== null ? (
        <p className="nd-exec-port-alt">
          {t("exec.preferBrowser")}{" "}
          <a className="nd-link" href={href} target="_blank" rel="noopener noreferrer" aria-describedby={nameId} data-open-browser="">
            {t("app.openNewTab")}
          </a>
        </p>
      ) : null}
    </li>
  );
}

export function ExecutionPanel({ projectId, specId, projectPath, size }: PanelProps) {
  const shell = useShell();
  const { t } = useT();
  const layout = useContext(WindowLayoutContext);
  const layoutRef = useRef(layout);
  layoutRef.current = layout;
  const visible = useWindowVisibility("execution");
  const run = useRunSession(projectId, specId, {
    visible,
    logs: true,
    onLive: () => {
      showAppLiveToast(shell.showToast);
      // A página "foi aberta na janela App": se ela está fechada, abre.
      if (layoutRef.current !== null && layoutRef.current.windows.app?.open !== true) shell.openWindow("app");
    },
  });
  const { snap, actions } = run;
  const { phase, logs } = snap;
  const askable =
    snap.failure !== null &&
    snap.failure.action === "ask_architect_fix" &&
    (specId === null || snap.failure.spec_id === specId) &&
    (phase.kind === "stopped" || phase.kind === "exited");
  const [copied, setCopied] = useState(false);
  const reasonId = useId();

  // Fechar a Execução com a confirmação aberta a cancela (o App não fica preso em "subindo").
  const confirmingRef = useRef(snap.confirming);
  confirmingRef.current = snap.confirming;
  const cancelRef = useRef(actions.cancelStart);
  cancelRef.current = actions.cancelStart;
  useEffect(
    () => () => {
      if (confirmingRef.current) cancelRef.current();
    },
    [],
  );

  const blocked = gateBlocked(shell.readiness, "execute");
  const startable = canStart(snap);
  const port = firstPort(phase);
  const ports = phase.kind === "running" ? phase.ports : [];
  // Ajuda curta na linha da porta (095/T09, 097): só pela rede e enquanto a faixa em uso não foi
  // marcada como liberada ("Já liberei").
  const network = useNetworkView(snap.network);
  const range = network?.lan_view_ports ?? null;
  const remote = !isLoopbackHost(reachEnvironment.pageHostname()) && !rangeReleased(shell.lanReleased, range);
  const portNote = (p: AppPort): string | null =>
    !remote ? null : range !== null ? reachRangeNote(range) : reachPortNote(urlPort(p.url) ?? String(p.published));
  const noOutput = logs === null || logs === "";
  const reason = specId === null ? t("exec.noSpecReason") : null;

  return (
    <div className="nd-exec nd-app-scroll" data-execution="" data-size={size}>
      <section className="nd-exec-card" aria-label={t("exec.card")} data-run-panel="">
        <div className="nd-exec-card-text">
          <span className="nd-exec-spec">{specId ?? t("exec.noCompletedSpec")}</span>
          <span className="nd-exec-line">{port !== null ? t("exec.composePort", { port: port.published }) : "docker compose up"}</span>
          {phase.kind === "exited" ? <span className="nd-exec-line">{appExitedText(phase.code, phase.reason)}</span> : null}
          {reason !== null ? (
            <span id={reasonId} className="nd-exec-line">
              {reason}
            </span>
          ) : null}
        </div>
        <StatePill phase={phase} />
        {startable ? (
          <button
            type="button"
            className="nd-btn"
            data-variant="primary"
            disabled={specId === null || blocked}
            aria-describedby={reason !== null ? reasonId : undefined}
            onClick={actions.requestStart}
          >
            <Icon name="play" size={14} />
            {t("exec.start")}
          </button>
        ) : null}
        {phase.kind === "running" || phase.kind === "other" ? (
          <button type="button" className="nd-btn" data-variant="danger" onClick={actions.stop} disabled={snap.stopping} aria-busy={snap.stopping}>
            <Icon name="stop" size={14} />
            {t(snap.stopping ? "run.stopping" : "run.stop")}
          </button>
        ) : null}
        {specId !== null ? (
          <button
            type="button"
            className="nd-btn"
            data-variant="ghost"
            data-open-console=""
            onClick={() => {
              actions.openConsole();
              shell.openWindow("app");
            }}
          >
            <Icon name="terminal" size={14} />
            {t("app.openConsole")}
          </button>
        ) : null}
      </section>
      {ports.length > 0 ? (
        <ul aria-label={t("exec.ports")} className="nd-exec-ports">
          {ports.map((p) => (
            <PortRow
              key={`${p.service}:${p.published}`}
              port={p}
              note={portNote(p)}
              onOpenInApp={() => {
                actions.showPort(p.url);
                shell.openWindow("app");
              }}
            />
          ))}
        </ul>
      ) : null}
      {phase.kind === "running" && snap.notices.length > 0 ? (
        <p role="status" aria-label={t("exec.portNotice")} className="nd-strip" data-tone="amber">
          {snap.notices.map((notice) => noticeText(notice, notice.text)).join(" ")}
        </p>
      ) : null}
      {startable && specId !== null && blocked && shell.readiness !== null ? (
        <ReadinessNotice title={t("exec.blockedTitle")} missing={shell.readiness.execute.missing} />
      ) : null}
      {snap.problem !== null ? <ProblemNotice message={runProblemText(snap.problem)} missing={snap.problem.missing} prereq={snap.problem.prereq} /> : null}
      {askable ? (
        <div className="nd-exec-fix" data-exec-fix="">
          <FixButton run={run} primary={false} />
        </div>
      ) : null}
      {snap.problem !== null && snap.errorDetail !== null ? (
        <details className="nd-details nd-exec-detail">
          <summary>
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {t("exec.details")}
          </summary>
          <pre aria-label={t("exec.detailsLabel")} className="nd-mono nd-prewrap">
            {snap.errorDetail}
          </pre>
        </details>
      ) : null}
      <section className="nd-exec-console" aria-label="Console" data-exec-console="">
        <div className="nd-exec-console-head">
          <span className="nd-exec-console-title">{t("exec.log")}</span>
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant="ghost"
            disabled={noOutput}
            onClick={() => {
              void copyText(logs ?? "").then((ok) => setCopied(ok));
            }}
          >
            <Icon name={copied ? "check" : "copy"} size={14} />
            {copied ? t("result.copied") : t("exec.copyLog")}
          </button>
        </div>
        <div className="nd-exec-console-body">
          <AutoScrollBox
            items={NO_ITEMS}
            tailKey={logs}
            label={t("exec.logLabel")}
            className="nd-app-log nd-mono"
            scrollAttrs={{ role: "log", "aria-live": "off", "data-run-log": "" }}
          >
            {noOutput ? t("exec.noOutput") : logs}
          </AutoScrollBox>
        </div>
      </section>
      <p className="nd-exec-hint" data-exec-hint="">
        <Icon name="info" size={14} />
        <span>{t("exec.hint")}</span>
      </p>
      {snap.confirming ? (
        <ConfirmRunDialog
          projectPath={projectPath}
          onCancel={actions.cancelStart}
          onConfirm={() => actions.confirmStart({ readiness: shell.readiness, reloadReadiness: shell.reloadReadiness })}
        />
      ) : null}
    </div>
  );
}

function ExecutionContentView({ size }: AppContentProps) {
  const space = useSpace();
  if (space === null) return null;
  const { data } = space;
  const spec = data.activeSpec;
  const specId = spec !== null && spec.status === "COMPLETED" ? spec.id : null;
  return (
    <ExecutionPanel
      key={`${data.projectId}:${specId ?? ""}`}
      projectId={data.projectId}
      specId={specId}
      projectPath={data.project?.path ?? ""}
      size={size}
    />
  );
}

export const ExecutionContent = memo(ExecutionContentView);
