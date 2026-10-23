/*
 * Nova conexão (spec 116 T03, decisão 7): o primeiro passo pergunta como conectar, com dois cartões.
 * - "Chave de API" (OpenAI, Azure e outros compatíveis): segue para o formulário de sempre.
 * - "Plano ChatGPT (Plus ou Pro)": uma frase curta e o botão "Continue with ChatGPT". Sem lista do
 *   que funciona ou não (decisão do usuário).
 * Usado em Configurações > Conexões e nos Primeiros passos.
 */
import "./settings.css";
import { useId, useState } from "react";
import type { ConnectionView } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { ConnectionForm } from "./ConnectionForm";
import { ChatgptButton } from "./PlanConnection";

interface NewConnectionProps {
  existingIds: readonly string[];
  onSaved: (view: ConnectionView) => void;
  /** Sem conexão salva, não há para onde voltar: sem "Cancelar". */
  onCancel?: () => void;
}

export function NewConnection({ existingIds, onSaved, onCancel }: NewConnectionProps) {
  const { t } = useT();
  const headingId = useId();
  const [method, setMethod] = useState<"choose" | "api_key">("choose");

  if (method === "api_key") {
    return <ConnectionForm existingIds={existingIds} onSaved={onSaved} onCancel={() => setMethod("choose")} />;
  }

  return (
    <div className="nd-stack-sm" data-connection-method="">
      <h4 id={headingId} className="nd-section-label">
        {t("connection.method.title")}
      </h4>
      <ul className="nd-conn-methods" aria-labelledby={headingId}>
        <li>
          <button type="button" className="nd-conn-method" data-method="api_key" onClick={() => setMethod("api_key")}>
            <span className="nd-row">
              <Icon name="key" size={16} />
              <span className="nd-strong">{t("connection.method.apiKey")}</span>
              <Icon name="chevron-right" size={14} className="nd-conn-method-arrow" />
            </span>
            <span className="nd-field-hint">{t("connection.method.apiKeyHint")}</span>
          </button>
        </li>
        <li className="nd-conn-method" data-method="chatgpt_plan">
          <span className="nd-row">
            <Icon name="sparkles" size={16} />
            <span className="nd-strong">{t("connection.method.plan")}</span>
          </span>
          <span className="nd-field-hint">{t("connection.method.planHint")}</span>
          <ChatgptButton />
        </li>
      </ul>
      {onCancel !== undefined ? (
        <div className="nd-dialog-actions">
          <button type="button" className="nd-btn" onClick={onCancel}>
            {t("common.cancel")}
          </button>
        </div>
      ) : null}
    </div>
  );
}
