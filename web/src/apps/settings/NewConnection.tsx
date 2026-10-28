/*
 * Nova conexão (spec 116 T03, decisão 7; spec 132 T10): o primeiro passo pergunta como conectar,
 * com um cartão por provedor e uma linha de dica em cada.
 * - "OpenAI ou compatível (chave de API)" (OpenAI, Azure, OpenRouter, servidores locais): segue para
 *   o formulário de sempre, com o endereço e o protocolo.
 * - "Anthropic (Claude)" e "Google Gemini (AI Studio)": seguem para o formulário nativo, só com o
 *   nome e a chave (`NativeConnectionForm`).
 * - "Plano ChatGPT": uma frase curta e o botão "Continue with ChatGPT". Sem lista do que funciona
 *   ou não (decisão do usuário).
 * Usado em Configurações > Conexões e nos Primeiros passos.
 */
import "./settings.css";
import { useId, useState } from "react";
import type { ConnectionView } from "../../api/types";
import { Icon, type IconName } from "../../desktop/Icon";
import type { MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import type { NativeKind } from "../../settings/rules";
import { ConnectionForm } from "./ConnectionForm";
import { NativeConnectionForm } from "./NativeConnectionForm";
import { ChatgptButton } from "./PlanConnection";

interface NewConnectionProps {
  existingIds: readonly string[];
  onSaved: (view: ConnectionView) => void;
  /** Sem conexão salva, não há para onde voltar: sem "Cancelar". */
  onCancel?: () => void;
}

type Method = "choose" | "api_key" | NativeKind;

/** Os cartões que levam a um formulário, na ordem da tela (o do plano vem por último). */
const FORM_METHODS: readonly { method: Exclude<Method, "choose">; icon: IconName; label: MessageKey; hint: MessageKey }[] = [
  { method: "api_key", icon: "key", label: "connection.method.apiKey", hint: "connection.method.apiKeyHint" },
  { method: "anthropic", icon: "message", label: "connection.method.anthropic", hint: "connection.method.anthropicHint" },
  { method: "google_gemini", icon: "lightbulb", label: "connection.method.gemini", hint: "connection.method.geminiHint" },
];

export function NewConnection({ existingIds, onSaved, onCancel }: NewConnectionProps) {
  const { t } = useT();
  const headingId = useId();
  const [method, setMethod] = useState<Method>("choose");

  if (method === "api_key") {
    return <ConnectionForm existingIds={existingIds} onSaved={onSaved} onCancel={() => setMethod("choose")} />;
  }
  if (method === "anthropic" || method === "google_gemini") {
    return <NativeConnectionForm key={method} kind={method} existingIds={existingIds} onSaved={onSaved} onCancel={() => setMethod("choose")} />;
  }

  return (
    <div className="nd-stack-sm" data-connection-method="">
      <h4 id={headingId} className="nd-section-label">
        {t("connection.method.title")}
      </h4>
      <ul className="nd-conn-methods" aria-labelledby={headingId}>
        {FORM_METHODS.map((item) => (
          <li key={item.method}>
            <button type="button" className="nd-conn-method" data-method={item.method} onClick={() => setMethod(item.method)}>
              <span className="nd-row">
                <Icon name={item.icon} size={16} />
                <span className="nd-strong">{t(item.label)}</span>
                <Icon name="chevron-right" size={14} className="nd-conn-method-arrow" />
              </span>
              <span className="nd-field-hint">{t(item.hint)}</span>
            </button>
          </li>
        ))}
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
