/*
 * Partes da mensagem do Revisor: selo do
 * veredito, apontamentos e observações (com o rótulo da observação rebaixada da 092) e a matriz
 * de critérios. As marcas e setas do formato viram ícones; todo texto é texto React.
 */
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import {
  finalVerificationCounts,
  reviewStateCounts,
  reviewStateLine,
  type ChecklistItem,
  type ReviewDecision,
  type ReviewFinding,
  type ReviewMessage,
  type ReviewResponse,
  type ReviewVerdict,
} from "../../reviewMessage";

/** Linha da verificação final antes do veredito: no idioma ativo quando reconhecida; senão, como veio. */
export function ReviewPrefix({ prefix }: { prefix: string }) {
  const { t } = useT();
  const counts = finalVerificationCounts(prefix);
  return <p className="nd-muted">{counts === null ? prefix : t("review.finalPrefix", counts)}</p>;
}

/** Selo do veredito: "Aprovado" (verde) ou "Pediu ajustes" (âmbar). */
export function ReviewVerdictBadge({ verdict }: { verdict: ReviewVerdict }) {
  const approved = verdict === "approved";
  const { t } = useT();
  return (
    <span data-testid="review-verdict" data-verdict={verdict} className="nd-pill" data-tone={approved ? "green" : "amber"}>
      <Icon name={approved ? "check" : "alert"} size={14} />
      {approved ? t("review.approved") : t("review.changes")}
    </span>
  );
}

/** Spec 124 T01: resumo dos estados ("1 novo · 2 ainda abertos · 1 resolvido"); nada em mensagens antigas. */
export function ReviewStateSummary({ review }: { review: ReviewMessage }) {
  const { tp } = useT();
  const line = reviewStateLine(reviewStateCounts(review), tp);
  if (line === "") return null;
  return (
    <p data-testid="review-state-summary" className="nd-muted">
      {line}
    </p>
  );
}

/** Selo do estado de um apontamento (spec 124 T01): novo, ainda aberto há N revisões ou resolvido. */
function FindingStateBadge({ finding }: { finding: ReviewFinding }) {
  const { t, tp } = useT();
  if (finding.state === undefined) return null;
  const label =
    finding.state === "new"
      ? t("review.stateNew")
      : finding.state === "open"
        ? tp("review.stateOpen", finding.recurrence ?? 2)
        : t("review.stateResolved");
  const tone = finding.state === "open" ? "rose" : finding.state === "new" ? "violet" : "green";
  return (
    <span data-testid="review-finding-state" data-state={finding.state} className="nd-pill nd-pill-sm" data-tone={tone}>
      {label}
    </span>
  );
}

const LIST_HEADER = { blocking: "review.findingsHeader", observation: "review.observationsHeader", resolved: "review.resolvedHeader" } as const;
const LIST_LABEL = { blocking: "review.findings", observation: "review.observations", resolved: "review.resolved" } as const;

/** Apontamentos (bloqueiam), observações (não bloqueiam) ou resolvidos nesta revisão (spec 124 T01). */
export function ReviewFindingList({ findings, kind }: { findings: readonly ReviewFinding[]; kind: "blocking" | "observation" | "resolved" }) {
  const { t } = useT();
  if (findings.length === 0) return null;
  const blocking = kind === "blocking";
  return (
    <section className="nd-findings" data-kind={kind}>
      <h4 className="nd-section-label" data-tone={blocking ? "amber" : kind === "resolved" ? "green" : undefined}>
        {t(LIST_HEADER[kind])}
      </h4>
      <ul aria-label={t(LIST_LABEL[kind])} className="nd-finding-list">
        {findings.map((f, index) => (
          <li key={index} className="nd-finding" data-kind={kind}>
            <div className="nd-row nd-wrap">
              {f.id !== "" ? <span className="nd-strong">{f.id}</span> : null}
              {f.severity !== "" ? (
                <span className="nd-pill nd-pill-sm" data-tone={blocking ? "amber" : "muted"}>
                  {f.severity}
                </span>
              ) : null}
              <FindingStateBadge finding={f} />
              {f.unchangedFile === true ? (
                <span data-testid="review-unchanged-file" className="nd-pill nd-pill-sm" data-tone="cyan">
                  {t("review.unchangedFile")}
                </span>
              ) : null}
              {f.lowConfidence === true ? (
                <span data-testid="review-low-confidence" className="nd-pill nd-pill-sm" data-tone="muted">
                  {t("review.lowConfidence")}
                </span>
              ) : null}
              {f.location !== "" ? <code className="nd-mono nd-muted nd-break">{f.location}</code> : null}
            </div>
            <p className="nd-finding-text">{f.description}</p>
            {f.recommendation !== "" ? (
              <p className="nd-finding-text nd-muted nd-row">
                <Icon name="arrow-right" size={14} />
                <span className="nd-sr-only">{t("review.recommendation")}</span>
                <span>{f.recommendation}</span>
              </p>
            ) : null}
            {f.scenario !== undefined ? (
              <p data-testid="review-scenario" className="nd-finding-scenario">
                {t("review.scenario")} {f.scenario}
              </p>
            ) : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

const RESPONSE_KEYS = {
  fixed: "review.response.fixed",
  test_added: "review.response.test_added",
  contested: "review.response.contested",
} as const;
const CONTEST_KEYS = {
  environment: "review.contest.environment",
  out_of_scope: "review.contest.out_of_scope",
  incorrect: "review.contest.incorrect",
} as const;
const DECISION_KEYS = { keep: "review.decision.keep", adjust: "review.decision.adjust", manual: "review.decision.manual" } as const;

/** Rótulo do dicionário para um código conhecido; o código como veio, senão. */
function codeLabel<K extends string>(keys: Readonly<Record<string, K>>, code: string, t: (key: K) => string): string {
  return Object.hasOwn(keys, code) ? t(keys[code] as K) : code;
}

/** Spec 124 T02: respostas do Desenvolvedor por apontamento (corrigido, teste adicionado, contestado). */
export function ReviewResponseList({ responses }: { responses: readonly ReviewResponse[] }) {
  const { t } = useT();
  if (responses.length === 0) return null;
  return (
    <section className="nd-findings" data-kind="responses">
      <h4 className="nd-section-label">{t("review.responsesHeader")}</h4>
      <ul aria-label={t("review.responses")} className="nd-finding-list">
        {responses.map((r, index) => (
          <li key={index} className="nd-finding" data-kind="response" data-response={r.kind}>
            <div className="nd-row nd-wrap">
              {r.id !== "" ? <span className="nd-strong">{r.id}</span> : null}
              <span data-testid="review-response-kind" className="nd-pill nd-pill-sm" data-tone={r.kind === "contested" ? "amber" : "green"}>
                {codeLabel(RESPONSE_KEYS, r.kind, t)}
              </span>
              {r.reason !== undefined ? (
                <span data-testid="review-contest-reason" className="nd-pill nd-pill-sm" data-tone="muted">
                  {codeLabel(CONTEST_KEYS, r.reason, t)}
                </span>
              ) : null}
            </div>
            {r.detail !== "" ? <p className="nd-finding-text nd-break">{r.detail}</p> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Spec 124 T02: decisões do Arquiteto sobre os apontamentos contestados. */
export function ReviewDecisionList({ decisions }: { decisions: readonly ReviewDecision[] }) {
  const { t } = useT();
  if (decisions.length === 0) return null;
  return (
    <section className="nd-findings" data-kind="decisions">
      <h4 className="nd-section-label">{t("review.decisionsHeader")}</h4>
      <ul aria-label={t("review.decisions")} className="nd-finding-list">
        {decisions.map((d, index) => (
          <li key={index} className="nd-finding" data-kind="decision" data-decision={d.decision}>
            <div className="nd-row nd-wrap">
              {d.id !== "" ? <span className="nd-strong">{d.id}</span> : null}
              <span data-testid="review-decision" className="nd-pill nd-pill-sm" data-tone={d.decision === "keep" ? "rose" : "violet"}>
                {codeLabel(DECISION_KEYS, d.decision, t)}
              </span>
            </div>
            {d.detail !== "" ? <p className="nd-finding-text">{d.detail}</p> : null}
          </li>
        ))}
      </ul>
    </section>
  );
}

/** Matriz de critérios: ícone de atendido ou não atendido, com o nome acessível. */
export function ReviewerChecklist({ items }: { items: readonly ChecklistItem[] }) {
  const { t } = useT();
  return (
    <ul aria-label={t("review.criteria")} className="nd-criteria">
      {items.map((item, index) => (
        <li key={index} className="nd-criterion" data-met={item.met ? "true" : "false"}>
          <span role="img" aria-label={item.met ? t("review.met") : t("review.unmet")} className="nd-criterion-mark">
            <Icon name={item.met ? "check" : "close"} size={14} />
          </span>
          <span className="nd-criterion-text">{item.text}</span>
        </li>
      ))}
    </ul>
  );
}
