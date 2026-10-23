/*
 * Partes da mensagem do Revisor: selo do
 * veredito, apontamentos e observações (com o rótulo da observação rebaixada da 092) e a matriz
 * de critérios. As marcas e setas do formato viram ícones; todo texto é texto React.
 */
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { finalVerificationCounts, type ChecklistItem, type ReviewFinding, type ReviewVerdict } from "../../reviewMessage";

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

/** Apontamentos (bloqueiam) ou observações (não bloqueiam). */
export function ReviewFindingList({ findings, kind }: { findings: readonly ReviewFinding[]; kind: "blocking" | "observation" }) {
  const { t } = useT();
  if (findings.length === 0) return null;
  const blocking = kind === "blocking";
  return (
    <section className="nd-findings" data-kind={kind}>
      <h4 className="nd-section-label" data-tone={blocking ? "amber" : undefined}>
        {blocking ? t("review.findingsHeader") : t("review.observationsHeader")}
      </h4>
      <ul aria-label={blocking ? t("review.findings") : t("review.observations")} className="nd-finding-list">
        {findings.map((f, index) => (
          <li key={index} className="nd-finding" data-kind={kind}>
            <div className="nd-row nd-wrap">
              {f.id !== "" ? <span className="nd-strong">{f.id}</span> : null}
              {f.severity !== "" ? (
                <span className="nd-pill nd-pill-sm" data-tone={blocking ? "amber" : "muted"}>
                  {f.severity}
                </span>
              ) : null}
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
