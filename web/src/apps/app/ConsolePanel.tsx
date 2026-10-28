/*
 * Console do projeto na janela App (spec 130, mockup aprovado: "Console" e "Console no celular").
 * O padrão para qualquer projeto: quando não há página nem processo rodando, o usuário roda os
 * comandos do projeto no container do ambiente aprovado.
 * - console de comando único (C-11): um comando por vez, sem entrada interativa, com o tempo
 *   máximo do servidor; cada comando passa por uma confirmação humana (diálogo com o comando
 *   exato) antes do `POST` com `confirm: true`;
 * - a saída ao vivo (eventos `app_exec_*`), o código e a duração de cada comando, o Parar, o
 *   histórico (clicar põe o comando no campo: repetir é confirmar de novo) e os arquivos que o
 *   último comando criou ou mudou (Abrir nos Arquivos e Copiar o caminho);
 * - ao lado, o "Como usar" do relatório final e o que foi provado (G2: nunca só um erro);
 * - no celular (janela estreita): abas Saída, Como usar e Provas, e o campo embaixo.
 * Todo texto do servidor é renderizado como texto React.
 */
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import type { AppFailureView } from "../../api/client";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { apiFailureText, translateCode } from "../../i18n/serverCodes";
import { AutoScrollBox, type ScrollItem } from "../conversation/AutoScrollBox";
import { ChangedFilesList, FailureView, HowToUseCard, ProofsCard } from "./RunViews";
import { useConsoleSession, type TranscriptEntry } from "./consoleSession";
import type { useAppGuide } from "./useAppGuide";
import type { useRunSession } from "./runSession";

const NO_ITEMS: readonly ScrollItem[] = [];
/** Entradas do histórico mostradas ao lado. */
const HISTORY_SHOWN = 8;

type GuideState = ReturnType<typeof useAppGuide>;
type RunSessionApi = ReturnType<typeof useRunSession>;
type Tab = "output" | "howto" | "proofs";

function ConfirmCommandDialog({ command, onCancel, onConfirm }: { command: string; onCancel: () => void; onConfirm: () => void }) {
  const { t } = useT();
  const titleId = useId();
  const descId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);
  return (
    <Dialog titleId={titleId} title={t("console.confirm.title")} describedById={descId} onClose={onCancel} width={500} initialFocus={cancelRef}>
      <div id={descId} className="nd-dialog-text">
        <p>{t("console.confirm.text")}</p>
        <code className="nd-code nd-mono nd-exec-command nd-break" data-console-command="">
          {command}
        </code>
      </div>
      <div className="nd-dialog-actions">
        <button ref={cancelRef} type="button" className="nd-btn" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button type="button" className="nd-btn" data-variant="primary" onClick={onConfirm}>
          <Icon name="play" size={14} />
          {t("console.run")}
        </button>
      </div>
    </Dialog>
  );
}

/** Duração legível: "0,9 s" ou "2 min 5 s". */
function useDuration(): (ms: number | null) => string {
  const { t, formatNumber } = useT();
  return (ms) => {
    if (ms === null) return "";
    const seconds = ms / 1000;
    if (seconds < 60) return t("console.duration.seconds", { seconds: formatNumber(seconds, { maximumFractionDigits: 1 }) });
    const whole = Math.round(seconds);
    return t("console.duration.minutes", { minutes: formatNumber(Math.floor(whole / 60)), seconds: formatNumber(whole % 60) });
  };
}

/** Linha de fim de um comando: código e duração, parado, tempo estourado ou não rodou. */
function ResultLine({ entry }: { entry: TranscriptEntry }) {
  const { t } = useT();
  const duration = useDuration();
  const result = entry.result;
  if (result === null) {
    return (
      <div className="nd-console-result" data-console-result="running">
        {t("console.result.running")}
      </div>
    );
  }
  const time = duration(result.duration_ms);
  let text: string;
  if (result.status === "exited") text = result.exit_code === null ? t("console.result.exitedNoCode", { duration: time }) : t("console.result.exited", { code: result.exit_code, duration: time });
  else if (result.status === "stopped") text = t("console.result.stopped", { duration: time });
  else if (result.status === "timed_out") text = t("console.result.timedOut", { duration: time });
  else text = t("console.result.failed");
  const notice = result.notice === null ? null : translateCode("serverError", result.notice.code, result.notice.params);
  return (
    <>
      <div className="nd-console-result" data-console-result={result.status} data-tone={result.status === "exited" && result.exit_code === 0 ? undefined : "rose"}>
        {text}
      </div>
      {entry.truncated || result.output_truncated ? <div className="nd-console-result">{t("console.result.truncated")}</div> : null}
      {notice !== null ? (
        <div className="nd-console-notice" data-console-notice={result.notice?.code}>
          {notice}
        </div>
      ) : null}
    </>
  );
}

function Transcript({ entries, emptyText }: { entries: readonly TranscriptEntry[]; emptyText: string }) {
  const { t } = useT();
  const tail = entries.map((e) => `${e.execId}:${e.text.length}:${e.result?.status ?? ""}`).join("|");
  return (
    <AutoScrollBox items={NO_ITEMS} tailKey={tail} label={t("console.outputLabel")} className="nd-app-log nd-mono nd-console-output" scrollAttrs={{ role: "log", "aria-live": "off", "data-console-output": "" }}>
      {entries.length === 0 ? (
        <span className="nd-console-empty">{emptyText}</span>
      ) : (
        entries.map((entry) => (
          <div key={entry.execId} className="nd-console-entry" data-console-entry={entry.execId}>
            <div className="nd-console-cmd">{`$ ${entry.command}`}</div>
            {entry.text !== "" ? <div className="nd-console-text">{entry.text}</div> : null}
            <ResultLine entry={entry} />
          </div>
        ))
      )}
    </AutoScrollBox>
  );
}

interface ConsolePanelProps {
  projectId: string;
  specId: string | null;
  compact: boolean;
  visible: boolean;
  guide: GuideState;
  run: RunSessionApi;
  /** Ação do cabeçalho (voltar ao app ou iniciá-lo). */
  headAction: ReactNode;
  /**
   * Spec 131 T05: a spec não declara como executar; o Console é a forma de uso. Mostra a linha que
   * explica isso e, quando houver, a decisão do plano (`nothing_to_run`).
   */
  noRun?: { decision: string | null } | null;
}

export function ConsolePanel({ projectId, specId, compact, visible, guide, run, headAction, noRun = null }: ConsolePanelProps) {
  const { t, tp, formatTime } = useT();
  const { snap, actions } = useConsoleSession(projectId, specId, visible);
  const [draft, setDraft] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("output");
  const inputRef = useRef<HTMLInputElement>(null);
  const inputId = useId();
  const limitsId = useId();
  const busy = snap.active !== null || snap.posting;
  const minutes = Math.max(1, Math.round(snap.timeoutSecs / 60));
  const lastFiles = snap.last?.status === "exited" || snap.last?.status === "stopped" || snap.last?.status === "timed_out" ? snap.last : null;
  // A falha do Console vem do próprio resultado (com o log e o próximo passo); um comando que
  // rodou a esconde. Sem resultado nesta aba, vale a falha guardada no servidor.
  const lastResult = snap.last;
  let consoleFailure: AppFailureView | null = null;
  if (lastResult === null) {
    consoleFailure = run.snap.failure !== null && run.snap.failure.code === "app_exec_docker_failed" ? run.snap.failure : null;
  } else if (lastResult.status === "failed" && lastResult.notice !== null && lastResult.notice.action === "ask_architect_fix") {
    consoleFailure = {
      spec_id: lastResult.spec_id !== "" ? lastResult.spec_id : (specId ?? ""),
      code: lastResult.notice.code,
      detail: lastResult.detail ?? "",
      at: lastResult.finished_at ?? "",
      action: lastResult.notice.action,
    };
  }
  // O fim de um comando muda a falha guardada no servidor (gravada ou limpa): relê o estado do app.
  const finishedKey = lastResult === null ? null : `${lastResult.exec_id}:${lastResult.status}`;
  const refresh = run.actions.refresh;
  useEffect(() => {
    if (finishedKey !== null) refresh();
  }, [finishedKey, refresh]);

  const submit = (event: FormEvent): void => {
    event.preventDefault();
    const command = draft.trim();
    if (command === "" || busy || specId === null) return;
    setConfirming(command);
  };

  const problem = snap.problem === null ? null : apiFailureText(snap.problem);

  const output = (
    <div className="nd-console-out" data-console-out="">
      <Transcript entries={snap.transcript} emptyText={t("console.empty")} />
      {consoleFailure !== null ? <FailureView failure={consoleFailure} run={run} canRetry={false} onRetry={() => undefined} inline /> : null}
    </div>
  );

  const form = (
    <form className="nd-console-form" onSubmit={submit} data-console-form="">
      <label htmlFor={inputId} className="nd-sr-only">
        {t("console.input")}
      </label>
      <input
        ref={inputRef}
        id={inputId}
        className="nd-input nd-mono nd-console-input"
        value={draft}
        placeholder={t("console.placeholder")}
        autoComplete="off"
        autoCapitalize="off"
        spellCheck={false}
        aria-describedby={limitsId}
        onChange={(e) => {
          setDraft(e.target.value);
          if (snap.problem !== null) actions.clearProblem();
        }}
      />
      {snap.active !== null ? (
        <button type="button" className="nd-btn" data-variant="danger" disabled={snap.stopping} aria-busy={snap.stopping} data-console-stop="" onClick={actions.stop}>
          <Icon name="stop" size={14} />
          {t(snap.stopping ? "run.stopping" : "console.stop")}
        </button>
      ) : (
        <button type="submit" className="nd-btn" data-variant="primary" disabled={busy || specId === null || draft.trim() === ""} data-console-run="">
          <Icon name="play" size={14} />
          {t("console.run")}
        </button>
      )}
    </form>
  );

  const limits = (
    <p id={limitsId} className="nd-runview-muted nd-console-limits">
      {tp("console.limits", minutes)}
    </p>
  );

  const problemLine =
    problem !== null ? (
      <p role="alert" className="nd-runview-error" data-console-problem={snap.problem?.code ?? ""}>
        {problem}
      </p>
    ) : null;

  const history = (
    <section className="nd-runview-card" aria-label={t("console.history")} data-console-history="">
      <h3 className="nd-runview-label">{t("console.history")}</h3>
      {snap.history.length === 0 ? (
        <p className="nd-runview-muted">{t("console.historyEmpty")}</p>
      ) : (
        <ul className="nd-runview-plain nd-console-history">
          {snap.history.slice(0, HISTORY_SHOWN).map((entry) => {
            const at = Date.parse(entry.at);
            const time = Number.isNaN(at) ? "" : formatTime(at);
            const origin = entry.origin.deviceName !== null ? t("console.origin.device", { device: entry.origin.deviceName, time }) : entry.origin.loopback ? t("console.origin.local", { time }) : time;
            return (
              <li key={entry.exec_id}>
                <button
                  type="button"
                  className="nd-btn nd-btn-sm nd-console-history-btn"
                  data-variant="ghost"
                  aria-label={t("console.history.use", { command: entry.command })}
                  disabled={entry.redacted}
                  title={entry.redacted ? t("console.redacted") : undefined}
                  onClick={() => {
                    setDraft(entry.command);
                    setTab("output");
                    inputRef.current?.focus();
                  }}
                >
                  <span className="nd-mono nd-console-history-cmd">{entry.command}</span>
                </button>
                <span className="nd-runview-small nd-runview-muted">{origin}</span>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );

  const files =
    lastFiles !== null && (lastFiles.files.length > 0 || lastFiles.files_total > 0) ? (
      <ChangedFilesList projectId={projectId} files={lastFiles.files} total={lastFiles.files_total} titleKey={compact ? "console.files" : "runview.files.title"} />
    ) : null;

  const head = (
    <div className="nd-runview-head" data-runview-head="">
      <h2 className="nd-runview-title nd-console-title">{t("console.title")}</h2>
      <span className="nd-runview-muted nd-console-sub">{t("console.subtitle", { spec: specId ?? "" })}</span>
      <span className="nd-runview-spacer" />
      <div className="nd-runview-actions">{headAction}</div>
    </div>
  );

  const noRunLine =
    noRun !== null ? (
      <div className="nd-console-norun" data-console-norun="">
        <p className="nd-runview-text">{t("console.noRun")}</p>
        {noRun.decision !== null ? (
          <p className="nd-runview-muted" data-console-decision="">
            {noRun.decision}
          </p>
        ) : null}
      </div>
    ) : null;

  const dialog =
    confirming !== null ? (
      <ConfirmCommandDialog
        command={confirming}
        onCancel={() => setConfirming(null)}
        onConfirm={() => {
          actions.run(confirming);
          setConfirming(null);
          setDraft("");
        }}
      />
    ) : null;

  if (compact) {
    const tabs: readonly { id: Tab; label: string }[] = [
      { id: "output", label: t("console.tab.output") },
      { id: "howto", label: t("runview.howTo") },
      { id: "proofs", label: t("console.tab.proofs") },
    ];
    return (
      <section className="nd-runview nd-console" aria-label={t("console.title")} data-console="" data-compact="true">
        {head}
        {noRunLine}
        <div role="group" aria-label={t("console.tabs")} className="nd-segment nd-chrome nd-console-tabs">
          {tabs.map((item) => (
            <button key={item.id} type="button" className="nd-segment-item" aria-pressed={tab === item.id} data-console-tab={item.id} onClick={() => setTab(item.id)}>
              <span>{item.label}</span>
            </button>
          ))}
        </div>
        <div className="nd-console-pane">
          {tab === "output" ? (
            <>
              {output}
              {files}
              {history}
            </>
          ) : tab === "howto" ? (
            <HowToUseCard guide={guide} />
          ) : (
            <ProofsCard projectId={projectId} specId={specId} guide={guide} />
          )}
        </div>
        {problemLine}
        {form}
        {limits}
        {dialog}
      </section>
    );
  }

  return (
    <section className="nd-runview nd-console" aria-label={t("console.title")} data-console="">
      {head}
      {noRunLine}
      <div className="nd-runview-split">
        <div className="nd-runview-main nd-console-main">
          {output}
          {problemLine}
          {form}
          {limits}
        </div>
        <div className="nd-runview-side">
          <HowToUseCard guide={guide} />
          <ProofsCard projectId={projectId} specId={specId} guide={guide} />
          {files}
          {history}
        </div>
      </div>
      <p className="nd-runview-foot">{t("console.foot")}</p>
      {dialog}
    </section>
  );
}
