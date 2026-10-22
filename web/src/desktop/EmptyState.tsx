/* Estado vazio padrão de uma janela (spec 093, decisão 3): ícone, uma frase e uma ação. */
import { Icon, type IconName } from "./Icon";

export interface EmptyAction {
  label: string;
  onClick: () => void;
}

interface EmptyStateProps {
  icon: IconName;
  message: string;
  action?: EmptyAction;
}

export function EmptyState({ icon, message, action }: EmptyStateProps) {
  return (
    <div className="nd-empty" data-empty-state>
      <Icon name={icon} size={20} className="nd-empty-icon" />
      <p>{message}</p>
      {action !== undefined ? (
        <button type="button" className="nd-btn" onClick={action.onClick}>
          {action.label}
        </button>
      ) : null}
    </div>
  );
}
