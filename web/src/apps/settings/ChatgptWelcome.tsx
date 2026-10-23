/*
 * Boas-vindas do plano ChatGPT (spec 116 T03): aparecem uma vez só, depois do primeiro login pelo
 * plano, com "Gerenciar uso" e "Entendi". O shell guarda que já foram vistas (seção global
 * `chatgpt` do ui-state).
 */
import { useId, useRef } from "react";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { PLAN_USAGE_URL } from "./chatgptPlan";

export function ChatgptWelcome({ onClose }: { onClose: () => void }) {
  const { t } = useT();
  const titleId = useId();
  const textId = useId();
  const okRef = useRef<HTMLButtonElement | null>(null);
  return (
    <Dialog titleId={titleId} title={t("chatgpt.welcome.title")} describedById={textId} onClose={onClose} initialFocus={okRef}>
      <div className="nd-stack-sm" data-chatgpt-welcome="">
        <p id={textId} className="nd-muted">
          {t("chatgpt.welcome.text")}
        </p>
        <div className="nd-dialog-actions">
          <a className="nd-btn" data-variant="ghost" href={PLAN_USAGE_URL} target="_blank" rel="noopener noreferrer" data-plan-usage="">
            <Icon name="external" size={14} />
            {t("chatgpt.manageUsage")}
          </a>
          <button ref={okRef} type="button" className="nd-btn" data-variant="primary" onClick={onClose}>
            {t("chatgpt.welcome.ok")}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
