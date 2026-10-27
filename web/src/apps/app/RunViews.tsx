/*
 * As formas da janela App além da página (spec 130, mockup aprovado na T01), escolhidas pelo que
 * o programa faz, nunca pela tecnologia:
 * - `RunningView`: fica rodando sem página (worker, serviço, API sem tela): a porta, o comando, o
 *   registro ao vivo e o Parar;
 * - `FinishedView`: rodou e terminou (CLI, script, job): o comando, o código, a saída e os arquivos
 *   que ele criou ou mudou (Abrir nos Arquivos e Copiar o caminho);
 * - `FailureView`: o ambiente não subiu: o fim do registro, "Pedir ao Arquiteto para corrigir" e
 *   "Tentar de novo";
 * - `SandboxView`: o Arquiteto declarou no plano que o app não roda no sandbox (janela nativa,
 *   app móvel): explica, mostra como rodar no computador e o que foi provado.
 * As peças partilhadas com o Console (`ConsolePanel.tsx`): o "Como usar", as provas e a lista dos
 * arquivos. Todo texto do servidor é renderizado como texto React; os rótulos vêm do dicionário.
 */
import { useEffect, useId, useState, type ReactNode } from "react";
import type { AppFailureView, AppPort, ChangedFile } from "../../api/client";
import { apiFailureText, translateCode } from "../../i18n/serverCodes";
import { Icon } from "../../desktop/Icon";
import { useShell, useSpace } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import type { MessageKey } from "../../i18n";
import { AutoScrollBox, type ScrollItem } from "../conversation/AutoScrollBox";
import { copyText } from "../conversation/ResultMessage";
import { CaptureThumbnails } from "../shared/CaptureThumbnails";
import { MarkdownText } from "../shared/ReportMarkdown";
import { requestOpenFile } from "../files/openRequest";
import { finishedLabel, hostPort, safeHref, type RunPhase } from "./appModel";
import type { useAppGuide } from "./useAppGuide";
import type { useRunSession } from "./runSession";

const NO_ITEMS: readonly ScrollItem[] = [];

type GuideState = ReturnType<typeof useAppGuide>;
type RunSessionApi = ReturnType<typeof useRunSession>;

/** Cabeçalho de uma forma: o selo do modo, o que mais couber e as ações à direita. */
function ViewHead({ tone, label, children, actions }: { tone: string; label: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="nd-runview-head" data-runview-head="">
      <span role="status" className="nd-pill nd-pill-nowrap nd-runview-badge" data-tone={tone} data-runview-badge="">
        {label}
      </span>
      {children}
      <span className="nd-runview-spacer" />
      {actions !== undefined ? <div className="nd-runview-actions">{actions}</div> : null}
    </div>
  );
}

/** Rótulo de uma seção (caixa alta pelo CSS). */
function SectionLabel({ id, children }: { id?: string; children: ReactNode }) {
  return (
    <h3 id={id} className="nd-runview-label">
      {children}
    </h3>
  );
}

// --- Arquivos criados ou mudados ---

interface FilesProps {
  projectId: string;
  files: readonly ChangedFile[];
  total: number;
  /** Rótulo da seção (o padrão é "Arquivos criados ou mudados"). */
  titleKey?: MessageKey;
}

/** Um arquivo: o selo (novo ou mudou), o caminho, Abrir (nos Arquivos) e Copiar (o caminho). */
function FileRow({ projectId, file }: { projectId: string; file: ChangedFile }) {
  const shell = useShell();
  const space = useSpace();
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  const open = (): void => {
    space?.setSelection({ filePath: file.path, fileSource: "base" });
    requestOpenFile(projectId, file.path);
    shell.openWindow("files");
  };
  return (
    <li className="nd-runview-file" data-changed-file={file.path}>
      <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone={file.change === "added" ? "green" : "amber"}>
        {t(file.change === "added" ? "runview.files.added" : "runview.files.changed")}
      </span>
      <span className="nd-runview-file-path nd-mono" title={file.path}>
        {file.path}
      </span>
      <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" aria-label={t("runview.files.openLabel", { path: file.path })} data-file-open="" onClick={open}>
        <Icon name="file" size={14} />
        {t("runview.files.open")}
      </button>
      <button
        type="button"
        className="nd-btn nd-iconbtn nd-btn-sm"
        data-variant="ghost"
        aria-label={copied ? t("runview.files.copied") : t("runview.files.copyLabel", { path: file.path })}
        title={t("runview.files.copy")}
        data-file-copy=""
        onClick={() => {
          void copyText(file.path).then((ok) => setCopied(ok));
        }}
      >
        <Icon name={copied ? "check" : "copy"} size={14} />
      </button>
    </li>
  );
}

export function ChangedFilesList({ projectId, files, total, titleKey = "runview.files.title" }: FilesProps) {
  const { t, tp } = useT();
  const labelId = useId();
  const more = Math.max(0, total - files.length);
  return (
    <section className="nd-runview-files" aria-labelledby={labelId} data-changed-files="">
      <SectionLabel id={labelId}>{t(titleKey)}</SectionLabel>
      {files.length === 0 ? (
        <p className="nd-runview-muted">{t("runview.files.none")}</p>
      ) : (
        <ul className="nd-runview-file-list">
          {files.map((file) => (
            <FileRow key={file.path} projectId={projectId} file={file} />
          ))}
        </ul>
      )}
      {more > 0 ? <p className="nd-runview-muted">{tp("runview.files.more", more)}</p> : null}
    </section>
  );
}

// --- "Como usar" e as provas ---

export function HowToUseCard({ guide, title = "runview.howTo" }: { guide: GuideState; title?: MessageKey }) {
  const shell = useShell();
  const { t } = useT();
  const labelId = useId();
  const how = guide?.status === "ok" ? guide.guide.howToUse : null;
  return (
    <section className="nd-runview-card" aria-labelledby={labelId} data-how-to-use="">
      <SectionLabel id={labelId}>{t(title)}</SectionLabel>
      {guide === null || guide.status === "loading" ? (
        <p className="nd-runview-muted">{t("runview.loadingGuide")}</p>
      ) : how === null ? (
        <p className="nd-runview-muted">{t("runview.howTo.empty")}</p>
      ) : (
        <>
          {how.source === "readme" ? <p className="nd-runview-muted">{t("runview.howTo.fromReadme")}</p> : null}
          <div className="nd-runview-md">
            <MarkdownText text={how.markdown} />
          </div>
        </>
      )}
      <button type="button" className="nd-btn nd-btn-sm nd-runview-link" data-variant="ghost" data-open-report="" onClick={() => shell.openWindow("report")}>
        <Icon name="report" size={14} />
        {t("runview.openReport")}
      </button>
    </section>
  );
}

export function ProofsCard({ projectId, specId, guide }: { projectId: string; specId: string | null; guide: GuideState }) {
  const { t, tp } = useT();
  const labelId = useId();
  const data = guide?.status === "ok" ? guide.guide : null;
  const final = data?.finalVerification ?? null;
  const finalKey: MessageKey =
    final === null ? "runview.proofs.final.none" : final.passed === true ? "runview.proofs.final.passed" : final.passed === false ? "runview.proofs.final.failed" : "runview.proofs.final.unknown";
  return (
    <section className="nd-runview-card" aria-labelledby={labelId} data-proofs="">
      <SectionLabel id={labelId}>{t("runview.proofs")}</SectionLabel>
      {data === null ? (
        <p className="nd-runview-muted">{t(guide?.status === "error" ? "runview.guideError" : "runview.loadingGuide")}</p>
      ) : (
        <ul className="nd-runview-proofs">
          <li data-tone={data.tasksTotal > 0 && data.tasksDone === data.tasksTotal ? "green" : undefined}>
            {t("runview.proofs.tasks", { done: data.tasksDone, total: data.tasksTotal })}
          </li>
          <li data-tone={final?.passed === true ? "green" : final?.passed === false ? "amber" : undefined}>{t(finalKey)}</li>
          {data.capturesTotal > 0 ? <li>{tp("runview.proofs.captures", data.capturesTotal)}</li> : null}
        </ul>
      )}
      {data !== null && specId !== null && data.captures.length > 0 ? <CaptureThumbnails projectId={projectId} specId={specId} names={data.captures} /> : null}
    </section>
  );
}

// --- Pedir ao Arquiteto para corrigir ---

/** O botão e, se o pedido falhar, o motivo ao lado. Leva o usuário à Conversa. */
export function FixButton({ run, primary = true }: { run: RunSessionApi; primary?: boolean }) {
  const shell = useShell();
  const { t } = useT();
  const { snap, actions } = run;
  return (
    <>
      <button
        type="button"
        className="nd-btn"
        data-variant={primary ? "primary" : undefined}
        disabled={snap.fixing}
        aria-busy={snap.fixing}
        data-ask-fix=""
        onClick={() => actions.askFix(() => shell.openWindow("conversation"))}
      >
        <Icon name="sparkles" size={14} />
        {t(snap.fixing ? "runview.fixing" : "runview.fix")}
      </button>
      {snap.fixProblem !== null ? (
        <p role="alert" className="nd-runview-error" data-fix-problem="">
          {apiFailureText(snap.fixProblem)}
        </p>
      ) : null}
    </>
  );
}

// --- Rodando sem página ---

function useMinutesSince(iso: string | null | undefined): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);
  if (iso === null || iso === undefined) return null;
  const started = Date.parse(iso);
  if (Number.isNaN(started)) return null;
  return Math.max(0, Math.floor((now - started) / 60_000));
}

interface RunningProps {
  phase: Extract<RunPhase, { kind: "running" }>;
  logs: string | null;
  stopping: boolean;
  onStop: () => void;
}

export function RunningView({ phase, logs, stopping, onStop }: RunningProps) {
  const { t, tp } = useT();
  const minutes = useMinutesSince(phase.startedAt);
  const port: AppPort | undefined = phase.ports[0];
  const href = port === undefined ? null : safeHref(port.url);
  const noLog = logs === null || logs === "";
  return (
    <section className="nd-runview" aria-label={t("app.state.runningNoPage")} data-runview="running">
      <ViewHead
        tone="cyan"
        label={t("app.state.runningNoPage")}
        actions={
          <button type="button" className="nd-btn" data-variant="danger" disabled={stopping} aria-busy={stopping} onClick={onStop}>
            <Icon name="stop" size={14} />
            {t(stopping ? "run.stopping" : "run.stop")}
          </button>
        }
      >
        {minutes !== null ? <span className="nd-runview-muted">{minutes < 1 ? t("runview.sinceNow") : tp("runview.since", minutes)}</span> : null}
      </ViewHead>
      <div className="nd-runview-cards">
        <div className="nd-runview-card" data-runview-port="">
          <span className="nd-runview-label-sm">{t("runview.port")}</span>
          {port === undefined ? (
            <span className="nd-runview-muted">{t("runview.noPort")}</span>
          ) : (
            <>
              <span className="nd-mono nd-runview-value">{hostPort(port.url)}</span>
              {href !== null ? (
                <a className="nd-link nd-runview-small" href={href} target="_blank" rel="noopener noreferrer">
                  {t("app.openNewTab")}
                </a>
              ) : null}
            </>
          )}
        </div>
        <div className="nd-runview-card" data-runview-command="">
          <span className="nd-runview-label-sm">{t("runview.command")}</span>
          <span className="nd-mono nd-runview-value">{phase.command ?? ""}</span>
          <span className="nd-runview-muted">{t("runview.commandNote")}</span>
        </div>
      </div>
      <div className="nd-runview-log">
        <AutoScrollBox items={NO_ITEMS} tailKey={logs} label={t("runview.logLabel")} className="nd-app-log nd-mono" scrollAttrs={{ role: "log", "aria-live": "off", "data-runview-log": "" }}>
          {noLog ? t("runview.noLog") : logs}
        </AutoScrollBox>
        <span className="nd-runview-live" data-runview-live="">
          {t("runview.live")}
        </span>
      </div>
      <p className="nd-runview-foot">{t("runview.runningHint")}</p>
    </section>
  );
}

// --- Terminou ---

interface FinishedProps {
  projectId: string;
  phase: Extract<RunPhase, { kind: "exited" }>;
  canRun: boolean;
  onRunAgain: () => void;
  onOpenConsole: () => void;
}

export function FinishedView({ projectId, phase, canRun, onRunAgain, onOpenConsole }: FinishedProps) {
  const { t, tp } = useT();
  const finished = phase.finished;
  if (finished === null || finished === undefined) return null;
  const code = finished.exit_code ?? phase.code;
  const ok = code === 0;
  const output = finished.output ?? "";
  const outsideMore = Math.max(0, finished.outside_work_total - finished.outside_work.length);
  return (
    <section className="nd-runview" aria-label={finishedLabel(code)} data-runview="finished">
      <ViewHead
        tone={ok ? "green" : "rose"}
        label={finishedLabel(code)}
        actions={
          <>
            <button type="button" className="nd-btn" data-open-console="" onClick={onOpenConsole}>
              <Icon name="terminal" size={14} />
              {t("runview.finished.openConsole")}
            </button>
            <button type="button" className="nd-btn" data-variant="primary" disabled={!canRun} data-run-again="" onClick={onRunAgain}>
              <Icon name="play" size={14} />
              {t("runview.finished.runAgain")}
            </button>
          </>
        }
      />
      {phase.command !== null && phase.command !== undefined ? (
        <p className="nd-runview-command nd-mono" data-runview-command="">
          {`$ ${phase.command}`}
        </p>
      ) : null}
      <div className="nd-runview-split">
        <div className="nd-runview-main">
          <AutoScrollBox items={NO_ITEMS} tailKey={output} label={t("runview.output.label")} className="nd-app-log nd-mono" scrollAttrs={{ role: "log", "aria-live": "off", "data-runview-output": "" }}>
            {output === "" ? t("runview.output.empty") : output}
          </AutoScrollBox>
        </div>
        <div className="nd-runview-side">
          <ChangedFilesList projectId={projectId} files={finished.files} total={finished.files_total} />
          {finished.outside_work.length > 0 ? (
            <section className="nd-runview-files" data-outside-work="">
              <SectionLabel>{t("runview.outside.title")}</SectionLabel>
              <ul className="nd-runview-plain">
                {finished.outside_work.map((path) => (
                  <li key={path} className="nd-mono nd-runview-small">
                    {path}
                  </li>
                ))}
              </ul>
              {outsideMore > 0 ? <p className="nd-runview-muted">{tp("runview.outside.more", outsideMore)}</p> : null}
              <p className="nd-runview-muted">{t("runview.outside.note")}</p>
            </section>
          ) : null}
        </div>
      </div>
      <p className="nd-runview-foot">{ok ? t("runview.finished.okHint") : code === null ? t("runview.finished.noCodeHint") : t("runview.finished.errorHint", { code })}</p>
    </section>
  );
}

// --- O ambiente não subiu ---

const FAILURE_TITLES: Readonly<Record<string, MessageKey>> = {
  app_docker_up_failed: "runview.failure.title.up",
  project_not_docker: "runview.failure.title.notDocker",
  app_exec_docker_failed: "runview.failure.title.exec",
};

interface FailureProps {
  failure: AppFailureView;
  run: RunSessionApi;
  canRetry: boolean;
  onRetry: () => void;
  /** Dentro do Console (falha de um comando): sem o selo de cabeçalho. */
  inline?: boolean;
}

export function FailureView({ failure, run, canRetry, onRetry, inline = false }: FailureProps) {
  const { t, formatTime } = useT();
  const titleId = useId();
  const titleKey = FAILURE_TITLES[failure.code];
  const title = titleKey !== undefined ? t(titleKey) : (translateCode("serverError", failure.code, undefined) ?? t("runview.failure.badge"));
  const at = Date.parse(failure.at);
  const askable = failure.action === "ask_architect_fix" || failure.action === null;
  return (
    <section className={inline ? "nd-runview-failure-inline" : "nd-runview"} aria-labelledby={titleId} data-runview="failure" data-failure-code={failure.code}>
      {inline ? null : <ViewHead tone="rose" label={t("runview.failure.badge")} />}
      <div className="nd-runview-card nd-runview-failure">
        <h2 id={titleId} className="nd-runview-title">
          {title}
        </h2>
        <p className="nd-runview-text">{t("runview.failure.text")}</p>
        {Number.isNaN(at) ? null : <p className="nd-runview-muted" data-failure-at="">{t("runview.failure.at", { time: formatTime(at) })}</p>}
        <div className="nd-runview-row">
          {askable ? <FixButton run={run} /> : null}
          {inline ? null : (
            <button type="button" className="nd-btn" disabled={!canRetry} data-retry="" onClick={onRetry}>
              <Icon name="refresh" size={14} />
              {t("runview.retry")}
            </button>
          )}
        </div>
      </div>
      <SectionLabel>{t("runview.failure.log")}</SectionLabel>
      <pre className="nd-app-log nd-mono nd-runview-faillog" aria-label={t("runview.failure.log")} data-failure-log="">
        {failure.detail === "" ? t("runview.failure.noLog") : failure.detail}
      </pre>
      <p className="nd-runview-foot">{t("runview.failure.note")}</p>
    </section>
  );
}

// --- Não roda no sandbox ---

interface SandboxProps {
  projectId: string;
  specId: string | null;
  guide: GuideState;
  onOpenConsole: () => void;
}

export function SandboxView({ projectId, specId, guide, onOpenConsole }: SandboxProps) {
  const { t } = useT();
  const titleId = useId();
  const note = guide?.status === "ok" ? guide.guide.sandboxNote : null;
  return (
    <section className="nd-runview" aria-labelledby={titleId} data-runview="sandbox">
      <ViewHead tone="amber" label={t("runview.sandbox.badge")} />
      <div className="nd-runview-split">
        <div className="nd-runview-main nd-runview-stack">
          <div className="nd-runview-card">
            <h2 id={titleId} className="nd-runview-title">
              {t("runview.sandbox.title")}
            </h2>
            {note !== null ? <p className="nd-runview-text" data-sandbox-note="">{note}</p> : null}
            <p className="nd-runview-text">{t("runview.sandbox.text")}</p>
          </div>
          <HowToUseCard guide={guide} title="runview.sandbox.how" />
          <button type="button" className="nd-btn nd-runview-start" data-open-console="" onClick={onOpenConsole}>
            <Icon name="terminal" size={14} />
            {t("runview.openConsole")}
          </button>
        </div>
        <div className="nd-runview-side">
          <ProofsCard projectId={projectId} specId={specId} guide={guide} />
        </div>
      </div>
    </section>
  );
}
