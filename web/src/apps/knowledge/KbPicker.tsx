/*
 * Seletor de documento e seção da base (spec 136 T06, C-11): um componente só, usado pelo `@` da
 * Conversa e pela busca rápida da janela Conhecimento (T07).
 *
 * - **Documentos:** o filtro roda no cliente, sem acento (`kbPickerModel`); cada linha tem o ícone
 *   do tipo, o título (com o trecho casado em destaque) e a origem.
 * - **Seção (opcional):** Tab, seta para a direita ou a seta da linha abrem o sumário do documento
 *   em níveis, com "Documento inteiro" no topo. A consulta da seção vem de quem usa o seletor.
 * - **Teclado:** quem usa o seletor mantém o foco no próprio campo e repassa as teclas
 *   ({@link kbPickerKey}); a lista segue o padrão listbox com `aria-activedescendant`. O toque e o
 *   clique escolhem sem tirar o foco do campo (o teclado do celular continua aberto).
 */
import "./knowledge.css";
import { useMemo, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import type { KbDoc, KbSection } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { useKbSections } from "./kbCiteStore";
import { docOrigin, filterKbDocs, filterKbSections, hasSections, type MatchRange } from "./kbPickerModel";
import { kindIcon } from "./knowledgeModel";

type KbPickerItem =
  | { kind: "doc"; doc: KbDoc; ranges: MatchRange[]; sections: boolean }
  | { kind: "whole"; doc: KbDoc }
  | { kind: "section"; doc: KbDoc; section: KbSection; ranges: MatchRange[]; depth: number };

export interface KbPickerChoice {
  doc: KbDoc;
  section: KbSection | null;
}

interface KbPickerState {
  items: KbPickerItem[];
  /** O documento do passo da seção, ou `null` no passo dos documentos. */
  sectionDoc: KbDoc | null;
  sectionsStatus: "loading" | "ok" | "error" | null;
  /** Consulta em uso (a dos documentos ou a da seção). */
  query: string;
}

/** Itens do seletor para a consulta e o passo pedidos. */
export function useKbPicker(projectId: string, docs: readonly KbDoc[], query: string, sectionOf: { doc: KbDoc; query: string } | null): KbPickerState {
  // O memo depende do documento e da consulta, não do objeto `sectionOf`, que quem chama recria a
  // cada render (observação da T06).
  const sectionDoc = sectionOf?.doc ?? null;
  const sectionQuery = sectionOf?.query ?? "";
  const outline = useKbSections(projectId, sectionDoc?.id ?? null);
  return useMemo(() => {
    if (sectionDoc === null) {
      const items: KbPickerItem[] = filterKbDocs(docs, query).map(({ doc, ranges }) => ({ kind: "doc", doc, ranges, sections: hasSections(doc) }));
      return { items, sectionDoc: null, sectionsStatus: null, query };
    }
    const doc = sectionDoc;
    const items: KbPickerItem[] = sectionQuery.trim() === "" ? [{ kind: "whole", doc }] : [];
    if (outline.status === "ok") {
      const top = Math.min(...outline.sections.map((s) => s.level), 6);
      for (const { section, ranges } of filterKbSections(outline.sections, sectionQuery)) {
        items.push({ kind: "section", doc, section, ranges, depth: Math.max(0, section.level - top) });
      }
    }
    return { items, sectionDoc: doc, sectionsStatus: outline.status, query: sectionQuery };
  }, [docs, query, sectionDoc, sectionQuery, outline]);
}

/** O que a tecla faz no seletor aberto; `null` deixa a tecla para o campo. */
type PickerAction = { kind: "move"; index: number } | { kind: "choose"; item: KbPickerItem } | { kind: "sections"; doc: KbDoc } | { kind: "back" } | { kind: "close" };

/**
 * Teclas do seletor (C-11): setas movem; Enter escolhe só com uma opção destacada; Tab e a seta
 * para a direita abrem as seções do documento destacado (ou escolhem a seção); a seta para a
 * esquerda e o Backspace com a consulta da seção vazia voltam aos documentos; Esc fecha.
 */
export function kbPickerKey(event: Pick<ReactKeyboardEvent, "key" | "shiftKey">, state: KbPickerState, active: number): PickerAction | null {
  const count = state.items.length;
  const current = active >= 0 && active < count ? state.items[active] : undefined;
  switch (event.key) {
    case "ArrowDown":
      return count === 0 ? null : { kind: "move", index: active < 0 ? 0 : (active + 1) % count };
    case "ArrowUp":
      return count === 0 ? null : { kind: "move", index: active <= 0 ? count - 1 : active - 1 };
    case "Enter":
      if (event.shiftKey || current === undefined) return null;
      return { kind: "choose", item: current };
    case "Tab":
      if (event.shiftKey || current === undefined) return null;
      if (current.kind === "doc") return current.sections ? { kind: "sections", doc: current.doc } : { kind: "choose", item: current };
      return { kind: "choose", item: current };
    case "ArrowRight":
      return current?.kind === "doc" && current.sections && state.sectionDoc === null ? { kind: "sections", doc: current.doc } : null;
    case "ArrowLeft":
    case "Backspace":
      return state.sectionDoc !== null && state.query === "" ? { kind: "back" } : null;
    case "Escape":
      return { kind: "close" };
    default:
      return null;
  }
}

/** Escolha do item: o documento, o documento inteiro ou a seção. */
function choiceOf(item: KbPickerItem): KbPickerChoice {
  return item.kind === "section" ? { doc: item.doc, section: item.section } : { doc: item.doc, section: null };
}

function Highlighted({ text, ranges }: { text: string; ranges: readonly MatchRange[] }) {
  if (ranges.length === 0) return <>{text}</>;
  const out: ReactNode[] = [];
  let last = 0;
  ranges.forEach(([from, to], i) => {
    const start = Math.max(from, last);
    if (start > last) out.push(text.slice(last, start));
    if (to > start) out.push(<mark key={i} className="nd-kb-picker-hit">{text.slice(start, to)}</mark>);
    last = Math.max(last, to);
  });
  if (last < text.length) out.push(text.slice(last));
  return <>{out}</>;
}

interface KbPickerProps {
  state: KbPickerState;
  listId: string;
  active: number;
  onActive: (index: number) => void;
  onChoose: (choice: KbPickerChoice) => void;
  onSections: (doc: KbDoc) => void;
  onBack: () => void;
  /**
   * `popover`: flutua ancorado no cursor (desktop); `panel`: faixa acima do teclado (celular);
   * `embedded`: um grupo dentro do painel da busca rápida (T07), sem sobreposição nem rodapé.
   */
  variant: "popover" | "panel" | "embedded";
  /** Título do cabeçalho no passo dos documentos (padrão: "Base de conhecimento"). */
  title?: string;
  /** O que o Enter faz, no rodapé do `popover` (padrão: "mencionar"). */
  chooseHint?: string;
  style?: CSSProperties;
}

/** Id da opção (para o `aria-activedescendant` do campo). */
export function kbPickerOptionId(listId: string, index: number): string {
  return `${listId}-opt-${index}`;
}

/** Evita que o toque ou o clique tire o foco do campo (o teclado do celular fica aberto). */
const keepFocus = (event: { preventDefault: () => void }): void => event.preventDefault();

export function KbPicker({ state, listId, active, onActive, onChoose, onSections, onBack, variant, title, chooseHint, style }: KbPickerProps) {
  const { t, tp } = useT();
  const { items, sectionDoc } = state;
  const label = sectionDoc === null ? t("kb.picker.label") : t("kb.picker.sectionsLabel", { doc: sectionDoc.title });
  return (
    <div className={variant === "embedded" ? "nd-kb-picker" : "nd-kb-picker nd-overlay"} data-variant={variant} data-step={sectionDoc === null ? "docs" : "sections"} data-kb-picker="" style={style}>
      <div className="nd-kb-picker-head">
        {sectionDoc === null ? (
          <>
            <Icon name="knowledge" size={14} />
            <span className="nd-kb-picker-head-title">{title ?? t("kb.picker.title")}</span>
            <span className="nd-kb-picker-count">{tp("kb.picker.results", items.length)}</span>
          </>
        ) : (
          <>
            <button
              type="button"
              tabIndex={-1}
              className="nd-wbtn nd-kb-picker-back"
              aria-label={t("kb.picker.back")}
              title={t("kb.picker.back")}
              onPointerDown={keepFocus}
              onMouseDown={keepFocus}
              onClick={onBack}
            >
              <Icon name="arrow-left" size={14} />
            </button>
            <Icon name={kindIcon(sectionDoc)} size={14} />
            <span className="nd-kb-picker-head-title">{sectionDoc.title}</span>
          </>
        )}
      </div>
      <ul id={listId} role="listbox" aria-label={label} className="nd-kb-picker-list">
        {items.map((item, index) => (
          <li
            key={item.kind === "section" ? `s:${item.section.anchor}` : `${item.kind}:${item.doc.id}`}
            id={kbPickerOptionId(listId, index)}
            role="option"
            aria-selected={index === active}
            data-active={index === active ? "true" : undefined}
            data-kind={item.kind}
            className="nd-kb-picker-item"
            data-depth={item.kind === "section" ? Math.min(item.depth, 3) : undefined}
            onPointerDown={keepFocus}
            onMouseDown={keepFocus}
            onMouseMove={() => {
              if (index !== active) onActive(index);
            }}
            onClick={() => onChoose(choiceOf(item))}
          >
            {item.kind === "doc" ? (
              <>
                <span className="nd-kb-picker-icon" aria-hidden="true">
                  <Icon name={kindIcon(item.doc)} size={14} />
                </span>
                <span className="nd-kb-picker-text">
                  <span className="nd-kb-picker-title">
                    <Highlighted text={item.doc.title} ranges={item.ranges} />
                  </span>
                  <span className="nd-kb-picker-origin">{docOrigin(item.doc)}</span>
                </span>
                {item.sections ? (
                  <span
                    className="nd-kb-picker-more"
                    aria-hidden="true"
                    title={t("kb.picker.showSections", { doc: item.doc.title })}
                    onClick={(event) => {
                      event.stopPropagation();
                      onSections(item.doc);
                    }}
                  >
                    <Icon name="chevron-right" size={14} />
                  </span>
                ) : null}
              </>
            ) : item.kind === "whole" ? (
              <>
                <span className="nd-kb-picker-icon" aria-hidden="true">
                  <Icon name={kindIcon(item.doc)} size={14} />
                </span>
                <span className="nd-kb-picker-text">
                  <span className="nd-kb-picker-title">{t("kb.picker.wholeDoc")}</span>
                </span>
              </>
            ) : (
              <span className="nd-kb-picker-text">
                <span className="nd-kb-picker-title">
                  <span className="nd-kb-picker-hash" aria-hidden="true">
                    #
                  </span>
                  <Highlighted text={item.section.title} ranges={item.ranges} />
                </span>
              </span>
            )}
          </li>
        ))}
      </ul>
      {sectionDoc !== null && state.sectionsStatus === "loading" ? (
        <div className="nd-kb-picker-skeleton" role="status" aria-label={t("kb.picker.loadingSections")}>
          <span />
          <span />
          <span />
        </div>
      ) : null}
      {sectionDoc !== null && state.sectionsStatus === "error" ? <p className="nd-kb-picker-note">{t("kb.picker.sectionsError")}</p> : null}
      {sectionDoc !== null && state.sectionsStatus === "ok" && items.length === 0 ? (
        <p className="nd-kb-picker-note">{t("kb.picker.noSection", { query: state.query })}</p>
      ) : null}
      {variant === "popover" ? (
        <div className="nd-kb-picker-foot" aria-hidden="true">
          <span>
            <kbd>Enter</kbd> {chooseHint ?? t("kb.picker.hintMention")}
          </span>
          {sectionDoc === null ? (
            <span>
              <kbd>Tab</kbd> {t("kb.picker.hintSections")}
            </span>
          ) : null}
          <span>
            <kbd>Esc</kbd> {t("kb.picker.hintClose")}
          </span>
        </div>
      ) : null}
    </div>
  );
}
