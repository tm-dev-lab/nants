/*
 * Mensagem "Relatório final" do Arquiteto na Conversa de uma spec concluída. Sem relatório, os fatos (o que foi criado, como executar,
 * histórico de integração e pasta); com relatório, o Markdown restrito com "Como executar" em
 * destaque e as evidências recolhidas. Somente leitura; tudo como texto React.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { getSpecResult } from "../../api/client";
import type { ResultCommit, ResultFile, ResultTask, SpecReport, SpecResult } from "../../api/types";
import { reportFailureMessage } from "../../copy";
import { Icon } from "../../desktop/Icon";
import { formatDateTime, type MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { findHowToSection, parseReportMarkdown } from "../../reportMarkdown";
import { loadSpecReport } from "../../specReport";
import { reportKey, useAppStore } from "../../store";
import { ChatCard, ChatCardHead } from "./ChatCard";
import { ReportMarkdown } from "../shared/ReportMarkdown";

const COPIED_FEEDBACK_MS = 2_000;

const CHANGE_LABEL_KEYS: Readonly<Record<ResultFile["change"], MessageKey>> = { A: "result.change.A", M: "result.change.M", D: "result.change.D" };


export type LoadState = { status: "loading" } | { status: "error" } | { status: "ready"; result: SpecResult };
export type ReportState = { status: "loading" } | { status: "absent" } | { status: "failed" } | { status: "ready"; report: SpecReport };

interface Props {
  projectId: string;
  specId: string;
  reportPending?: boolean;
  onReportAt?: (at: number | null) => void;
}

export function ResultMessage({ projectId, specId, reportPending = false, onReportAt }: Props) {
  const [load, setLoad] = useState<LoadState>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [report, setReport] = useState<ReportState>({ status: "loading" });
  const [reportAttempt, setReportAttempt] = useState(0);
  const signal = useAppStore((s) => s.reportSignals[reportKey(projectId, specId)] ?? null);
  const { t } = useT();
  const mine = signal !== null && signal.specId === specId ? signal : null;
  const initialSignal = useRef(mine);

  useEffect(() => {
    let current = true;
    void getSpecResult(projectId, specId).then((result) => {
      if (!current) return;
      setLoad(result.ok ? { status: "ready", result: result.data } : { status: "error" });
    });
    return () => {
      current = false;
    };
  }, [projectId, specId, attempt]);

  useEffect(() => {
    let current = true;
    void loadSpecReport(projectId, specId).then((outcome) => {
      if (!current || outcome.kind === "stale") return;
      if (outcome.kind === "ready") setReport({ status: "ready", report: outcome.report });
      else if (outcome.kind === "absent") setReport({ status: "absent" });
      else setReport({ status: "failed" });
    });
    return () => {
      current = false;
    };
  }, [projectId, specId, reportAttempt]);

  useEffect(() => {
    if (mine === null || mine === initialSignal.current || mine.kind !== "ready") return;
    setReport({ status: "loading" });
    setReportAttempt((n) => n + 1);
  }, [mine]);

  const failureCode = mine !== null && mine.kind === "failed" ? mine.code : null;
  const retryFacts = () => {
    setLoad({ status: "loading" });
    setAttempt((n) => n + 1);
  };
  const retryReport = () => {
    setReport({ status: "loading" });
    setReportAttempt((n) => n + 1);
  };

  const readyReport = report.status === "ready" ? report.report : null;
  const generatedAt = readyReport !== null ? Date.parse(readyReport.generated_at) : Number.NaN;
  useEffect(() => {
    onReportAt?.(Number.isNaN(generatedAt) ? null : generatedAt);
  }, [generatedAt, onReportAt]);

  return (
    <ChatCard as="section" aria-label={t("result.title")} data-report-message="">
      <ChatCardHead agent="architect" extra={<span className="nd-task-tag">{t("result.title")}</span>} at={Number.isNaN(generatedAt) ? null : generatedAt} />
      <div className="nd-chat-card-body nd-bubble nd-stack">
        {readyReport !== null ? (
          <ReportBody report={readyReport} facts={load} onRetryFacts={retryFacts} />
        ) : (
          <>
            <ReportNotice pending={reportPending} failureCode={failureCode} absent={report.status === "absent"} failed={report.status === "failed"} onRetry={retryReport} />
            {load.status === "loading" ? (
              <p role="status" className="nd-muted">
                {t("result.loading")}
              </p>
            ) : load.status === "error" ? (
              <FactsError onRetry={retryFacts} />
            ) : (
              <ResultBody result={load.result} />
            )}
          </>
        )}
      </div>
    </ChatCard>
  );
}

function RetryButton({ onClick }: { onClick: () => void }) {
  const { t } = useT();
  return (
    <button type="button" onClick={onClick} className="nd-btn nd-btn-sm">
      <Icon name="refresh" size={14} />
      {t("common.tryAgain")}
    </button>
  );
}

export function FactsError({ onRetry }: { onRetry: () => void }) {
  const { t } = useT();
  return (
    <div className="nd-stack-sm">
      <p className="nd-muted">{t("result.factsError")}</p>
      <RetryButton onClick={onRetry} />
    </div>
  );
}

export function ReportNotice({ pending, failureCode, absent, failed, onRetry }: { pending: boolean; failureCode: string | null; absent: boolean; failed: boolean; onRetry: () => void }) {
  const { t } = useT();
  if (pending) {
    return (
      <p role="status" className="nd-muted">
        {t("report.pending")}
      </p>
    );
  }
  if (absent) {
    return (
      <div className="nd-stack-sm">
        <p className="nd-muted">{t("report.absent")}</p>
        {failureCode !== null ? <p className="nd-muted">{reportFailureMessage(failureCode)}</p> : null}
      </div>
    );
  }
  if (failureCode !== null) return <p className="nd-muted">{reportFailureMessage(failureCode)}</p>;
  if (failed) {
    return (
      <div className="nd-stack-sm">
        <p className="nd-muted">{t("report.loadFailed")}</p>
        <RetryButton onClick={onRetry} />
      </div>
    );
  }
  return null;
}

export function ReportBody({ report, facts, onRetryFacts }: { report: SpecReport; facts: LoadState; onRetryFacts: () => void }) {
  const { t, lang } = useT();
  // O texto de reserva da imagem sem descrição segue o idioma: ele entra nas dependências.
  const { howTo, rest } = useMemo(() => findHowToSection(parseReportMarkdown(report.markdown)), [report.markdown, lang]);
  return (
    <div className="nd-stack">
      <header className="nd-stack-sm">
        <h3 className="nd-result-title">{t("result.title")}</h3>
        <p className="nd-muted">{t("result.generatedBy", { date: formatDate(report.generated_at) })}</p>
      </header>
      {howTo !== null ? (
        <section aria-label={t("result.howTo")} className="nd-howto">
          <ReportMarkdown blocks={howTo} />
        </section>
      ) : null}
      <article aria-label={t("result.content")}>
        <ReportMarkdown blocks={rest} />
      </article>
      {facts.status === "error" ? <FactsError onRetry={onRetryFacts} /> : null}
      {facts.status === "ready" && hasEvidence(facts.result) ? (
        <details className="nd-details" data-evidence="">
          <summary className="nd-section-label">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {t("result.evidence")}
          </summary>
          <EvidenceBody result={facts.result} />
        </details>
      ) : null}
    </div>
  );
}

/**
 * Há evidência a mostrar (094/T08, A-10): tarefas, arquivos, commits ou o comando de testes. Sem
 * nenhuma, o bloco "Evidências" não aparece (um título com a caixa vazia parecia quebrado).
 */
export function hasEvidence(result: SpecResult): boolean {
  return result.tasks.length > 0 || result.files.length > 0 || result.commits.length > 0 || (result.test_command !== null && result.test_command !== "");
}

function EvidenceBody({ result }: { result: SpecResult }) {
  const { t } = useT();
  return (
    <div className="nd-stack">
      {result.tasks.length > 0 ? (
        <div className="nd-stack-sm">
          <h3 className="nd-section-label">{t("result.tasks")}</h3>
          <TaskList tasks={result.tasks} />
        </div>
      ) : null}
      <div className="nd-stack-sm">
        <h3 className="nd-section-label">{t("result.created")}</h3>
        <p className="nd-muted">{t("result.legend")}</p>
        <FileList files={result.files} truncated={result.files_truncated} />
      </div>
      {result.test_command !== null && result.test_command !== "" ? <p className="nd-mono nd-break">{t("result.tests", { command: result.test_command })}</p> : null}
      <div className="nd-stack-sm">
        <h3 className="nd-section-label">{t("result.history")}</h3>
        <CommitList commits={result.commits} />
      </div>
      <ProjectPath path={result.project_path} />
    </div>
  );
}

export function ResultBody({ result }: { result: SpecResult }) {
  const integrated = result.tasks.filter((task) => task.status === "DONE").length;
  const { t, tp } = useT();
  return (
    <>
      <header className="nd-stack-sm">
        <p className="nd-result-done">{tp("result.done", integrated)}</p>
        <p className="nd-muted nd-break">{result.title}</p>
        <TaskList tasks={result.tasks} />
      </header>
      <div className="nd-stack-sm">
        <h3 className="nd-section-label">{t("result.howTo")}</h3>
        {result.readme === null ? (
          <p className="nd-muted">{t("result.noReadme")}</p>
        ) : (
          <>
            <p className="nd-muted">{t("result.readmeExcerpt")}</p>
            <pre className="nd-md-pre">{result.readme.text}</pre>
            {result.readme.truncated ? <p className="nd-muted">{t("result.readmeTruncated")}</p> : null}
          </>
        )}
        {result.test_command !== null && result.test_command !== "" ? <p className="nd-mono nd-break">{t("result.tests", { command: result.test_command })}</p> : null}
      </div>
      <details className="nd-details">
        <summary className="nd-section-label">
          <Icon name="chevron-right" size={14} className="nd-details-chevron" />
          {t("result.details")}
        </summary>
        <div className="nd-stack-sm">
          <h3 className="nd-section-label">{t("result.created")}</h3>
          <p className="nd-muted">{t("result.legend")}</p>
          <FileList files={result.files} truncated={result.files_truncated} />
        </div>
        <div className="nd-stack-sm">
          <h3 className="nd-section-label">{t("result.history")}</h3>
          <CommitList commits={result.commits} />
        </div>
        <ProjectPath path={result.project_path} />
      </details>
    </>
  );
}

function ProjectPath({ path }: { path: string }) {
  const { t } = useT();
  return (
    <div className="nd-stack-sm">
      <h3 className="nd-section-label">{t("result.projectFolder")}</h3>
      <code className="nd-mono nd-break">{path}</code>
      <CopyPathButton path={path} />
    </div>
  );
}

function TaskList({ tasks }: { tasks: readonly ResultTask[] }) {
  const { t } = useT();
  if (tasks.length === 0) return null;
  return (
    <ul aria-label={t("result.tasks")} className="nd-plain-list nd-muted">
      {tasks.map((task) => (
        <li key={task.id} className="nd-break">
          <span className="nd-mono">{task.id}</span> {task.title}
          {task.error !== undefined ? <span className="nd-error-inline"> {t("result.invalidFile")}</span> : null}
        </li>
      ))}
    </ul>
  );
}

function FileList({ files, truncated }: { files: readonly ResultFile[]; truncated: boolean }) {
  const { t } = useT();
  if (files.length === 0 && !truncated) return <p className="nd-muted">{t("result.noFiles")}</p>;
  return (
    <>
      <ul aria-label={t("result.files")} className="nd-plain-list">
        {files.map((file) => (
          <li key={`${file.change}:${file.path}`} className="nd-row">
            <span className="nd-muted" title={t(CHANGE_LABEL_KEYS[file.change])}>
              {file.change}
            </span>
            <span className="nd-mono nd-break">{file.path}</span>
          </li>
        ))}
      </ul>
      {truncated ? <p className="nd-muted">{t("result.moreFiles")}</p> : null}
    </>
  );
}

function CommitList({ commits }: { commits: readonly ResultCommit[] }) {
  const { t } = useT();
  if (commits.length === 0) return <p className="nd-muted">{t("result.noCommits")}</p>;
  return (
    <ul aria-label={t("result.commits")} className="nd-plain-list">
      {commits.map((commit) => (
        <li key={commit.hash} className="nd-row nd-wrap">
          <code className="nd-mono nd-muted">{commit.hash}</code>
          <span className="nd-break">{commit.subject}</span>
          <time dateTime={commit.date} className="nd-muted">
            {formatDate(commit.date)}
          </time>
        </li>
      ))}
    </ul>
  );
}

/** Data e hora curtas no idioma ativo (formatador da 101); texto inválido volta como veio. */
export function formatDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return formatDateTime(date, { dateStyle: "short", timeStyle: "short" });
}

export async function copyText(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Permissão negada ou contexto inseguro: cai no fallback.
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  } catch {
    return false;
  }
}

function CopyPathButton({ path }: { path: string }) {
  const [copied, setCopied] = useState(false);
  const { t } = useT();
  const timer = useRef<number | null>(null);
  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);
  const onCopy = () => {
    void copyText(path).then((ok) => {
      if (!ok) return;
      setCopied(true);
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), COPIED_FEEDBACK_MS);
    });
  };
  return (
    <button type="button" onClick={onCopy} className="nd-btn nd-btn-sm">
      <Icon name={copied ? "check" : "copy"} size={14} />
      {copied ? t("result.copied") : t("result.copyPath")}
    </button>
  );
}
