/*
 * Plano da spec ativa, na janela Plano, no desenho enxuto da 095/T04 (`reference/Janelas.dc.html`):
 * - título e resumo (spec e tarefas) com divisória; tarefas em lista simples (id mono `--muted`,
 *   título, "depende de" à direita) com divisória entre os itens;
 * - o que pede atenção fica na vista principal: os itens das checagens numa faixa âmbar ("O
 *   Revisor fez N apontamentos"), a revisão em curso ou a linha "nada a corrigir";
 * - o detalhe fica num `<details>` recolhido: premissas, orientações por papel, ambiente Docker,
 *   perfis de especialista, as checagens por origem e "Mudou nesta versão";
 * - rodapé separado com "Aprovar e iniciar" (a única primária) e "Pedir correção" (enviado pela
 *   Conversa), que só aparece quando as checagens têm itens; os itens nunca bloqueiam;
 * - etapa do plano (`planStage`, spec 094 decisão 3): "Planejando" = "Plano em elaboração", sem
 *   checagens nem ações; "Revisando" = Aprovar e Pedir correção desabilitados ("aguardando o
 *   Revisor"); "Aguardando" = "Aprovar e iniciar" com confirmação. Spec legada sem comando de
 *   testes pede ao Arquiteto. A reconsulta do estado durante a revisão é do espaço
 *   (`useSpaceData`), não deste cartão;
 * - prontidão (decisão 15): sem `readiness.execute`, o botão fica desabilitado com o que falta;
 *   o 412 `prereq_missing` mostra o que falta com "Configurar".
 */
import { useId, useState, type ReactNode } from "react";
import { approveSpec, postChat } from "../../api/client";
import type { EnvironmentView, PlanIssue, PlanReviewView, SpecView } from "../../api/types";
import { chatFailureMessage } from "../../chatFailure";
import { ARCHITECT_COMPLETE_PLAN_MESSAGE } from "../../copy";
import { Icon } from "../../desktop/Icon";
import { t, tp, type MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { useShell } from "../../desktop/shellContext";
import { planStage, type PlanStage } from "../../phase";
import { architectWireApi } from "../../settings/rules";
import { appStore, useAppStore } from "../../store";
import { gateBlocked } from "../shared/readiness";
import { MarkdownText } from "../shared/ReportMarkdown";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import { approveProblem, type RunProblem } from "../shared/runProblems";
import { SPEC_STATUS_LABELS } from "./summary";

/** Mensagem do usuário com todos os itens das checagens, um por linha (no idioma ativo). */
export function planReviewFixMessage(automatic: readonly string[], issues: readonly PlanIssue[]): string {
  const lines = [...automatic.map((warning) => `- ${warning}`), ...issues.map((issue) => `- ${issue.task ?? "spec"}: ${issue.text}`)];
  return [t("plan.review.fixPrefix"), ...lines].join("\n");
}

/** Título da faixa âmbar das checagens ("O Revisor fez 1 apontamento"), no idioma ativo. */
export function planWarnTitle(reviewer: number, automatic: number): string {
  const byReviewer = tp("plan.warn.reviewer", reviewer);
  const byNants = tp("plan.warn.automatic", automatic);
  if (reviewer > 0 && automatic > 0) return t("plan.warn.both", { reviewer: byReviewer, automatic: byNants });
  if (reviewer > 0) return byReviewer;
  return t("plan.warn.automaticOnly", { automatic: byNants });
}

/** Situação do perfil de especialista (vocabulário fixo do servidor), no idioma ativo; outra, como veio. */
const SKILL_STATUS_KEYS: Readonly<Record<string, MessageKey>> = {
  criado: "plan.skillStatus.criado",
  atualizado: "plan.skillStatus.atualizado",
  reaproveitado: "plan.skillStatus.reaproveitado",
};

function skillStatusLabel(status: string): string {
  const key = SKILL_STATUS_KEYS[status];
  return key === undefined ? status : t(key);
}

/** Partes do detalhe do plano (premissas, orientações...), pela chave do rótulo. */
type MorePart = "premises" | "guidance" | "environment" | "skills" | "checks" | "changes";

const MORE_PART_KEYS: Readonly<Record<MorePart, MessageKey>> = {
  premises: "plan.part.premises",
  guidance: "plan.part.guidance",
  environment: "plan.part.environment",
  skills: "plan.part.skills",
  checks: "plan.part.checks",
  changes: "plan.part.changes",
};

/** Rótulo do `<details>` com as partes presentes ("Premissas, ambiente e perfis"), já traduzidas. */
export function planMoreLabel(parts: readonly string[]): string {
  if (parts.length === 0) return "";
  const last = parts[parts.length - 1] ?? "";
  const text = parts.length === 1 ? last : t("plan.list.and", { list: parts.slice(0, -1).join(", "), last });
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function Pending({ children, id }: { children: ReactNode; id?: string }) {
  return (
    <p role="status" className="nd-muted nd-row" {...(id !== undefined ? { id } : {})}>
      <span aria-hidden="true" className="nd-dot" data-tone="violet" data-pulse="true" />
      {children}
    </p>
  );
}

function ReviewerStatus({ review }: { review: PlanReviewView }) {
  const { t } = useT();
  if (review.status === "running" || review.status === "pending") return <Pending>{t("plan.review.running")}</Pending>;
  if (review.status === "stale") return <p className="nd-muted">{t("plan.review.stale")}</p>;
  if (review.status === "unavailable") return <p className="nd-muted">{t("plan.review.unavailable")}</p>;
  return <p className="nd-muted">{t("plan.review.clean")}</p>;
}

/** Situação curta do Revisor no detalhe (a frase completa fica na vista principal). */
function reviewerDetail(review: PlanReviewView, issues: number): string {
  if (issues > 0) return tp("plan.review.issuesAbove", issues);
  if (review.status === "running" || review.status === "pending") return t("plan.review.reviewing");
  if (review.status === "stale") return t("plan.review.noAnswer");
  if (review.status === "unavailable") return t("plan.review.unavailableShort");
  return t("plan.review.clean");
}

/** Itens das checagens e a situação da revisão (o mesmo cálculo para a vista, o detalhe e o rodapé). */
interface Checks {
  automatic: readonly string[];
  issues: readonly PlanIssue[];
  review: PlanReviewView | undefined;
  total: number;
  reviewDone: boolean;
}

function checksOf(spec: SpecView): Checks {
  const automatic = [...(spec.scope_warnings ?? []), ...(spec.coverage_warnings ?? [])];
  const review = spec.plan_review;
  const issues = review?.status === "ok" ? review.issues : [];
  return { automatic, issues, review, total: automatic.length + issues.length, reviewDone: review === undefined || review.status === "ok" };
}

/**
 * Vista principal das checagens: a faixa âmbar com os itens por origem; sem itens, a revisão em
 * curso ou uma linha discreta (sem resposta, indisponível, nada a corrigir). Nunca bloqueia.
 */
function PlanChecksSummary({ checks }: { checks: Checks }) {
  const { t } = useT();
  const titleId = useId();
  const { automatic, issues, review, total, reviewDone } = checks;
  if (total === 0) {
    if (review !== undefined && review.status !== "ok") return <ReviewerStatus review={review} />;
    return (
      <p data-testid="plan-checks-clean" className="nd-muted">
        {t("plan.checks.clean")}
      </p>
    );
  }
  const reviewerShown = review !== undefined && (issues.length > 0 || !reviewDone);
  const both = reviewerShown && automatic.length > 0;
  return (
    <section aria-label={t("plan.checks.title")} aria-describedby={titleId} className="nd-plan-warn" data-plan-warn="">
      <Icon name="alert" size={16} className="nd-plan-warn-icon" />
      <div className="nd-plan-warn-text">
        <h3 id={titleId} className="nd-plan-warn-title">
          {planWarnTitle(issues.length, automatic.length)}
        </h3>
        {reviewerShown ? (
          <section aria-label={t("role.reviewer")} className="nd-stack-xs">
            {both ? <h4 className="nd-plan-warn-source">{t("role.reviewer")}</h4> : null}
            {issues.length > 0 ? (
              <ul className="nd-plan-warn-list">
                {issues.map((issue, i) => (
                  <li key={`${i}-${issue.text}`} className="nd-break">
                    <span className="nd-mono">{issue.task ?? "spec"}</span>: {issue.text}
                  </li>
                ))}
              </ul>
            ) : (
              <ReviewerStatus review={review} />
            )}
          </section>
        ) : null}
        {automatic.length > 0 ? (
          <section aria-label={t("plan.checks.autoTitle")} className="nd-stack-xs">
            {both ? <h4 className="nd-plan-warn-source">{t("plan.checks.autoTitle")}</h4> : null}
            <ul className="nd-plan-warn-list">
              {automatic.map((warning, i) => (
                <li key={`${i}-${warning}`} className="nd-break">
                  {warning}
                </li>
              ))}
            </ul>
          </section>
        ) : null}
        <p className="nd-plan-warn-note">{t("plan.checks.notBlocking")}</p>
      </div>
    </section>
  );
}

/** Checagens no detalhe: o que são e a situação de cada origem (os itens ficam na faixa). */
function PlanChecksDetail({ checks }: { checks: Checks }) {
  const { t, tp } = useT();
  const headingId = useId();
  const { automatic, issues, review } = checks;
  return (
    <section aria-labelledby={headingId} className="nd-plan-more-section" data-plan-checks-detail="">
      <h3 id={headingId} className="nd-plan-heading">
        {t("plan.checks.about")}
      </h3>
      <p className="nd-muted">{t("plan.checks.hint")}</p>
      <dl className="nd-facts">
        <dt>{t("plan.checks.autoTitle")}</dt>
        <dd>{automatic.length > 0 ? tp("plan.checks.autoWarnings", automatic.length) : t("plan.checks.autoClean")}</dd>
        {review !== undefined ? (
          <>
            <dt>{t("role.reviewer")}</dt>
            <dd>{reviewerDetail(review, issues.length)}</dd>
          </>
        ) : null}
      </dl>
    </section>
  );
}

function PlanChangesSection({ lines }: { lines: readonly string[] }) {
  const { t } = useT();
  return (
    <section aria-label={t("plan.changesTitle")} className="nd-plan-more-section">
      <h3 className="nd-plan-heading">
        {t("plan.changesTitle")} <span className="nd-muted">({lines.length})</span>
      </h3>
      <ul className="nd-bullets">
        {lines.map((line, i) => (
          <li key={`${i}-${line}`} className="nd-break">
            {line}
          </li>
        ))}
      </ul>
    </section>
  );
}

function EnvironmentSection({ env }: { env: EnvironmentView }) {
  const { t } = useT();
  const headingId = useId();
  return (
    <section aria-labelledby={headingId} className="nd-plan-more-section">
      <h3 id={headingId} className="nd-plan-heading">
        {t("plan.env.title")}
      </h3>
      <dl className="nd-facts">
        <dt>{t("plan.env.mode")}</dt>
        <dd>{t("plan.env.modeDocker")}</dd>
        <dt>{t("plan.env.services")}</dt>
        <dd>{env.services.join(", ")}</dd>
        {env.base_image !== undefined ? (
          <>
            <dt>{t("plan.env.baseImage")}</dt>
            <dd>{env.base_image}</dd>
          </>
        ) : null}
      </dl>
      {env.files.map((file) => (
        <details key={file.path} className="nd-details">
          <summary className="nd-mono">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {file.path}
          </summary>
          <pre className="nd-md-pre">{file.content}</pre>
        </details>
      ))}
    </section>
  );
}

interface Props {
  projectId: string;
  spec: SpecView;
  /** Recarrega o estado (depois de aprovar, ou enquanto a revisão do plano espera). */
  onApproved: () => Promise<void>;
  /** Envia na Conversa o pedido de correção das checagens do plano. */
  onRequestFix?: (text: string) => void;
  fixDisabled?: boolean;
  /**
   * Etapa do plano do espaço (`SpaceData.planStage`). Sem ela (cartão avulso), a mesma
   * `planStage` é aplicada ao cartão, sem execução.
   */
  stage?: PlanStage | null;
}

export function PlanCard({ projectId, spec, onApproved, onRequestFix, fixDisabled = false, stage: stageProp }: Props) {
  const shell = useShell();
  const { t, tp } = useT();
  const titleId = useId();
  const confirmId = useId();
  const waitingId = useId();
  // Orientações sem texto não contam (094/T08, A-08): sem nenhuma, a seção some.
  const guidance = (spec.role_guidance ?? []).filter((g) => g.text.trim() !== "");
  const config = useAppStore((s) => s.config);
  const turnOpen = useAppStore((s) => s.architectTurns[projectId] !== undefined);
  const stage = stageProp !== undefined ? stageProp : planStage({ running: false }, spec, turnOpen);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [completing, setCompleting] = useState(false);
  const [problem, setProblem] = useState<RunProblem | null>(null);
  const drafting = stage === "planning";
  const waiting = stage === "reviewing";
  const awaiting = spec.status === "AWAITING_APPROVAL" && stage !== null;
  const stale = spec.plan_review?.status === "stale";
  const blocked = stage === "awaiting" && gateBlocked(shell.readiness, "execute");
  const checks = checksOf(spec);
  const premises = spec.premises ?? [];
  const skills = spec.skills ?? [];
  const changes = spec.plan_changes ?? [];
  const moreParts: MorePart[] = [
    ...(premises.length > 0 ? (["premises"] as const) : []),
    ...(guidance.length > 0 ? (["guidance"] as const) : []),
    ...(spec.environment !== undefined ? (["environment"] as const) : []),
    ...(skills.length > 0 ? (["skills"] as const) : []),
    ...(drafting ? [] : (["checks"] as const)),
    ...(changes.length > 0 ? (["changes"] as const) : []),
  ];

  async function askArchitectToComplete(): Promise<void> {
    setCompleting(true);
    const result = await postChat(projectId, ARCHITECT_COMPLETE_PLAN_MESSAGE, { internal: true });
    setCompleting(false);
    if (!result.ok) {
      setProblem({ message: chatFailureMessage(result.error, architectWireApi(config)), missing: null });
      await onApproved();
      return;
    }
    appStore.showArchitectReply(projectId, result.data.reply, result.data.turn_id);
    await onApproved();
  }

  async function approve(): Promise<void> {
    if (busy || completing || blocked) return;
    setBusy(true);
    setProblem(null);
    const result = await approveSpec(projectId, spec.id);
    setBusy(false);
    setConfirming(false);
    if (result.ok) {
      shell.refreshAttention();
      await onApproved();
      return;
    }
    if (result.error.reason === "test_command_missing") {
      await askArchitectToComplete();
      return;
    }
    const next = approveProblem(result.error, shell.readiness);
    setProblem(next);
    if (next.missing !== null) void shell.reloadReadiness();
  }

  const fixButton =
    !drafting && checks.total > 0 && onRequestFix !== undefined ? (
      <button
        type="button"
        className="nd-btn"
        data-plan-fix=""
        disabled={fixDisabled || waiting}
        title={waiting ? t("plan.review.waitingReason") : undefined}
        aria-describedby={waiting ? waitingId : undefined}
        onClick={() => onRequestFix(planReviewFixMessage(checks.automatic, checks.issues))}
      >
        <Icon name="conversation" size={14} />
        {t("plan.review.fixButton")}
      </button>
    ) : null;

  let footer: ReactNode;
  if (awaiting && drafting && !completing) {
    footer = <Pending>{t("plan.drafting")}</Pending>;
  } else if (awaiting && confirming) {
    footer = (
      <div role="group" aria-labelledby={confirmId} className="nd-row nd-wrap">
        <span id={confirmId}>{t("plan.confirmStart")}</span>
        <button type="button" className="nd-btn" data-variant="primary" onClick={() => void approve()} disabled={busy || waiting || blocked} aria-busy={busy}>
          <Icon name="play" size={14} />
          {busy ? t("plan.starting") : t("plan.confirm")}
        </button>
        <button type="button" className="nd-btn" onClick={() => setConfirming(false)} disabled={busy}>
          {t("common.cancel")}
        </button>
      </div>
    );
  } else if (awaiting && completing) {
    footer = <Pending>{t("plan.architectCompleting")}</Pending>;
  } else if (awaiting) {
    footer = (
      <>
        {waiting ? <Pending id={waitingId}>{t("plan.review.waiting")}</Pending> : null}
        <div className="nd-row nd-wrap nd-plan-actions">
          <button
            type="button"
            className="nd-btn nd-plan-action"
            data-variant="primary"
            data-approve=""
            onClick={() => setConfirming(true)}
            disabled={busy || waiting || blocked}
            title={waiting ? t("plan.review.waitingReason") : undefined}
            aria-describedby={waiting ? waitingId : undefined}
          >
            {t("plan.approve")}
          </button>
          {fixButton}
        </div>
        {stale ? <p className="nd-muted">{t("plan.review.staleNotice")}</p> : null}
        <p className="nd-muted nd-plan-foot-note">{t("plan.footNote")}</p>
      </>
    );
  } else {
    footer = (
      <>
        {fixButton !== null ? <div className="nd-row nd-wrap nd-plan-actions">{fixButton}</div> : null}
        <p className="nd-muted" data-plan-status={spec.status ?? ""}>
          {SPEC_STATUS_LABELS[spec.status ?? ""] ?? t("plan.status.fallback")}
        </p>
      </>
    );
  }

  return (
    <section aria-labelledby={titleId} className="nd-plan" data-plan="">
      <header className="nd-plan-header">
        <h2 id={titleId} className="nd-plan-title">
          {spec.title !== "" ? spec.title : t("plan.untitled")}
        </h2>
        <p className="nd-plan-summary">
          <span>{spec.id}</span>
          {" · "}
          {drafting ? <span>{t("plan.drafting")}</span> : <span>{tp("plan.taskCount", spec.tasks.length)}</span>}
        </p>
      </header>

      <ol className="nd-plan-tasks" aria-label={t("plan.tasksLabel")}>
        {spec.tasks.map((task) => (
          <li key={task.id} className="nd-plan-task">
            {/* Só o id identifica a tarefa (094/T08, A-09): sem numeração redundante. */}
            <span className="nd-mono nd-plan-task-id">{task.id}</span>
            <span className="nd-plan-task-title nd-break">{task.title}</span>
            {task.depends_on.length > 0 ? (
              <span className="nd-meta-line nd-plan-task-meta" data-task-meta="">
                <span className="nd-meta-part nd-break">{t("plan.dependsOn", { ids: task.depends_on.join(", ") })}</span>
              </span>
            ) : null}
          </li>
        ))}
      </ol>

      {drafting ? null : <PlanChecksSummary checks={checks} />}

      {moreParts.length > 0 ? (
        <details className="nd-plan-more" data-plan-more="">
          <summary className="nd-plan-more-summary">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {planMoreLabel(moreParts.map((part) => t(MORE_PART_KEYS[part])))}
          </summary>
          <div className="nd-plan-more-body">
            {premises.length > 0 ? (
              <section aria-labelledby={`${titleId}-premises`} className="nd-plan-more-section">
                <h3 id={`${titleId}-premises`} className="nd-plan-heading">
                  {t("plan.premises")}
                </h3>
                <ul className="nd-bullets">
                  {premises.map((p, i) => (
                    <li key={`${i}-${p}`}>{p}</li>
                  ))}
                </ul>
              </section>
            ) : null}

            {guidance.length > 0 ? (
              <section aria-labelledby={`${titleId}-guidance`} className="nd-plan-more-section" data-plan-guidance="">
                <h3 id={`${titleId}-guidance`} className="nd-plan-heading">
                  {t("plan.guidance")}
                </h3>
                {guidance.map((g) => (
                  <div key={g.role} className="nd-stack-xs">
                    <h4 className="nd-section-label">{g.role}</h4>
                    <p className="nd-prewrap">{g.text}</p>
                  </div>
                ))}
              </section>
            ) : null}

            {spec.environment !== undefined ? <EnvironmentSection env={spec.environment} /> : null}

            {skills.length > 0 ? (
              <section aria-labelledby={`${titleId}-skills`} className="nd-plan-more-section">
                <h3 id={`${titleId}-skills`} className="nd-plan-heading">
                  {t("plan.skills")}
                </h3>
                {skills.map((s) => (
                  <details key={s.role} open={s.status !== "reaproveitado"} data-testid={`skill-${s.role}`} className="nd-details">
                    <summary className="nd-section-label">
                      <Icon name="chevron-right" size={14} className="nd-details-chevron" />
                      {s.role === "developer" ? t("role.developer") : s.role === "reviewer" ? t("role.reviewer") : s.role} · {skillStatusLabel(s.status)}
                      {s.stack.length > 0 ? ` · ${s.stack.join(", ")}` : ""}
                    </summary>
                    {s.reason !== undefined && s.reason !== "" ? <p className="nd-dialog-warning">{t("plan.reason", { reason: s.reason })}</p> : null}
                    {/* O perfil vem em Markdown (094/T08, A-08): o mesmo renderizador do Relatório. */}
                    <div className="nd-plan-skill-text" data-skill-text="">
                      <MarkdownText text={s.text} />
                    </div>
                  </details>
                ))}
              </section>
            ) : null}

            {drafting ? null : <PlanChecksDetail checks={checks} />}

            {changes.length > 0 ? <PlanChangesSection lines={changes} /> : null}
          </div>
        </details>
      ) : null}

      <footer className="nd-plan-foot" {...(awaiting ? { "data-plan-approve": "" } : {})}>
        {awaiting && blocked && shell.readiness !== null ? <ReadinessNotice title={t("plan.approveBlockedTitle")} missing={shell.readiness.execute.missing} /> : null}
        {footer}
        {problem !== null ? <ProblemNotice message={problem.message} missing={problem.missing} prereq={problem.prereq} /> : null}
      </footer>
    </section>
  );
}
