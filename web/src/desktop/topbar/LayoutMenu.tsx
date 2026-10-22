/*
 * Menu Layout (spec 093, decisão 5): aplicar o meu layout, aplicar o padrão, salvar como meu
 * layout desta fase e restaurar o padrão. Toda mudança de arranjo pode ser desfeita (o aviso traz
 * "Desfazer" por 8 s; Ctrl+Z também desfaz).
 */
import { useRef, useState } from "react";
import { useT } from "../../i18n/react";
import { Icon } from "../Icon";
import { PopupMenu } from "../PopupMenu";
import { CATEGORY_LABELS, type LayoutCategory } from "../presets";

export type LayoutCommand = "applyMine" | "applyDefault" | "saveMine" | "restoreDefault";

interface LayoutMenuProps {
  category: LayoutCategory;
  /** Há um "meu layout" salvo para a categoria. */
  hasMine: boolean;
  onCommand: (command: LayoutCommand) => void;
}

export function LayoutMenu({ category, hasMine, onCommand }: LayoutMenuProps) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const close = (returnFocus: boolean): void => {
    setOpen(false);
    if (returnFocus) buttonRef.current?.focus({ preventScroll: true });
  };
  const run = (command: LayoutCommand): void => {
    close(true);
    onCommand(command);
  };
  const phaseName = CATEGORY_LABELS[category];
  return (
    <>
      <button
        ref={buttonRef}
        type="button"
        className="nd-btn nd-bar-btn"
        data-variant="ghost"
        aria-haspopup="menu"
        aria-expanded={open}
        data-layout-menu=""
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="layout" size={16} />
        <span className="nd-bar-text">{t("layout.button", { phase: phaseName })}</span>
      </button>
      {open ? (
        <PopupMenu label={t("layout.menu")} anchor={buttonRef} onClose={close} width={300}>
          <div className="nd-menu-label">{t("layout.heading", { phase: phaseName })}</div>
          <button type="button" role="menuitem" className="nd-menu-item" onClick={() => run("applyMine")}>
            <Icon name="layout" size={16} />
            <span className="nd-menu-text">{t("layout.applyMine")}</span>
            {!hasMine ? <span className="nd-menu-hint">{t("layout.sameAsDefault")}</span> : null}
          </button>
          <button type="button" role="menuitem" className="nd-menu-item" onClick={() => run("applyDefault")}>
            <Icon name="monitor" size={16} />
            <span className="nd-menu-text">{t("layout.applyDefault")}</span>
          </button>
          <div className="nd-menu-sep" role="separator" />
          <button type="button" role="menuitem" className="nd-menu-item" onClick={() => run("saveMine")}>
            <Icon name="save" size={16} />
            <span className="nd-menu-text">{t("layout.saveMine")}</span>
          </button>
          <button type="button" role="menuitem" className="nd-menu-item" onClick={() => run("restoreDefault")}>
            <Icon name="undo" size={16} />
            <span className="nd-menu-text">{t("layout.restoreDefault")}</span>
          </button>
        </PopupMenu>
      ) : null}
    </>
  );
}
