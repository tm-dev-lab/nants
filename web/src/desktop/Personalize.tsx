/*
 * Personalizar (spec 093, decisão 13): tema Escuro, Claro ou Automático e o papel de parede
 * (Pontos, Nebulosa, Aurora, Liso). O Desktop grava a escolha na seção global `appearance`.
 * As escolhas (`AppearanceChoices`) são as mesmas da aba Aparência das Configurações.
 */
import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useT } from "../i18n/react";
import { AppearanceChoices } from "./AppearanceChoices";
import { Icon } from "./Icon";
import type { Appearance } from "./theme/appearance";
import type { ThemeName } from "./theme/tokens";

interface PersonalizeProps {
  appearance: Appearance;
  /** Tema efetivo (as miniaturas dos papéis de parede seguem ele). */
  theme: ThemeName;
  onChange: (next: Appearance) => void;
  onClose: () => void;
}

export function Personalize({ appearance, theme, onChange, onClose }: PersonalizeProps) {
  const { t } = useT();
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>("[aria-pressed='true']")?.focus({ preventScroll: true });
  }, []);
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    onClose();
  };
  return (
    <div
      ref={ref}
      role="dialog"
      aria-labelledby="nd-personalize-title"
      className="nd-popover nd-overlay"
      data-personalize=""
      style={{ right: 10, top: 52, width: 340 }}
      onKeyDown={onKeyDown}
    >
      <div className="nd-popover-head">
        <h2 id="nd-personalize-title" className="nd-popover-title">
          {t("desktop.personalize")}
        </h2>
        <button type="button" className="nd-wbtn" aria-label={t("common.close")} onClick={onClose}>
          <Icon name="close" size={14} />
        </button>
      </div>
      <AppearanceChoices appearance={appearance} theme={theme} onChange={onChange} />
    </div>
  );
}
