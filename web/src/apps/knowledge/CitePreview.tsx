/*
 * Prévia da citação `kb:` (spec 136 T04, critério 4): o documento, a seção e o trecho citado, mais
 * "Abrir na base". No desktop, um cartão ancorado na citação (abre ao passar o mouse ou ao focar
 * pelo teclado; Esc fecha e devolve o foco); no celular, uma folha inferior aberta pelo toque.
 * A altura do trecho é reservada desde o carregamento, para o cartão não pular nem piscar.
 */
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import { createPortal } from "react-dom";
import type { KbDoc } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { FOCUSABLE, trapTab } from "../../desktop/focusTrap";
import { useDesktopLayer } from "../../desktop/layer";
import { useT } from "../../i18n/react";
import { MarkdownText } from "../shared/ReportMarkdown";
import { useKbExcerpt } from "./kbCiteStore";
import { kindIcon } from "./knowledgeModel";

export interface CitePreviewTarget {
  projectId: string;
  doc: string;
  section: string | null;
  /** O documento na lista da base; `null` quando saiu dela. */
  kbDoc: KbDoc | null;
  /** Título exibido (o do documento, ou o id quando ele saiu da base). */
  title: string;
}

const CARD_WIDTH = 380;
const GAP = 6;

function ExcerptBody({ target, onOpen, sheet }: { target: CitePreviewTarget; onOpen: () => void; sheet: boolean }) {
  const { t } = useT();
  const removed = target.kbDoc === null;
  const state = useKbExcerpt(target.projectId, target.doc, target.section, !removed);
  const excerpt = state.status === "ok" ? state.excerpt : null;
  const trail = excerpt?.trail.filter((part) => part !== target.title && part !== excerpt.title) ?? [];
  return (
    <>
      <div className="nd-kb-card-doc">
        <Icon name={target.kbDoc === null ? "file" : kindIcon(target.kbDoc)} size={14} />
        <span className="nd-kb-card-doc-title">{target.title}</span>
        {excerpt?.page !== null && excerpt?.page !== undefined ? <span className="nd-kb-card-page">{t("kb.cite.page", { page: excerpt.page })}</span> : null}
      </div>
      {removed ? (
        <p className="nd-kb-card-note" data-tone="rose">
          <Icon name="alert" size={14} />
          <span>{t("kb.cite.removed")}</span>
        </p>
      ) : (
        <>
          <div className="nd-kb-card-heading">
            {trail.length > 0 ? <span className="nd-kb-card-trail">{trail.join(" › ")}</span> : null}
            <span className="nd-kb-card-title">{excerpt?.title ?? (target.section === null ? t("plan.sources.wholeDoc") : (excerpt?.title ?? target.section))}</span>
          </div>
          <div className="nd-kb-card-text" data-state={state.status} aria-busy={state.status === "loading"}>
            {state.status === "loading" ? (
              <div className="nd-kb-card-skeleton" role="status" aria-label={t("kb.cite.loading")}>
                <span />
                <span />
                <span />
              </div>
            ) : state.status === "error" ? (
              <p className="nd-muted">{t("kb.cite.error")}</p>
            ) : (
              <MarkdownText text={state.excerpt.text} />
            )}
          </div>
          {excerpt?.truncated === true && sheet ? <p className="nd-kb-card-cut">{t("kb.cite.truncated")}</p> : null}
        </>
      )}
      <div className="nd-kb-card-foot">
        {!removed ? (
          <button type="button" className="nd-btn nd-btn-sm nd-kb-card-open" data-variant={sheet ? "primary" : undefined} onClick={onOpen}>
            <Icon name="knowledge" size={14} />
            {t("kb.cite.openInBase")}
            <Icon name="arrow-right" size={14} />
          </button>
        ) : null}
      </div>
    </>
  );
}

/** Posição do cartão: abaixo da citação, ou acima quando falta espaço; sem sair da raiz. */
function placement(anchor: HTMLElement | null, layer: HTMLElement | null): { left: number; top: number; above: boolean } {
  if (anchor === null) return { left: 8, top: 48, above: false };
  const a = anchor.getBoundingClientRect();
  const root = layer?.getBoundingClientRect();
  const originX = root?.left ?? 0;
  const originY = root?.top ?? 0;
  const height = root?.height ?? window.innerHeight;
  const maxLeft = Math.max(8, (root?.width ?? window.innerWidth) - CARD_WIDTH - 8);
  const left = Math.min(Math.max(8, a.left - originX), maxLeft);
  const below = a.bottom - originY + GAP;
  const above = below + 320 > height && a.top - originY > 340;
  return { left, top: above ? a.top - originY - GAP : below, above };
}

interface CardProps {
  target: CitePreviewTarget;
  anchor: RefObject<HTMLElement | null>;
  onOpen: () => void;
  onClose: (returnFocus: boolean) => void;
  onHover: (inside: boolean) => void;
}

/** Cartão do desktop, na camada de sobreposição. */
export function CiteCard({ target, anchor, onOpen, onClose, onHover }: CardProps) {
  const { t } = useT();
  const layer = useDesktopLayer();
  const [pos, setPos] = useState(() => placement(anchor.current, layer));
  useLayoutEffect(() => {
    setPos(placement(anchor.current, layer));
  }, [anchor, layer]);
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    onClose(true);
  };
  const node = (
    <div
      role="dialog"
      aria-label={t("kb.cite.label", { doc: target.title })}
      className="nd-kb-card nd-overlay"
      data-kb-card=""
      data-above={pos.above ? "true" : undefined}
      style={{ left: pos.left, top: pos.top, width: CARD_WIDTH }}
      onKeyDown={onKeyDown}
      onMouseEnter={() => onHover(true)}
      onMouseLeave={() => onHover(false)}
    >
      <ExcerptBody target={target} onOpen={onOpen} sheet={false} />
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}

/**
 * Folha inferior do celular, modal: fundo escurecido, foco no primeiro controle e Tab preso nela
 * (o mesmo do `Dialog`); Esc, o botão de fechar ou o toque fora fecham, e quem abriu devolve o foco
 * à citação.
 */
export function CiteSheet({ target, onOpen, onClose }: { target: CitePreviewTarget; onOpen: () => void; onClose: () => void }) {
  const { t } = useT();
  const layer = useDesktopLayer();
  const panel = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const target = panel.current?.querySelector<HTMLElement>(FOCUSABLE) ?? panel.current;
    target?.focus({ preventScroll: true });
  }, []);
  const node = (
    <div className="nd-kb-sheet-backdrop" data-kb-sheet-backdrop="" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label={t("kb.cite.label", { doc: target.title })}
        tabIndex={-1}
        className="nd-kb-sheet"
        data-kb-sheet=""
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            onClose();
            return;
          }
          trapTab(event, panel.current);
        }}
      >
        <span className="nd-kb-sheet-grab" aria-hidden="true" />
        <button type="button" className="nd-wbtn nd-kb-sheet-close" aria-label={t("common.close")} onClick={onClose}>
          <Icon name="close" size={14} />
        </button>
        <ExcerptBody target={target} onOpen={onOpen} sheet />
      </div>
    </div>
  );
  return layer === null ? node : createPortal(node, layer);
}
