/*
 * Aviso de erro da barra superior (pílula de execução, YOLO, Internet, carga do espaço):
 * `role="alert"` logo abaixo da barra, com uma ação opcional ("Tentar de novo") e "Dispensar".
 */
import { createPortal } from "react-dom";
import { useT } from "../../i18n/react";
import { Icon } from "../Icon";
import { useDesktopLayer } from "../layer";

interface BarAlertProps {
  message: string;
  action?: { label: string; onClick: () => void };
  onDismiss?: () => void;
  /** Identifica a origem (testes e estilo). */
  source: string;
}

export function BarAlert({ message, action, onDismiss, source }: BarAlertProps) {
  const { t } = useT();
  const layer = useDesktopLayer();
  const node = (
    <div className="nd-bar-alert nd-overlay" role="alert" data-bar-alert={source}>
      <Icon name="alert" size={16} className="nd-bar-alert-icon" />
      <span className="nd-bar-alert-text">{message}</span>
      {action !== undefined ? (
        <button type="button" className="nd-btn" onClick={action.onClick}>
          {action.label}
        </button>
      ) : null}
      {onDismiss !== undefined ? (
        <button type="button" className="nd-wbtn" aria-label={t("common.dismiss")} onClick={onDismiss}>
          <Icon name="close" size={14} />
        </button>
      ) : null}
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}
