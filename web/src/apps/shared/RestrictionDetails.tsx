/*
 * Spec 141 T04 (C-03): a troca de uma etapa do comando de testes por ela mesma com um caminho
 * acrescentado, como o servidor a oferece numa opção `config` de recuperação. Mostra o antes e o
 * depois, o que foi acrescentado, o que a saída de cada etapa informa (lado a lado) e as duas
 * rodadas que o próprio Nants fez (a etapa atual falhando e a restrita passando). A restrição nunca é
 * aplicada sozinha: a pessoa aprova vendo isto.
 */
import type { RecoveryRestriction } from "../../api/types";
import { useT } from "../../i18n/react";

/** A opção tem a evidência das rodadas do servidor (só então o "aprovar" aparece). */
export function hasRestrictionEvidence(restriction: RecoveryRestriction | undefined): restriction is RecoveryRestriction {
  return restriction !== undefined && restriction.tree !== "" && restriction.original_output !== "" && restriction.restricted_output !== "";
}

/**
 * A etapa nova roda menos testes do que os que passavam na atual (com as três contagens
 * informadas): `M < N - F`.
 */
function runsFewer(r: RecoveryRestriction): boolean {
  if (r.tests === undefined || r.original_tests === undefined || r.original_failed === undefined) return false;
  return r.tests < r.original_tests - r.original_failed;
}

export function RestrictionDetails({ restriction }: { restriction: RecoveryRestriction }) {
  const { t, tp } = useT();
  const current =
    restriction.original_tests === undefined
      ? null
      : restriction.original_failed === undefined
        ? tp("recovery.restriction.currentTests", restriction.original_tests)
        : `${tp("recovery.restriction.currentTests", restriction.original_tests)}, ${tp("recovery.restriction.currentFailed", restriction.original_failed)}`;
  return (
    <div className="nd-stack-xs" data-restriction="">
      <p className="nd-strong">{t("recovery.restriction.title")}</p>
      <dl className="nd-stack-xs">
        <div>
          <dt className="nd-muted">{t("recovery.restriction.before")}</dt>
          <dd className="nd-mono nd-break">{restriction.original}</dd>
        </div>
        <div>
          <dt className="nd-muted">{t("recovery.restriction.after")}</dt>
          <dd className="nd-mono nd-break">{restriction.restricted}</dd>
        </div>
      </dl>
      <p className="nd-break">{t("recovery.restriction.appended", { paths: restriction.appended.join(" ") })}</p>
      {current !== null || restriction.tests !== undefined ? (
        <div className="nd-row" data-restriction-counts="">
          {current !== null ? <p>{current}</p> : null}
          {restriction.tests !== undefined ? <p>{tp("recovery.restriction.newTests", restriction.tests)}</p> : null}
        </div>
      ) : null}
      {runsFewer(restriction) ? (
        <p className="nd-dialog-warning" role="note">
          {t("recovery.restriction.fewer")}
        </p>
      ) : null}
      <p className="nd-muted">{t("recovery.restriction.note")}</p>
      <p className="nd-muted">{t("recovery.restriction.officialRun")}</p>
      <pre className="nd-md-pre">{restriction.original_output}</pre>
      <p className="nd-muted">{t("recovery.restriction.restrictedRun")}</p>
      <pre className="nd-md-pre">{restriction.restricted_output}</pre>
    </div>
  );
}
