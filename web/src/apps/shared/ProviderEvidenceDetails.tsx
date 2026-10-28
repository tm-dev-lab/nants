/*
 * Detalhe recolhível da falha do provedor (spec 133 T01): os códigos crus (o código do Nants, o
 * status HTTP, o tipo, o código e o parâmetro do erro, o id do pedido, o tipo da conexão e a
 * tentativa). A frase principal da mensagem nunca mostra esses códigos.
 */
import type { CodeParams } from "../../api/codes";
import { Icon } from "../../desktop/Icon";
import { PROVIDER_STOP_CODES, providerEvidence } from "../../i18n/providerEvidence";
import { useT } from "../../i18n/react";

export function ProviderEvidenceDetails({ code, params }: { code?: string; params?: CodeParams }) {
  const { t, lang } = useT();
  if (code === undefined || !PROVIDER_STOP_CODES.has(code)) return null;
  const evidence = providerEvidence(params, lang);
  if (evidence === null || evidence.raw.length === 0) return null;
  return (
    <details className="nd-details" data-provider-evidence="">
      <summary>
        <Icon name="chevron-right" size={14} className="nd-details-chevron" />
        {t("provider.evidence.details")}
      </summary>
      <dl className="nd-provider-evidence">
        {evidence.raw.map((row) => (
          <div key={row.label}>
            <dt>{row.label}</dt>
            <dd className="nd-break">{row.value}</dd>
          </div>
        ))}
      </dl>
    </details>
  );
}
