/*
 * Diálogo modal do desktop: desenhado na camada de sobreposição (dentro de `.nd-root`, com as
 * variáveis do tema). Foco inicial no primeiro controle (ou no `initialFocus`), Tab preso no
 * painel, Esc fecha (e não chega aos atalhos do desktop), e o foco volta a quem abriu.
 */
import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { useT } from "../i18n/react";
import { Icon } from "./Icon";
import { FOCUSABLE, trapTab } from "./focusTrap";
import { useDesktopLayer } from "./layer";

interface DialogProps {
  titleId: string;
  title: ReactNode;
  describedById?: string;
  onClose: () => void;
  /** Largura do painel em px (padrão 440). */
  width?: number;
  initialFocus?: RefObject<HTMLElement | null>;
  /** Papel `alertdialog` para confirmações destrutivas. */
  alert?: boolean;
  children: ReactNode;
}

export function Dialog({ titleId, title, describedById, onClose, width = 440, initialFocus, alert = false, children }: DialogProps) {
  const { t } = useT();
  const layer = useDesktopLayer();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const panel = panelRef.current;
    const target = initialFocus?.current ?? panel?.querySelector<HTMLElement>(FOCUSABLE) ?? panel;
    target?.focus({ preventScroll: true });
    return () => {
      if (opener?.isConnected === true) opener.focus({ preventScroll: true });
    };
    // Foco inicial e devolução: só na montagem.
  }, []);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeRef.current();
      return;
    }
    trapTab(event, panelRef.current);
  };

  const node = (
    <div className="nd-dialog-backdrop" data-dialog-backdrop="" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        ref={panelRef}
        role={alert ? "alertdialog" : "dialog"}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedById}
        tabIndex={-1}
        className="nd-dialog"
        style={{ width: `min(${width}px, calc(100% - 32px))` }}
        onKeyDown={onKeyDown}
      >
        <div className="nd-popover-head">
          <h2 id={titleId} className="nd-popover-title">
            {title}
          </h2>
          <button type="button" className="nd-wbtn" aria-label={t("common.close")} onClick={onClose}>
            <Icon name="close" size={14} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}
