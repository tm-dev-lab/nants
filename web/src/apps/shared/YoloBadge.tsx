/* Selo discreto da aprovação automática (antigo "YOLO"; o nome do componente segue a chave interna `yolo`). */
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";

export function YoloBadge({ decorative = false }: { decorative?: boolean }) {
  const { t } = useT();
  return (
    <span aria-hidden={decorative ? "true" : undefined} data-testid="yolo-badge" className="nd-pill nd-pill-sm" data-tone="amber">
      <Icon name="zap" size={14} />
      {t("autoApprove.label")}
    </span>
  );
}
