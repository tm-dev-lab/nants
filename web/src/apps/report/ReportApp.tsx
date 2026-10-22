/*
 * Janela Relatório (`DEFAULT_APPS.report.Content`, em `memo`). Com a spec concluída:
 * - no topo, "Projeto concluído", os fatos (tarefas
 *   integradas, pasta) e "Ver relatório" (rola até o relatório dentro da janela), mais "Abrir o
 *   App" (a execução do app fica na janela App);
 * - o relatório do Arquiteto em Markdown restrito, com "Como executar" em destaque e as
 *   evidências recolhidas; sem relatório (ou enquanto é escrito), os fatos do resultado.
 * Os blocos de conteúdo são os mesmos da mensagem "Relatório final" da Conversa.
 *
 * A carga do relatório é própria da janela (`getSpecReport`), sem a sequência por projeto do
 * `loadSpecReport`: a Conversa e o Relatório abertos juntos não se anulam.
 */
import { memo, useEffect, useRef, useState } from "react";
import { getSpecReport, getSpecResult } from "../../api/client";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useShell, useSpace } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { scrollWithin } from "../../scroll";
import { reportKey, useAppStore } from "../../store";
import { ReportBody, ReportNotice, ResultBody, FactsError, type LoadState, type ReportState } from "../conversation/ResultMessage";
import type { AppContentProps } from "../registry";
import "../shared/apps.css";
import "./report.css";
import { integratedCount } from "./summary";

function ReportContentView(_props: AppContentProps) {
  const space = useSpace();
  const shell = useShell();
  // Assina o idioma: o conteúdo (em `memo`) re-renderiza na troca.
  const { t } = useT();
  if (space === null) return null;
  const spec = space.data.activeSpec;
  if (spec === null || spec.status !== "COMPLETED") {
    return <EmptyState icon="report" message={t("windowEmpty.report")} action={{ label: t("report.openBoard"), onClick: () => shell.openWindow("board") }} />;
  }
  return (
    <ReportView
      key={`${space.data.projectId}:${spec.id}`}
      projectId={space.data.projectId}
      specId={spec.id}
      reportPending={space.data.state?.report_pending === true}
      onOpenApp={() => shell.openWindow("app")}
    />
  );
}

export const ReportContent = memo(ReportContentView);

interface ReportViewProps {
  projectId: string;
  specId: string;
  reportPending: boolean;
  onOpenApp: () => void;
}

function ReportView({ projectId, specId, reportPending, onOpenApp }: ReportViewProps) {
  const { t, tp } = useT();
  const [facts, setFacts] = useState<LoadState>({ status: "loading" });
  const [factsAttempt, setFactsAttempt] = useState(0);
  const [report, setReport] = useState<ReportState>({ status: "loading" });
  const [reportAttempt, setReportAttempt] = useState(0);
  const signal = useAppStore((s) => s.reportSignals[reportKey(projectId, specId)] ?? null);
  const firstSignal = useRef(signal);
  const wasPending = useRef(reportPending);
  const scrollRef = useRef<HTMLDivElement>(null);
  const reportRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let current = true;
    void getSpecResult(projectId, specId).then((r) => {
      if (current) setFacts(r.ok ? { status: "ready", result: r.data } : { status: "error" });
    });
    return () => {
      current = false;
    };
  }, [projectId, specId, factsAttempt]);

  useEffect(() => {
    let current = true;
    void getSpecReport(projectId, specId).then((r) => {
      if (!current) return;
      if (r.ok) setReport({ status: "ready", report: r.data });
      else if (r.error.kind === "http" && r.error.status === 404) setReport({ status: "absent" });
      else setReport({ status: "failed" });
    });
    return () => {
      current = false;
    };
  }, [projectId, specId, reportAttempt]);

  // Relatório novo (sinal `report_ready`) ou o Arquiteto terminou de escrever: relê.
  useEffect(() => {
    if (signal === null || signal === firstSignal.current || signal.kind !== "ready") return;
    setReport({ status: "loading" });
    setReportAttempt((n) => n + 1);
  }, [signal]);
  useEffect(() => {
    if (wasPending.current && !reportPending) {
      setReport({ status: "loading" });
      setReportAttempt((n) => n + 1);
    }
    wasPending.current = reportPending;
  }, [reportPending]);

  const failureCode = signal !== null && signal.kind === "failed" ? signal.code : null;
  const ready = report.status === "ready" ? report.report : null;
  const integrated = facts.status === "ready" ? integratedCount(facts.result.tasks) : null;

  const viewReport = (): void => {
    const container = scrollRef.current;
    const target = reportRef.current;
    if (container === null || target === null) return;
    scrollWithin(container, target);
    target.focus({ preventScroll: true });
  };
  const retryFacts = (): void => {
    setFacts({ status: "loading" });
    setFactsAttempt((n) => n + 1);
  };
  const retryReport = (): void => {
    setReport({ status: "loading" });
    setReportAttempt((n) => n + 1);
  };

  return (
    <div ref={scrollRef} className="nd-app-scroll nd-app-page nd-report" data-report-window="">
      {reportPending ? null : (
        <section className="nd-completed" aria-label={t("report.completed")} data-completed-banner="">
          <p className="nd-completed-title">
            <Icon name="check" size={16} />
            {t("report.completed")}
          </p>
          {integrated !== null ? (
            <p className="nd-muted" data-completed-facts="">
              {tp("report.integrated", integrated)}
              {facts.status === "ready" && facts.result.test_command !== null && facts.result.test_command !== ""
                ? ` · ${t("report.tests", { command: facts.result.test_command })}`
                : ""}
            </p>
          ) : null}
          <div className="nd-row">
            {/* 095/T07, A-05: secundária; dentro da própria janela do relatório, primária era redundante. */}
            <button type="button" className="nd-btn" data-report-view="" onClick={viewReport}>
              <Icon name="report" size={14} />
              {t("report.view")}
            </button>
            <button type="button" className="nd-btn" onClick={onOpenApp}>
              <Icon name="app" size={14} />
              {t("report.openApp")}
            </button>
          </div>
        </section>
      )}
      <div ref={reportRef} tabIndex={-1} className="nd-report-body" data-report-body="">
        {ready !== null ? (
          <ReportBody report={ready} facts={facts} onRetryFacts={retryFacts} />
        ) : (
          <div className="nd-stack">
            <ReportNotice pending={reportPending} failureCode={failureCode} absent={report.status === "absent"} failed={report.status === "failed"} onRetry={retryReport} />
            {facts.status === "loading" ? (
              <p role="status" className="nd-muted">
                {t("report.loadingResult")}
              </p>
            ) : facts.status === "error" ? (
              <FactsError onRetry={retryFacts} />
            ) : (
              <ResultBody result={facts.result} />
            )}
          </div>
        )}
      </div>
    </div>
  );
}
