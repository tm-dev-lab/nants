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
import { isRecoveryAnalysis, localizeAttempted, localizeRecoveryAnalysis, localizeRecoveryCause } from "../../recoveryAnalysis";
import { recoveryKey, useAppStore, type ChatMessage } from "../../store";
import { gateBlocked } from "../shared/readiness";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import { approveProblem, recoveryProblem, type RunProblem } from "../shared/runProblems";
import { ChatCard, ChatCardHead } from "./ChatCard";
import { runStopMessage } from "../board/boardModel";
import { PROVIDER_STOP_CODES, providerEvidence } from "../../i18n/providerEvidence";
import { ProviderEvidenceDetails } from "../shared/ProviderEvidenceDetails";
import { PLAN_USAGE_URL, planConnectionToReconnect } from "../settings/chatgptPlan";
import { useChatgptStart } from "../settings/PlanConnection";
import { putAgents } from "../../api/client";
import { apiFailureText } from "../../i18n/serverCodes";
import { ANTHROPIC_LIMITS_URL, ROLE_INFO } from "../../settings/rules";
import { appStore } from "../../store";
import { initialChoice, RoleConnectionSwitch, RoleFacts, type RoleConnectionChoice } from "../shared/RoleConnectionSwitch";
import { providerPage, readyConnections, roleOf, withRoleConnection } from "../shared/roleConnection";

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
      if (onlyRecovery && !isRecoveryAnalysis(m.text)) continue;
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
      {ctx.question !== null ? (
        <Block title={t("escalation.question")}>
          <p className="nd-prewrap nd-strong" data-recovery-question="">
            {ctx.question}
          </p>
        </Block>
      ) : null}
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

/** Marcador de lista no início de uma linha de critério: `- `, `* `, `• `, `1. ` ou `1) `. */
const BULLET = /^(?:[-*\u2022]|\d{1,3}[.)])\s+/;

/**
 * Critérios de uma opção `revise_task` como lista. O servidor manda um texto com uma linha por
 * critério (spec 073); a lista é aceita por compatibilidade. Qualquer outra forma vira lista vazia.
 */
function acceptanceLines(value: unknown): string[] {
  const lines = typeof value === "string" ? value.split(/\r?\n/) : Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
  return lines.map((l) => l.trim().replace(BULLET, "").trim()).filter((l) => l.length > 0);
}

function OptionBody({ option }: { option: RecoveryOption }) {
  const acceptance = option.kind === "revise_task" ? acceptanceLines(option.acceptance) : [];
  return (
    <div className="nd-stack-xs nd-muted">
      {option.kind === "environment" && option.files !== undefined ? <EnvironmentFiles option={option} /> : null}
      {option.kind === "config" && option.diff !== undefined ? <pre className="nd-md-pre">{option.diff}</pre> : null}
      {option.kind === "revise_task" ? (
        <div className="nd-stack-xs">
          {option.objective !== undefined ? <p className="nd-break">{option.objective}</p> : null}
          {acceptance.length > 0 ? (
            <ul className="nd-bullets">
              {acceptance.map((a, i) => (
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
  const { t, lang } = useT();
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
  const stopMessage = useAppStore((s) => (stalled ? runStopMessage(s.messages[projectId], spec.id, s.runStarts[projectId]) : null));
  const stopCode = stopMessage?.code ?? null;
  // Spec 133 T01: a parada pelo provedor diz o papel, a conexão, o modelo e o motivo.
  const providerStop = stopMessage !== null && stopCode !== null && PROVIDER_STOP_CODES.has(stopCode) ? providerEvidence(stopMessage.params, lang) : null;
  const providerFailed = stopCode === "run.provider_failed" || stopCode === "run.provider_failed_detail";
  const planPause = stopCode === "run.plan_limit" ? "limit" : stopCode === "run.plan_needs_login" ? "sign_in" : null;
  // Limites do provedor que pausam (spec 132 T09/T10), no mesmo padrão: o teto de gasto da Anthropic
  // e a cota diária do Gemini.
  const limitPause = stopCode === "run.spend_limit" ? "spend" : stopCode === "run.daily_quota" ? "quota" : stopCode === "run.billing" ? "billing" : null;
  // Spec 133 T02: o papel que parou, com a troca da conexão só dele ali mesmo.
  const stopRole = providerStop !== null && planPause !== "sign_in" ? roleOf(stopMessage?.params?.role) : null;
  const agents = useAppStore((s) => s.config?.agents ?? null);
  const connections = useAppStore((s) => s.config?.connections ?? null);
  const [choice, setChoice] = useState<RoleConnectionChoice | null>(null);
  // A escolha começa de novo a cada parada (outro papel ou outra mensagem), não a cada gravação.
  const choiceKey = stopRole === null || stopMessage === null ? null : `${stopMessage.id}:${stopRole}`;
  const configLoaded = agents !== null && connections !== null;
  useEffect(() => {
    setChoice(stopRole !== null && agents !== null && connections !== null ? initialChoice(stopRole, agents, connections) : null);
  }, [choiceKey, configLoaded]);
  const otherReady = stopRole !== null && agents !== null && connections !== null && readyConnections(connections).some((c) => c.id !== agents[stopRole].connection_id);
  const stopParams = stopMessage?.params ?? {};
  const stopConnection = connections?.find((c) => c.id === stopParams.connection_id) ?? null;
  const page = stopRole === null || (limitPause === null && planPause !== "limit") ? null : providerPage(stopConnection, stopParams.connection_kind, limitPause === "billing" ? "billing" : "limits");
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

  /** Grava a nova conexão (e o modelo) só do papel que parou e continua a execução. */
  async function switchAndContinue(): Promise<void> {
    if (inFlight.current || stopRole === null || agents === null || choice === null) return;
    inFlight.current = true;
    setBusy(true);
    setProblem(null);
    const saved = await putAgents(withRoleConnection(agents, stopRole, choice.connectionId, choice.model));
    inFlight.current = false;
    if (!saved.ok) {
      setBusy(false);
      setProblem({ message: apiFailureText(saved.error), missing: null });
      return;
    }
    appStore.setState((s) => ({ config: s.config === null ? null : { ...s.config, agents: saved.data } }));
    setBusy(false);
    await resume();
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

  const canSwitch = stopRole !== null && otherReady;
  const choiceChanged =
    choice !== null && agents !== null && stopRole !== null && choice.model.trim() !== "" && (choice.connectionId !== agents[stopRole].connection_id || choice.model !== agents[stopRole].model);
  // Nos limites (cobrança, teto, cota, plano), trocar a conexão é a saída principal; na falha do
  // provedor, Continuar.
  const switchFirst = canSwitch && !providerFailed;
  const resumeButton = (
    <button
      type="button"
      className="nd-btn"
      data-variant={canSwitch && !switchFirst ? "primary" : undefined}
      onClick={() => void resume()}
      disabled={locked}
      aria-busy={busy}
      data-resume=""
    >
      <Icon name="play" size={14} />
      {switchFirst && !busy ? t("roleConn.continueWithout") : resumeLabel}
    </button>
  );


  return (
    <ChatCard as="article" aria-labelledby={titleId} tone="amber" data-paused-card="">
      <ChatCardHead
        agent={stopRole ?? "architect"}
        nameId={titleId}
        extra={
          blocked.length > 0 ? (
            blocked.map((task) => (
              <span key={task.id} className="nd-task-tag nd-mono">
                {task.id}
              </span>
            ))
          ) : stopRole !== null && typeof stopParams.task === "string" ? (
            <span className="nd-task-tag nd-mono" data-stop-task="">
              {stopParams.task}
            </span>
          ) : null
        }
      />
      <div className="nd-chat-card-body nd-stack">
        <h3 className="nd-callout-title" data-plan-pause={planPause ?? undefined} data-limit-pause={limitPause ?? undefined}>
          {planPause === "limit"
            ? t("paused.planLimitTitle")
            : planPause === "sign_in"
              ? t("paused.planLoginTitle")
              : limitPause === "spend"
                ? t("paused.spendLimitTitle")
                : limitPause === "quota"
                  ? t("paused.dailyQuotaTitle")
                  : limitPause === "billing"
                    ? t("paused.billingTitle")
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
        ) : limitPause === "spend" ? (
          <p className="nd-muted">{t("paused.spendLimitHint")}</p>
        ) : limitPause === "quota" ? (
          <p className="nd-muted">{t("paused.dailyQuotaHint")}</p>
        ) : limitPause === "billing" ? (
          <p className="nd-muted">{t("paused.billingHint")}</p>
        ) : stalled ? (
          <p className="nd-muted">{t("paused.stalledHint")}</p>
        ) : environmental ? (
          <p className="nd-muted">{t("envBlock.hint")}</p>
        ) : (
          <p className="nd-muted">{t("paused.hint")}</p>
        )}

        {providerStop !== null && stopMessage !== null ? (
          <div className="nd-stack-sm" data-provider-stop="">
            {providerFailed ? <p className="nd-strong">{t("paused.providerReason", { reason: providerStop.reason })}</p> : null}
            {stopRole !== null ? (
              <RoleFacts
                role={stopRole}
                connectionName={typeof stopParams.connection_name === "string" ? stopParams.connection_name : t("provider.evidence.noConnection")}
                connectionKind={stopParams.connection_kind}
                model={typeof stopParams.model === "string" ? stopParams.model : "?"}
              />
            ) : (
              <p className="nd-muted">{providerStop.who}</p>
            )}
            <ProviderEvidenceDetails code={stopMessage.code} params={stopMessage.params} />
          </div>
        ) : null}

        {stopRole !== null && agents !== null && connections !== null ? (
          otherReady && choice !== null ? (
            <section className="nd-stack-sm" aria-label={t("roleConn.switchTitle", { role: ROLE_INFO[stopRole].label })} data-stop-switch="">
              <h4 className="nd-section-label">{t("roleConn.switchTitle", { role: ROLE_INFO[stopRole].label })}</h4>
              <RoleConnectionSwitch role={stopRole} agents={agents} connections={connections} value={choice} onChange={setChoice} disabled={busy} />
              <p className="nd-field-hint">{t("roleConn.switchHint", { role: ROLE_INFO[stopRole].label })}</p>
            </section>
          ) : (
            <p className="nd-muted" data-no-other-connection="">
              {t("roleConn.noOther")}
            </p>
          )
        ) : null}

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
          {/* Na falha do provedor, que pode passar sozinha, Continuar é a ação principal (spec 133 T02). */}
          {switchFirst ? null : resumeButton}
          {canSwitch ? (
            <button
              type="button"
              className="nd-btn"
              data-variant={switchFirst ? "primary" : undefined}
              data-switch-continue=""
              disabled={locked || !choiceChanged}
              aria-busy={busy}
              onClick={() => void switchAndContinue()}
            >
              <Icon name="refresh" size={14} />
              {busy ? t("paused.continuing") : t("roleConn.switchAndContinue")}
            </button>
          ) : null}
          {/* Mensagens antigas, sem papel: as saídas de antes (o console da Anthropic, Agentes). */}
          {stopRole === null && limitPause === "spend" ? (
            <a className="nd-btn" href={ANTHROPIC_LIMITS_URL} target="_blank" rel="noopener noreferrer" data-provider-limits="">
              <Icon name="external" size={14} />
              {t("paused.openAnthropicLimits")}
            </a>
          ) : null}
          {stopRole === null && (limitPause === "quota" || limitPause === "billing") ? (
            <button type="button" className="nd-btn" data-switch-connection="" onClick={() => shell.openSettings("agents")}>
              <Icon name="users" size={14} />
              {t("paused.switchConnection")}
            </button>
          ) : null}
          {stopRole !== null && !otherReady ? (
            <button type="button" className="nd-btn" data-add-connection="" onClick={() => shell.openSettings("connections")}>
              <Icon name="plus" size={14} />
              {t("roleConn.addConnection")}
            </button>
          ) : null}
          {page !== null ? (
            <a className="nd-btn" href={page.url} target="_blank" rel="noopener noreferrer" data-provider-page="">
              <Icon name="external" size={14} />
              {page.label}
            </a>
          ) : planPause === "limit" ? (
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
          {switchFirst ? resumeButton : null}
        </div>
        {reconnect.notice}
        {problem !== null ? <ProblemNotice message={problem.message} missing={problem.missing} prereq={problem.prereq} /> : null}
      </div>
    </ChatCard>
  );
}
