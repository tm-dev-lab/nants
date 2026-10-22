/*
 * Folha de atalhos (Shift+/), gerada da tabela única `SHORTCUTS` (spec 093, decisão 4).
 * Esc ou o botão fechar a fecham; o Desktop devolve o foco a quem a abriu.
 */
import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { useT } from "../i18n/react";
import { Icon } from "./Icon";
import { SHORTCUTS } from "./shortcuts";

interface ShortcutSheetProps {
  onClose: () => void;
}

export function ShortcutSheet({ onClose }: ShortcutSheetProps) {
  const { t } = useT();
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    ref.current?.focus({ preventScroll: true });
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
      aria-labelledby="nd-shortcuts-title"
      tabIndex={-1}
      className="nd-sheet nd-popover nd-overlay"
      data-shortcut-sheet=""
      onKeyDown={onKeyDown}
    >
      <div className="nd-popover-head">
        <h2 id="nd-shortcuts-title" className="nd-popover-title">
          {t("desktop.shortcuts")}
        </h2>
        <button type="button" className="nd-wbtn" aria-label={t("common.close")} onClick={onClose}>
          <Icon name="close" size={14} />
        </button>
      </div>
      <dl className="nd-shortcuts">
        {SHORTCUTS.map((shortcut) => (
          <div key={shortcut.id} style={{ display: "contents" }}>
            <dt>
              <kbd>{shortcut.keys}</kbd>
            </dt>
            <dd>{shortcut.description}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
