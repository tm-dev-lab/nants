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
 * - tarefa grande demais (spec 126 T03): o `too_large` que restou depois da divisão pelo Arquiteto
 *   vem primeiro na faixa, com a tarefa em destaque e a sugestão de divisão, e a tarefa ganha a
 *   marca "grande demais" na lista; a aprovação continua possível;
 * - plano incompleto (spec 127 T01, `plan_review.status === "incomplete"`: a spec em aprovação sem
 *   nenhuma tarefa): o aviso, "Aprovar" desabilitado e "Pedir ao Arquiteto para terminar", que manda
 *   a continuação interna pelo servidor (`POST /chat/continue`);
 * - consenso do plano (spec 128 T05, `plan_review.consensus`): durante o ciclo, o progresso ("O
 *   Revisor está conferindo o plano", "O Arquiteto está avaliando N apontamentos", "Rodada 2 de 3")
 *   com a aprovação bloqueada; no fim, o resumo (apontamentos, ajustados, mantidos, tamanho do plano
 *   contra a primeira versão), os mantidos com o motivo recolhidos e, na faixa, só os pontos em
 *   aberto, reabertos ou em disputa com as duas posições. "Pedir correção" manda só esses pontos.
 *   Sem nada em aberto, o plano aparece como pronto. A confirmação da aprovação avisa a
 *   renumeração na ordem de execução da lista;
 * - prontidão (decisão 15): sem `readiness.execute`, o botão fica desabilitado com o que falta;
 *   o 412 `prereq_missing` mostra o que falta com "Configurar".
 */
import { useId, useState, type ReactNode } from "react";
import { approveSpec, postChat, postChatContinue } from "../../api/client";
import type { ConsensusView, EnvironmentView, PlanChangeEntry, PlanIssue, PlanPointView, PlanReviewView, SpecView } from "../../api/types";
import { chatFailureMessage } from "../../chatFailure";
import { loadChatHistory } from "../../chatHistory";
import { ARCHITECT_COMPLETE_PLAN_MESSAGE } from "../../copy";
import { Icon } from "../../desktop/Icon";
import { t, tp, type MessageKey } from "../../i18n";
import { planChangeText } from "../../i18n/planChanges";
import { providerStopText } from "../../i18n/providerEvidence";
import { useT } from "../../i18n/react";
import { localizeHeadingTitle } from "../../i18n/sddHeadings";
import { useShell } from "../../desktop/shellContext";
import { planStage, type PlanStage } from "../../phase";
import { architectWireApi } from "../../settings/rules";
import { appStore, useAppStore } from "../../store";
import { gateBlocked } from "../shared/readiness";
import { MarkdownText } from "../shared/ReportMarkdown";
import { ProblemNotice, ReadinessNotice } from "../shared/ReadinessNotice";
import { ProviderEvidenceDetails } from "../shared/ProviderEvidenceDetails";
import { renumberMap, tasksInExecutionOrder } from "../shared/executionOrder";
import { approveProblem, type RunProblem } from "../shared/runProblems";
import { SPEC_STATUS_LABELS } from "./summary";

/** Situação de um ponto que pede a decisão do usuário no consenso do plano (spec 128 T05). */
type PointState = "open" | "reopened" | "disputed";

/**
 * Item das checagens do Revisor. No consenso (spec 128 T05), só os pontos em aberto, reabertos ou
 * em disputa, com a posição do Arquiteto (`reason_kind` e `reason`) quando houver.
 */
interface CheckIssue extends PlanIssue {
  state?: PointState;
  reason_kind?: string;
  reason?: string;
}

const REASON_KIND_KEYS: Readonly<Record<string, MessageKey>> = {
  agree: "plan.consensus.reasonKind.agree",
  unverifiable_in_sandbox: "plan.consensus.reasonKind.unverifiable_in_sandbox",
  not_worth_cost: "plan.consensus.reasonKind.not_worth_cost",
  duplicate: "plan.consensus.reasonKind.duplicate",
  inflates_criteria: "plan.consensus.reasonKind.inflates_criteria",
  other: "plan.consensus.reasonKind.other",
};

const POINT_STATE_KEYS: Readonly<Record<PointState, MessageKey>> = {
  open: "plan.consensus.state.open",
  reopened: "plan.consensus.state.reopened",
  disputed: "plan.consensus.state.disputed",
};

const OUTCOME_KEYS: Readonly<Record<string, MessageKey>> = {
  open: "plan.consensus.outcome.open",
  disputed: "plan.consensus.outcome.disputed",
  awaiting_user: "plan.consensus.outcome.awaiting_user",
  unavailable: "plan.consensus.outcome.unavailable",
  interrupted: "plan.consensus.outcome.interrupted",
  shutdown: "plan.consensus.outcome.shutdown",
};

/**
 * Posição do Arquiteto num ponto (C-02): o motivo pelo `reason_kind`, traduzido, mais a frase dele.
 * `null` sem nenhum dos dois (o Arquiteto ainda não respondeu).
 */
export function architectPosition(point: { reason_kind?: string; reason?: string }): string | null {
  const key = point.reason_kind !== undefined ? REASON_KIND_KEYS[point.reason_kind] : undefined;
  const kind = key !== undefined ? t(key) : null;
  const reason = point.reason?.trim() ?? "";
  if (kind !== null && reason !== "") return t("plan.consensus.reasonWithText", { kind, reason });
  if (kind !== null) return kind;
  return reason !== "" ? reason : null;
}

/** Pontos do consenso que pedem a decisão do usuário: em aberto, reabertos e em disputa. */
export function consensusPending(consensus: ConsensusView): CheckIssue[] {
  const as =
    (state: PointState) =>
    (point: PlanPointView): CheckIssue => ({
      id: point.id,
      task: point.task,
      kind: point.kind,
      text: point.text,
      state,
      ...(point.reason_kind !== undefined ? { reason_kind: point.reason_kind } : {}),
      ...(point.reason !== undefined ? { reason: point.reason } : {}),
    });
  return [...consensus.open.map(as("open")), ...(consensus.reopened ?? []).map(as("reopened")), ...consensus.disputed.map(as("disputed"))];
}

/**
 * Mensagem do usuário com os itens das checagens, um por linha (no idioma ativo). No consenso, só
 * os pontos em aberto ou em disputa, cada um com a posição do Arquiteto na linha seguinte.
 */
export function planReviewFixMessage(automatic: readonly string[], issues: readonly CheckIssue[]): string {
  const lines = [
    ...automatic.map((warning) => `- ${warning}`),
    ...issues.flatMap((issue) => {
      const where = issue.task ?? "spec";
      const head = issue.state !== undefined && issue.id !== undefined ? `- ${where} (${issue.id}): ${issue.text}` : `- ${where}: ${issue.text}`;
      const position = issue.state !== undefined ? architectPosition(issue) : null;
      return position === null ? [head] : [head, `  ${t("plan.review.fixPosition", { position })}`];
    }),
  ];
  return [t("plan.review.fixPrefix"), ...lines].join("\n");
}

/** Apontamentos `too_large` com tarefa (spec 126 T03): os que pedem destaque no cartão. */
export function tooLargeIssues<T extends PlanIssue>(issues: readonly T[]): T[] {
  return issues.filter((issue) => issue.kind === "too_large" && issue.task !== null);
}

/**
 * Título da faixa âmbar das checagens ("O Revisor fez 1 apontamento"), no idioma ativo. No
 * consenso (`consensus`), a parte do Revisor conta os pontos em aberto entre ele e o Arquiteto.
 */
export function planWarnTitle(reviewer: number, automatic: number, consensus = false): string {
  const byReviewer = tp(consensus ? "plan.warn.open" : "plan.warn.reviewer", reviewer);
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

/**
 * Progresso da revisão em curso (spec 128 T05): "O Revisor está conferindo o plano", "O Arquiteto
 * está avaliando N apontamentos" e a rodada de ajuste ("Rodada 2 de 3"). Sem o ciclo, a frase de antes.
 */
function ReviewProgress({ review }: { review: PlanReviewView }) {
  const { t, tp } = useT();
  const consensus = review.consensus;
  if (consensus === undefined) return <Pending>{t("plan.review.running")}</Pending>;
  const text = consensus.phase === "architect" ? tp("plan.consensus.evaluating", consensus.evaluating) : t("plan.consensus.reviewing");
  return (
    <Pending>
      <span data-plan-progress={consensus.phase ?? "review"}>{text}</span>
      {consensus.round > 0 ? (
        <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-plan-round="">
          {t("plan.consensus.round", { round: consensus.round, max: consensus.max_rounds })}
        </span>
      ) : null}
    </Pending>
  );
}

function ReviewerStatus({ review }: { review: PlanReviewView }) {
  const { t, lang } = useT();
  if (review.status === "incomplete") return <p className="nd-muted">{t("plan.review.incomplete")}</p>;
  if (review.status === "running" || review.status === "pending") return <ReviewProgress review={review} />;
  if (review.status === "stale") return <p className="nd-muted">{t("plan.review.stale")}</p>;
  if (review.status === "unavailable") {
    // Spec 133 T11: com a evidência da falha, quem falhou (papel, conexão e modelo) e o motivo
    // traduzido; os códigos crus no detalhe recolhível.
    const failure = providerStopText(PLAN_REVIEW_FAILED_CODE, review.failure, null, lang);
    if (failure === null) return <p className="nd-muted">{t("plan.review.unavailable")}</p>;
    return (
      <div className="nd-stack-xs" data-plan-review-failure="">
        <p className="nd-muted">{t("plan.review.unavailableBy", { failure })}</p>
        <ProviderEvidenceDetails code={PLAN_REVIEW_FAILED_CODE} params={review.failure} />
      </div>
    );
  }
  return <p className="nd-muted">{t("plan.review.clean")}</p>;
}

/** Código com a frase da falha do provedor (o mesmo da conversa), para a revisão do plano. */
const PLAN_REVIEW_FAILED_CODE = "chat.provider_failed";

/** Situação curta do Revisor no detalhe (a frase completa fica na vista principal). */
function reviewerDetail(review: PlanReviewView, issues: number): string {
  if (issues > 0) return tp("plan.review.issuesAbove", issues);
  if (review.status === "running" || review.status === "pending") return t("plan.review.reviewing");
  if (review.status === "incomplete") return t("plan.review.incomplete");
  if (review.status === "stale") return t("plan.review.noAnswer");
  if (review.status === "unavailable") return t("plan.review.unavailableShort");
  return t("plan.review.clean");
}

/** Itens das checagens e a situação da revisão (o mesmo cálculo para a vista, o detalhe e o rodapé). */
interface Checks {
  automatic: readonly string[];
  /** Itens do Revisor; no consenso, só os pontos em aberto, reabertos ou em disputa. */
  issues: readonly CheckIssue[];
  review: PlanReviewView | undefined;
  /** Ciclo de consenso concluído (spec 128 T05); ausente nos registros antigos. */
  consensus: ConsensusView | undefined;
  total: number;
  reviewDone: boolean;
}

function checksOf(spec: SpecView): Checks {
  const automatic = [...(spec.scope_warnings ?? []), ...(spec.coverage_warnings ?? [])];
  const review = spec.plan_review;
  const consensus = review?.status === "ok" ? review.consensus : undefined;
  const issues = review?.status === "ok" ? (consensus !== undefined ? consensusPending(consensus) : review.issues) : [];
  return {
    automatic,
    issues,
    review,
    consensus,
    total: automatic.length + issues.length,
    reviewDone: review === undefined || review.status === "ok",
  };
}

/** Tamanho do plano (C-10): "N tarefas (eram M na primeira versão)" e os critérios antes e depois. */
function planSizeText(consensus: ConsensusView): string {
  const tasks =
    consensus.tasks === consensus.tasks_first
      ? tp("plan.consensus.tasksSame", consensus.tasks)
      : tp("plan.consensus.tasksChanged", consensus.tasks, { first: consensus.tasks_first });
  const criteria =
    consensus.criteria === consensus.criteria_first
      ? tp("plan.consensus.criteriaSame", consensus.criteria)
      : tp("plan.consensus.criteriaChanged", consensus.criteria, { first: consensus.criteria_first });
  return t("plan.consensus.size", { tasks, criteria });
}

/** Pontos que esperam a decisão do usuário: em aberto, reabertos e em disputa. */
function consensusWaiting(consensus: ConsensusView): number {
  return consensus.open.length + (consensus.reopened ?? []).length + consensus.disputed.length;
}

/**
 * Apontamentos do resumo, por ponto distinto: ajustados + mantidos + à espera. É a regra do
 * servidor (`total`); sem o campo, a mesma soma.
 */
function consensusTotal(consensus: ConsensusView): number {
  return consensus.total ?? consensus.adjusted + consensus.kept + consensusWaiting(consensus);
}

/**
 * Resumo do consenso (spec 128 T05, G1): quantos apontamentos houve, quantos foram ajustados,
 * mantidos e quantos seguem em aberto (a soma fecha com o total), o tamanho do plano e os mantidos
 * com o motivo, recolhidos por padrão.
 */
function ConsensusFacts({ consensus }: { consensus: ConsensusView }) {
  const { t, tp } = useT();
  const total = consensusTotal(consensus);
  if (total === 0) return null;
  const kept = consensus.kept_points;
  const waiting = consensusWaiting(consensus);
  const parts = {
    points: tp("plan.consensus.points", total),
    adjusted: tp("plan.consensus.adjusted", consensus.adjusted),
    kept: tp("plan.consensus.kept", consensus.kept),
  };
  return (
    <div className="nd-plan-consensus" data-plan-consensus="">
      <p data-plan-consensus-summary="">
        {waiting > 0 ? t("plan.consensus.summaryOpen", { ...parts, open: tp("plan.consensus.open", waiting) }) : t("plan.consensus.summary", parts)}
      </p>
      {consensus.tasks_first > 0 ? (
        <p className="nd-muted" data-plan-size="">
          {planSizeText(consensus)}
        </p>
      ) : null}
      {kept.length > 0 ? (
        <details className="nd-details" data-plan-kept="">
          <summary>
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {tp("plan.consensus.keptTitle", kept.length)}
          </summary>
          <ul className="nd-plan-warn-list">
            {kept.map((point) => (
              <li key={point.id} className="nd-break nd-plan-point" data-plan-kept-point={point.id}>
                <span>
                  <span className="nd-mono">{point.task ?? "spec"}</span>: {point.text}
                </span>
                <span className="nd-plan-point-side">
                  <span className="nd-plan-point-label">{t("plan.consensus.architect")}:</span> {architectPosition(point) ?? t("plan.consensus.noAnswer")}
                </span>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </div>
  );
}

/** Um ponto em aberto, reaberto ou em disputa: o apontamento do Revisor e a posição do Arquiteto. */
function ConsensusPoint({ point }: { point: CheckIssue }) {
  const { t } = useT();
  const position = architectPosition(point);
  return (
    <li className="nd-break nd-plan-point" data-plan-point={point.state ?? "open"} data-plan-point-id={point.id ?? ""}>
      <span className="nd-plan-point-head">
        <span className="nd-mono">{point.task ?? "spec"}</span>
        <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="amber">
          {t(POINT_STATE_KEYS[point.state ?? "open"])}
        </span>
      </span>
      <span>
        <span className="nd-plan-point-label">{t("plan.consensus.reviewer")}:</span> {point.text}
      </span>
      <span className="nd-plan-point-side">
        <span className="nd-plan-point-label">{t("plan.consensus.architect")}:</span> {position ?? t("plan.consensus.noAnswer")}
      </span>
    </li>
  );
}

/** Sem ponto em aberto: o plano aparece como pronto, sem aviso de problema (critério 3). */
function ConsensusReady({ consensus }: { consensus: ConsensusView }) {
  const { t } = useT();
  const total = consensusTotal(consensus);
  return (
    <section className="nd-plan-consensus" data-plan-ready="">
      <p className="nd-row">
        <Icon name="check" size={14} />
        <span>{total > 0 ? t("plan.consensus.ready") : t("plan.consensus.readyClean")}</span>
      </p>
      <ConsensusFacts consensus={consensus} />
    </section>
  );
}

/**
 * Vista principal das checagens: a faixa âmbar com os itens por origem; sem itens, a revisão em
 * curso ou uma linha discreta (sem resposta, indisponível, nada a corrigir). Nunca bloqueia.
 */
function PlanChecksSummary({ checks }: { checks: Checks }) {
  const { t } = useT();
  const titleId = useId();
  const { automatic, issues, review, consensus, total, reviewDone } = checks;
  if (total === 0) {
    // Plano incompleto (spec 127 T01): o aviso fica no rodapé, junto da ação.
    if (review?.status === "incomplete") return null;
    if (review !== undefined && review.status !== "ok") return <ReviewerStatus review={review} />;
    if (consensus !== undefined) return <ConsensusReady consensus={consensus} />;
    return (
      <p data-testid="plan-checks-clean" className="nd-muted">
        {t("plan.checks.clean")}
      </p>
    );
  }
  const reviewerShown = review !== undefined && (issues.length > 0 || !reviewDone);
  const big = consensus !== undefined ? [] : tooLargeIssues(issues);
  const others = issues.filter((issue) => !big.includes(issue));
  const both = reviewerShown && automatic.length > 0;
  const outcomeKey = consensus?.outcome !== undefined ? OUTCOME_KEYS[consensus.outcome] : undefined;
  const pendingPoints = consensus !== undefined && issues.length > 0;
  return (
    <>
    {consensus !== undefined ? <ConsensusFacts consensus={consensus} /> : null}
    <section aria-label={t("plan.checks.title")} aria-describedby={titleId} className="nd-plan-warn" data-plan-warn="">
      <Icon name="alert" size={16} className="nd-plan-warn-icon" />
      <div className="nd-plan-warn-text">
        <h3 id={titleId} className="nd-plan-warn-title">
          {planWarnTitle(issues.length, automatic.length, consensus !== undefined)}
        </h3>
        {reviewerShown ? (
          <section aria-label={t("role.reviewer")} className="nd-stack-xs">
            {both ? <h4 className="nd-plan-warn-source">{t("role.reviewer")}</h4> : null}
            {pendingPoints ? (
              <ul className="nd-plan-warn-list" data-plan-pending="">
                {issues.map((point, i) => (
                  <ConsensusPoint key={`${point.id ?? i}-${point.text}`} point={point} />
                ))}
              </ul>
            ) : issues.length > 0 ? (
              <ul className="nd-plan-warn-list">
                {big.map((issue, i) => (
                  <li key={`big-${i}-${issue.text}`} className="nd-break nd-plan-big" data-plan-too-large={issue.task ?? ""}>
                    <strong className="nd-plan-big-title">{t("plan.tooLarge.flag", { task: issue.task ?? "" })}</strong>{" "}
                    <span>{t("plan.tooLarge.suggestion", { text: issue.text })}</span>
                  </li>
                ))}
                {others.map((issue, i) => (
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
        {pendingPoints ? (
          <p className="nd-plan-warn-note" data-plan-decide="">
            {outcomeKey !== undefined ? `${t(outcomeKey, { max: consensus?.max_rounds ?? 0 })} ` : ""}
            {t("plan.consensus.decide")}
          </p>
        ) : (
          <p className="nd-plan-warn-note">{t("plan.checks.notBlocking")}</p>
        )}
      </div>
    </section>
    </>
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

/** Linhas do "Mudou nesta versão" no idioma ativo (spec 131 T06); código sem chave fica de fora. */
function PlanChangesSection({ entries }: { entries: readonly PlanChangeEntry[] }) {
  const { t, lang } = useT();
  const lines = entries.map((entry) => planChangeText(entry, lang)).filter((line): line is string => line !== null && line !== "");
  if (lines.length === 0) return null;
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
  const { t, tp, lang } = useT();
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
  const incomplete = spec.plan_review?.status === "incomplete";
  const incompleteId = useId();
  const blocked = stage === "awaiting" && gateBlocked(shell.readiness, "execute");
  const checks = checksOf(spec);
  const renumbering = renumberMap(spec)
    .map(([from, to]) => t("plan.renumberPair", { from, to }))
    .join(", ");
  const renumberId = useId();
  const bigTasks = new Set(tooLargeIssues(checks.issues).map((issue) => issue.task));
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

  /** "Pedir ao Arquiteto para terminar" (spec 127 T01): continuação interna do plano sem tarefas. */
  async function askArchitectToFinish(): Promise<void> {
    if (completing) return;
    setCompleting(true);
    setProblem(null);
    const result = await postChatContinue(projectId, spec.id);
    setCompleting(false);
    if (!result.ok) {
      // Queda da conexão de novo: o aviso fica gravado na Conversa.
      if (result.error.reason === "connection_lost" || result.error.reason === "failure_recorded") void loadChatHistory(projectId);
      setProblem({ message: chatFailureMessage(result.error, architectWireApi(config)), missing: null });
      await onApproved();
      return;
    }
    appStore.showArchitectReply(projectId, result.data.reply, result.data.turn_id);
    await onApproved();
  }

  async function approve(): Promise<void> {
    if (busy || completing || blocked || incomplete) return;
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
    // C-07: a confirmação avisa que as tarefas serão renumeradas na ordem de execução da lista.
    footer = (
      <div role="group" aria-labelledby={confirmId} aria-describedby={renumbering !== "" ? renumberId : undefined} className="nd-row nd-wrap">
        <span id={confirmId}>{t("plan.confirmStart")}</span>
        {renumbering !== "" ? (
          <span id={renumberId} className="nd-muted nd-break" data-plan-renumber="">
            {t("plan.confirmRenumber", { map: renumbering })}
          </span>
        ) : null}
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
  } else if (awaiting && incomplete) {
    footer = (
      <>
        <p id={incompleteId} role="status" className="nd-plan-incomplete" data-plan-incomplete="">
          <Icon name="alert" size={14} />
          <span>{t("plan.incomplete.text")}</span>
        </p>
        <div className="nd-row nd-wrap nd-plan-actions">
          <button type="button" className="nd-btn nd-plan-action" data-variant="primary" data-plan-finish="" onClick={() => void askArchitectToFinish()}>
            <Icon name="conversation" size={14} />
            {t("plan.incomplete.finish")}
          </button>
          <button type="button" className="nd-btn nd-plan-action" data-approve="" disabled aria-describedby={incompleteId}>
            {t("plan.approve")}
          </button>
        </div>
      </>
    );
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
        {tasksInExecutionOrder(spec).map((task) => (
          <li key={task.id} className="nd-plan-task">
            {/* Só o id identifica a tarefa (094/T08, A-09): sem numeração redundante. */}
            <span className="nd-mono nd-plan-task-id">{task.id}</span>
            <span className="nd-plan-task-title nd-break">{task.title}</span>
            {bigTasks.has(task.id) ? (
              <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone="amber" data-task-too-large="">
                {t("plan.tooLarge.badge")}
              </span>
            ) : null}
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
                    {/* O papel é o título da subseção no spec.md (gravado em pt-BR): no idioma ativo. */}
                    <h4 className="nd-section-label">{localizeHeadingTitle(g.role, lang)}</h4>
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

            {changes.length > 0 ? <PlanChangesSection entries={changes} /> : null}
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
