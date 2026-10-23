/*
 * Mensagem do Arquiteto "Execução pausada" ou "Execução parada" na Conversa, em cartão (095/T04:
 * cabeçalho do Arquiteto, corpo e rodapé com "Retomar"/"Continuar"): o que aconteceu (contexto da escalada), as tarefas bloqueadas,
 * as opções de escalada (com confirmação), a orientação livre e "Retomar"/"Continuar".
 * Retomar e escolher exigem `readiness.execute` (spec 093, decisão 15): sem ele, os botões ficam
 * desabilitados com a lista do que falta; o 412 `prereq_missing` também é tratado.
 */
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { chooseRecovery, resumeSpec } from "../../api/client";
import type { RecoveryOption, SpecView, TaskView } from "../../api/types";
import { RECOVERY_GUIDANCE_MAX_BYTES } from "../../copy";
import { Icon } from "../../desktop/Icon";
import { localized } from "../../i18n";
import { useT } from "../../i18n/react";
import { useShell } from "../../desktop/shellContext";
import { parseEscalation, type EscalationContext } from "../../escalation";
import { localizeAttempted, localizeRecoveryAnalysis, localizeRecoveryCause } from "../../recoveryAnalysis";
import { recoveryKey, useAppStore, type ChatMessage } from "../../store";
import { gateBlocked } from "../shared/readiness";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import { approveProblem, recoveryProblem, type RunProblem } from "../shared/runProblems";
import { ChatCard, ChatCardHead } from "./ChatCard";
import { runStopCode } from "../board/boardModel";
import { PLAN_USAGE_URL, planConnectionToReconnect } from "../settings/chatgptPlan";
import { useChatgptStart } from "../settings/PlanConnection";

/** Rótulo de cada tipo de opção de escalada, no idioma ativo. */
export const ESCALATION_KIND_LABELS: Readonly<Record<string, string>> = localized({
  config: "recovery.kind.config",
  environment: "recovery.kind.environment",
  guidance: "recovery.kind.guidance",
  revise_task: "recovery.kind.revise_task",
});

function blockedTasks(spec: SpecView): TaskView[] {
  return spec.tasks.filter((task) => task.status === "BLOCKED");
}

/** Última mensagem do Arquiteto no diálogo daquela (spec, tarefa). */
function lastAnalysis(messages: readonly ChatMessage[] | undefined, specId: string, taskId: string, onlyRecovery = false): string | null {
  if (messages === undefined) return null;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const m = messages[i];
    if (m !== undefined && m.role === "agent" && m.agent === "architect" && m.taskId === taskId && m.specId === specId) {
      if (onlyRecovery && !m.text.startsWith("Análise de recuperação")) continue;
      return m.text;
    }
  }
  return null;
}

function Block({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="nd-stack-xs">
      <h4 className="nd-section-label">{title}</h4>
      {children}
    </section>
  );
}

function EscalationDetails({ ctx }: { ctx: EscalationContext }) {
  const { t, lang } = useT();
  // As marcas e as causas fixas do servidor vêm em pt-BR: viram o idioma ativo (spec 115 T04).
  const cause = ctx.rootCause !== null ? localizeRecoveryCause(ctx.rootCause, lang) : localizeRecoveryAnalysis(ctx.summary, lang);
  return (
    <div className="nd-stack-sm nd-muted">
      {cause !== "" ? (
        <Block title={t("escalation.whatHappened")}>
          <p className="nd-prewrap">{cause}</p>
        </Block>
      ) : null}
      {ctx.command !== null || ctx.code !== null ? (
        <Block title={t("escalation.command")}>
          {ctx.command !== null ? <p className="nd-mono nd-break">{ctx.command}</p> : null}
          {ctx.code !== null ? <p>{t("escalation.exitCode", { code: ctx.code })}</p> : null}
        </Block>
      ) : null}
      {ctx.excerpt !== null ? (
        <details className="nd-details">
          <summary>
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {t("escalation.excerpt")}
          </summary>
          <pre className="nd-md-pre">{ctx.excerpt}</pre>
        </details>
      ) : null}
      {ctx.attempted !== null ? (
        <Block title={t("escalation.attempted")}>
          <p className="nd-break">{localizeAttempted(ctx.attempted, lang)}</p>
        </Block>
      ) : null}
      {ctx.whyStopped !== null ? (
        <Block title={t("escalation.whyStopped")}>
          <p className="nd-prewrap">{localizeRecoveryCause(ctx.whyStopped, lang)}</p>
        </Block>
      ) : null}
      {ctx.options.length > 0 ? (
        <Block title={t("escalation.options")}>
          <ol className="nd-ordered">
            {ctx.options.map((o, i) => (
              <li key={i} className="nd-break">
                {o}
              </li>
            ))}
          </ol>
        </Block>
      ) : null}
    </div>
  );
}

function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

function EnvironmentFiles({ option }: { option: RecoveryOption }) {
  const [open, setOpen] = useState(false);
  const { t } = useT();
  const panelId = useId();
  const files = option.files ?? [];
  const body = option.diff !== undefined && option.diff !== "" ? option.diff : files.map((f) => `# ${f.path}\n${f.content}`).join("\n\n");
  if (files.length === 0 && body === "") return null;
  return (
    <div className="nd-stack-xs">
      <button type="button" className="nd-btn nd-btn-sm" aria-expanded={open} aria-controls={open ? panelId : undefined} onClick={() => setOpen((v) => !v)}>
        <Icon name={open ? "chevron-down" : "chevron-right"} size={14} />
        {t("recovery.filesToggle")}
        {files.length > 0 ? ` (${files.length})` : ""}
      </button>
      {open ? (
        <pre id={panelId} className="nd-md-pre">
          {body}
        </pre>
      ) : null}
    </div>
  );
}

function OptionBody({ option }: { option: RecoveryOption }) {
  return (
    <div className="nd-stack-xs nd-muted">
      {option.kind === "environment" && option.files !== undefined ? <EnvironmentFiles option={option} /> : null}
      {option.kind === "config" && option.diff !== undefined ? <pre className="nd-md-pre">{option.diff}</pre> : null}
      {option.kind === "revise_task" ? (
        <div className="nd-stack-xs">
          {option.objective !== undefined ? <p className="nd-break">{option.objective}</p> : null}
          {option.acceptance !== undefined && option.acceptance.length > 0 ? (
            <ul className="nd-bullets">
              {option.acceptance.map((a, i) => (
                <li key={i} className="nd-break">
                  {a}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}
      {(option.kind === "guidance" || option.kind === "environment") && option.text !== undefined ? <p className="nd-prewrap">{option.text}</p> : null}
    </div>
  );
}

interface Props {
  projectId: string;
  spec: SpecView;
  onResumed: () => Promise<void>;
}

export function PausedRunCard({ projectId, spec, onResumed }: Props) {
  const shell = useShell();
  const { t } = useT();
  const titleId = useId();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<RunProblem | null>(null);
  const inFlight = useRef(false);
  const blocked = blockedTasks(spec);
  const stalled = blocked.length === 0;
  const blockKind = useAppStore((s) => s.blockKinds[projectId]);
  const recoveries = useAppStore((s) => s.recoveries);
  const messages = useAppStore((s) => s.messages[projectId]);
  const kind = blockKind !== undefined && blocked.some((t) => t.id === blockKind.taskId) ? blockKind.kind : undefined;
  const environmental = kind === "environmental";
  const escalatedTask = kind === "quality" ? blocked.find((t) => recoveries[recoveryKey(projectId, spec.id, t.id)]?.phase === "escalate") : undefined;
  const actionTask =
    escalatedTask ??
    blocked.find((t) => (t.recovery_options?.length ?? 0) > 0) ??
    blocked.find((t) => lastAnalysis(messages, spec.id, t.id, true) !== null) ??
    blocked[0];
  const analysis = actionTask === undefined ? null : lastAnalysis(messages, spec.id, actionTask.id, escalatedTask === undefined);
  const escalatedEntry = actionTask === undefined ? undefined : recoveries[recoveryKey(projectId, spec.id, actionTask.id)];
  const options: RecoveryOption[] = actionTask === undefined ? [] : (escalatedEntry?.options ?? actionTask.recovery_options ?? []);
  const optionsAnalysis = escalatedEntry?.options_analysis ?? actionTask?.recovery_options_analysis;
  const [pending, setConfirming] = useState<{ id: string; analysis: number; taskId: string } | null>(null);
  const confirming = pending !== null && pending.analysis === optionsAnalysis && pending.taskId === actionTask?.id ? pending : null;
  useEffect(() => {
    setConfirming(null);
  }, [optionsAnalysis, actionTask?.id]);
  const [guidance, setGuidance] = useState("");
  const guidanceId = useId();
  const guidanceBytes = byteLength(guidance);
  const [otherOpen, setOtherOpen] = useState(false);
  const hasOptions = options.length > 0 && optionsAnalysis !== undefined;
  const guidanceShown = !hasOptions || otherOpen;
  const guidanceTooLong = guidanceBytes > RECOVERY_GUIDANCE_MAX_BYTES;
  const blockedByReadiness = gateBlocked(shell.readiness, "execute");
  // Pausa pelo plano ChatGPT (spec 116 T03): limite de uso ou novo login, com o cartão próprio.
  const stopCode = useAppStore((s) => (stalled ? runStopCode(s.messages[projectId], spec.id, s.runStarts[projectId]) : null));
  const planPause = stopCode === "run.plan_limit" ? "limit" : stopCode === "run.plan_needs_login" ? "sign_in" : null;
  const reconnectTarget = useAppStore((s) => (planPause === "sign_in" ? (planConnectionToReconnect(s.config?.agents, s.config?.connections)?.id ?? null) : null));
  const reconnect = useChatgptStart(reconnectTarget ?? undefined);
  const locked = busy || blockedByReadiness;

  function fail(next: RunProblem): void {
    setProblem(next);
    if (next.missing !== null) void shell.reloadReadiness();
  }

  async function choose(body: { option_id: string; analysis: number } | { guidance: string }): Promise<void> {
    if (inFlight.current || actionTask === undefined) return;
    inFlight.current = true;
    setBusy(true);
    setProblem(null);
    const result = await chooseRecovery(projectId, spec.id, actionTask.id, body);
    if (result.ok) {
      setConfirming(null);
      setGuidance("");
      shell.refreshAttention();
      await onResumed();
    } else {
      fail(recoveryProblem(result.error, shell.readiness));
    }
    inFlight.current = false;
    setBusy(false);
  }

  async function resume(): Promise<void> {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setProblem(null);
    const result = await resumeSpec(projectId, spec.id);
    if (result.ok) {
      shell.refreshAttention();
      await onResumed();
    } else {
      fail(approveProblem(result.error, shell.readiness));
    }
    inFlight.current = false;
    setBusy(false);
  }

  const parsed = analysis === null ? null : parseEscalation(analysis);
  const resumeLabel = busy
    ? stalled
      ? t("paused.continuing")
      : t("paused.resuming")
    : stalled
      ? t("run.continue")
      : escalatedTask !== undefined
        ? t("recovery.resumeManually")
        : t("run.resume");

  return (
    <ChatCard as="article" aria-labelledby={titleId} tone="amber" data-paused-card="">
      <ChatCardHead
        agent="architect"
        nameId={titleId}
        extra={blocked.map((task) => (
          <span key={task.id} className="nd-task-tag nd-mono">
            {task.id}
          </span>
        ))}
      />
      <div className="nd-chat-card-body nd-stack">
        <h3 className="nd-callout-title" data-plan-pause={planPause ?? undefined}>
          {planPause === "limit"
            ? t("paused.planLimitTitle")
            : planPause === "sign_in"
              ? t("paused.planLoginTitle")
              : stalled
                ? t("paused.stalledTitle")
                : environmental
                  ? t("envBlock.title")
                  : t("paused.title")}
        </h3>
        {planPause === "limit" ? (
          <p className="nd-muted">{t("paused.planLimitHint")}</p>
        ) : planPause === "sign_in" ? (
          <p className="nd-muted">{t("paused.planLoginHint")}</p>
        ) : stalled ? (
          <p className="nd-muted">{t("paused.stalledHint")}</p>
        ) : environmental ? (
          <p className="nd-muted">{t("envBlock.hint")}</p>
        ) : (
          <p className="nd-muted">{t("paused.hint")}</p>
        )}

        {escalatedTask !== undefined || analysis !== null ? (
          <div className="nd-stack-sm">
            <p className="nd-strong">{t("recovery.escalatedTitle")}</p>
            {analysis !== null ? parsed !== null ? <EscalationDetails ctx={parsed} /> : <p className="nd-prewrap nd-muted">{analysis}</p> : null}
          </div>
        ) : null}

        <ul className="nd-plain-list">
          {blocked.map((task) => (
            <li key={task.id} className="nd-stack-xs">
              <span className="nd-error-inline">
                <span className="nd-mono">{task.id}</span> {t("paused.blocked")}
              </span>
              {task.next_action !== undefined && task.next_action !== "" ? <span className="nd-muted">{task.next_action}</span> : null}
            </li>
          ))}
        </ul>

        {blockedByReadiness && shell.readiness !== null ? (
          <ReadinessNotice title={t("paused.readinessTitle")} missing={shell.readiness.execute.missing} />
        ) : null}

        {actionTask !== undefined ? (
          <div className="nd-stack">
            {blocked.length > 1 ? (
              <p className="nd-muted">
                {t("paused.for")} <span className="nd-mono">{actionTask.id}</span>
              </p>
            ) : null}
            {hasOptions ? (
              <section aria-label={t("recovery.optionsTitle")} className="nd-stack-sm">
                <h4 className="nd-section-label">{t("recovery.optionsTitle")}</h4>
                <ul className="nd-plain-list nd-stack-sm">
                  {options.map((o) => (
                    <li key={o.id} className="nd-card nd-option-card">
                      <p className="nd-strong">{o.label}</p>
                      <p className="nd-muted">{ESCALATION_KIND_LABELS[o.kind] ?? o.kind}</p>
                      <OptionBody option={o} />
                      {confirming?.id === o.id ? (
                        <div className="nd-stack-sm">
                          {o.kind === "environment" && o.files !== undefined ? <p className="nd-dialog-warning">{t("recovery.envWarning")}</p> : null}
                          <div className="nd-actions">
                            <button
                              type="button"
                              className="nd-btn"
                             
                              autoFocus
                              disabled={locked}
                              aria-busy={busy}
                              onClick={() => void choose({ option_id: o.id, analysis: confirming.analysis })}
                            >
                              {busy ? t("paused.applying") : t("recovery.confirm")}
                            </button>
                            <button type="button" className="nd-btn" disabled={busy} onClick={() => setConfirming(null)}>
                              {t("common.cancel")}
                            </button>
                          </div>
                        </div>
                      ) : (
                        <button
                          type="button"
                          className="nd-btn"
                         
                          disabled={locked}
                          onClick={() => {
                            if (optionsAnalysis === undefined) return;
                            setProblem(null);
                            setConfirming({ id: o.id, analysis: optionsAnalysis, taskId: actionTask.id });
                          }}
                        >
                          {t("paused.choose", { label: o.label })}
                        </button>
                      )}
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}
            {hasOptions && !otherOpen ? (
              <button type="button" className="nd-btn" aria-expanded={false} disabled={busy} onClick={() => setOtherOpen(true)}>
                {t("recovery.otherPath")}
              </button>
            ) : null}
            {guidanceShown ? (
              <div className="nd-field">
                <label htmlFor={guidanceId} className="nd-field-label">
                  {t("recovery.guidanceLabel")}
                </label>
                <textarea
                  id={guidanceId}
                  value={guidance}
                  onChange={(e) => setGuidance(e.target.value)}
                  rows={3}
                  autoFocus={hasOptions}
                  disabled={busy}
                  className="nd-input nd-textarea"
                />
                <div className="nd-row nd-wrap">
                  <span className={guidanceTooLong ? "nd-field-error" : "nd-muted"}>
                    {guidanceBytes}/{RECOVERY_GUIDANCE_MAX_BYTES}
                  </span>
                  <button
                    type="button"
                    className="nd-btn"
                    disabled={locked || guidance.trim() === "" || guidanceTooLong}
                    aria-busy={busy}
                    onClick={() => void choose({ guidance: guidance.trim() })}
                  >
                    {busy ? t("paused.applying") : t("recovery.guidanceSend")}
                  </button>
                </div>
              </div>
            ) : null}
          </div>
        ) : null}
      </div>
      <div className="nd-chat-card-foot nd-stack-sm">
        <div className="nd-actions">
          {planPause === "limit" ? (
            <a className="nd-btn" href={PLAN_USAGE_URL} target="_blank" rel="noopener noreferrer" data-plan-usage="">
              <Icon name="external" size={14} />
              {t("chatgpt.manageUsage")}
            </a>
          ) : null}
          {planPause === "sign_in" ? (
            <button
              type="button"
              className="nd-btn"
              data-variant="primary"
              data-plan-reconnect=""
              disabled={reconnect.busy}
              aria-busy={reconnect.busy}
              onClick={() => (reconnectTarget !== null ? reconnect.start() : shell.openSettings("connections"))}
            >
              <Icon name="refresh" size={14} />
              {reconnect.busy ? t("chatgpt.opening") : t("chatgpt.reconnect")}
            </button>
          ) : null}
          <button type="button" className="nd-btn" onClick={() => void resume()} disabled={locked} aria-busy={busy} data-resume="">
            <Icon name="play" size={14} />
            {resumeLabel}
          </button>
        </div>
        {reconnect.notice}
        {problem !== null ? <ProblemNotice message={problem.message} missing={problem.missing} prereq={problem.prereq} /> : null}
      </div>
    </ChatCard>
  );
}
