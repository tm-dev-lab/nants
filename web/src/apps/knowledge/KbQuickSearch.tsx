/*
 * Busca rápida da janela Conhecimento (spec 136 T07, G8), no espírito do "quick switcher" do
 * Obsidian:
 * - **Títulos:** o filtro instantâneo no cliente é o seletor da T06 (`KbPicker`, C-11), com o passo
 *   das seções pelo Tab;
 * - **No texto:** a rota da T03, com espera curta, mínimo de letras e cancelamento
 *   (`useKbTextSearch`); cada resultado mostra o documento, o caminho da seção e o trecho com os
 *   termos marcados por `<mark>` montado com nós de texto (C-10), nunca HTML;
 * - **Teclado:** o foco fica no campo (combobox com `aria-activedescendant`); as setas passam pelos
 *   dois grupos, Enter abre, Tab abre as seções do documento, Esc fecha e, de novo, limpa; `/`
 *   na janela leva ao campo.
 * No desktop, os resultados flutuam sob o campo; estreito (celular), ocupam o corpo da janela.
 */
import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import type { KbDoc, KbSearchHit } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { KbPicker, kbPickerKey, kbPickerOptionId, useKbPicker, type KbPickerChoice } from "./KbPicker";
import { hitTrail, KB_SEARCH_MIN_CHARS, snippetParts, useKbTextSearch } from "./kbSearchModel";
import { kindIcon } from "./knowledgeModel";

/** Títulos mostrados junto com o texto (o resto fica a uma tecla, sem empurrar os trechos). */
const MAX_TITLES_WITH_TEXT = 4;
/** Títulos mostrados sem consulta (o campo focado e vazio). */
const MAX_TITLES_IDLE = 6;

/** O trecho com os termos achados: nós de texto e `<mark>`, sem HTML do servidor. */
export function Snippet({ text, highlights }: { text: string; highlights: readonly (readonly [number, number])[] }) {
  const parts = useMemo(() => snippetParts(text, highlights), [text, highlights]);
  return (
    <>
      {parts.map((part, i) =>
        part.hit ? (
          <mark key={i} className="nd-kb-hit">
            {part.text}
          </mark>
        ) : (
          part.text
        ),
      )}
    </>
  );
}

function HitRow({ hit, doc, id, active, onActive, onChoose }: { hit: KbSearchHit; doc: KbDoc | undefined; id: string; active: boolean; onActive: () => void; onChoose: () => void }) {
  const { t } = useT();
  const trail = hitTrail(hit);
  const section = hit.title !== hit.doc_title ? hit.title : null;
  return (
    <li
      id={id}
      role="option"
      aria-selected={active}
      data-active={active ? "true" : undefined}
      className="nd-kb-hit-row"
      data-kb-hit={`${hit.doc}#${hit.anchor}`}
      onPointerDown={(e) => e.preventDefault()}
      onMouseDown={(e) => e.preventDefault()}
      onMouseMove={() => {
        if (!active) onActive();
      }}
      onClick={onChoose}
    >
      <span className="nd-kb-picker-icon" aria-hidden="true">
        <Icon name={doc === undefined ? "file" : kindIcon(doc)} size={14} />
      </span>
      <span className="nd-kb-hit-main">
        <span className="nd-kb-hit-where">
          <span className="nd-kb-hit-doc">{hit.doc_title}</span>
          {trail.map((part, i) => (
            <span key={i} className="nd-kb-hit-trail">
              {part}
            </span>
          ))}
          {section !== null ? <span className="nd-kb-hit-section">{section}</span> : null}
          {hit.page !== null ? <span className="nd-kb-hit-page">{t("kb.cite.page", { page: hit.page })}</span> : null}
        </span>
        <span className="nd-kb-hit-snippet">
          <Snippet text={hit.snippet} highlights={hit.highlights} />
        </span>
      </span>
    </li>
  );
}

interface QuickSearchProps {
  projectId: string;
  docs: readonly KbDoc[];
  /** `popover`: os resultados flutuam sob o campo (desktop); `panel`: ocupam o corpo (estreito). */
  variant: "popover" | "panel";
  /** Abre o documento (e a seção) na prévia. */
  onOpen: (doc: string, section: string | null) => void;
  /** Avisa quando os resultados abrem ou fecham (o estreito troca a lista pelo painel). */
  onOpenChange?: (open: boolean) => void;
  inputRef?: RefObject<HTMLInputElement | null>;
}

export function KbQuickSearch({ projectId, docs, variant, onOpen, onOpenChange, inputRef }: QuickSearchProps) {
  const { t, tp } = useT();
  const ownRef = useRef<HTMLInputElement | null>(null);
  const input = inputRef ?? ownRef;
  const listId = useId();
  const textListId = useId();
  const [query, setQuery] = useState("");
  const [sectionOf, setSectionOf] = useState<{ doc: KbDoc; query: string } | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);

  const picker = useKbPicker(projectId, docs, sectionOf === null ? query : "", sectionOf);
  const text = useKbTextSearch(projectId, sectionOf === null ? query : "");
  const q = query.trim();
  const titleCap = sectionOf !== null ? Number.POSITIVE_INFINITY : q === "" ? MAX_TITLES_IDLE : text.status === "idle" ? MAX_TITLES_IDLE : MAX_TITLES_WITH_TEXT;
  const titles = useMemo(() => ({ ...picker, items: picker.items.slice(0, titleCap) }), [picker, titleCap]);
  const hits = sectionOf === null && text.status !== "idle" && text.status !== "error" ? text.results : [];
  const count = titles.items.length + hits.length;

  useEffect(() => {
    onOpenChange?.(open);
  }, [open, onOpenChange]);

  // Resultados novos: o primeiro fica destacado (Enter abre o melhor).
  useEffect(() => {
    setActive(0);
  }, [query, sectionOf?.doc.id, sectionOf?.query, text.status]);

  const close = (): void => {
    setOpen(false);
    setSectionOf(null);
  };

  const chooseTitle = (choice: KbPickerChoice): void => {
    onOpen(choice.doc.id, choice.section?.anchor ?? null);
    close();
    input.current?.blur();
  };
  const chooseHit = (hit: KbSearchHit): void => {
    onOpen(hit.doc, hit.anchor);
    close();
    input.current?.blur();
  };

  function onKeyDown(event: ReactKeyboardEvent<HTMLInputElement>): void {
    if (!open) {
      if (event.key === "Escape" && query !== "") {
        event.preventDefault();
        setQuery("");
        return;
      }
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setOpen(true);
      }
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      if (count === 0) return;
      event.preventDefault();
      const down = event.key === "ArrowDown";
      setActive((i) => (down ? (i + 1) % count : (i - 1 + count) % count));
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const hit = active >= titles.items.length ? hits[active - titles.items.length] : undefined;
      if (hit !== undefined) {
        chooseHit(hit);
        return;
      }
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (sectionOf !== null) setSectionOf(null);
      else close();
      return;
    }
    // Títulos: as teclas do seletor (Enter, Tab e setas laterais, voltar com Backspace).
    const action = kbPickerKey(event, titles, active < titles.items.length ? active : -1);
    if (action === null) return;
    event.preventDefault();
    if (action.kind === "choose") {
      const item = action.item;
      chooseTitle(item.kind === "section" ? { doc: item.doc, section: item.section } : { doc: item.doc, section: null });
    } else if (action.kind === "sections") setSectionOf({ doc: action.doc, query: "" });
    else if (action.kind === "back") setSectionOf(null);
    else if (action.kind === "close") close();
  }

  const activeId =
    !open || count === 0 ? undefined : active < titles.items.length ? kbPickerOptionId(listId, active) : `${textListId}-hit-${active - titles.items.length}`;
  const showTitles = titles.items.length > 0 || sectionOf !== null;
  const inText = sectionOf === null && Array.from(q).length >= KB_SEARCH_MIN_CHARS;
  const nothing = sectionOf === null && q !== "" && titles.items.length === 0 && text.status === "ok" && hits.length === 0;

  const panel = open ? (
    <div className={variant === "popover" ? "nd-kb-search-panel nd-overlay" : "nd-kb-search-panel"} data-variant={variant} data-kb-search-panel="" role="presentation">
      {nothing ? (
        <div className="nd-kb-search-empty" role="status">
          <Icon name="search" size={18} />
          <p className="nd-kb-search-empty-title">{t("kb.search.nothing", { query: q })}</p>
          <p className="nd-kb-search-empty-text">{t("kb.search.nothingHint")}</p>
        </div>
      ) : (
        <>
          {showTitles ? (
            <KbPicker
              state={titles}
              listId={listId}
              active={active < titles.items.length ? active : -1}
              onActive={setActive}
              onChoose={chooseTitle}
              onSections={(doc) => setSectionOf({ doc, query: "" })}
              onBack={() => setSectionOf(null)}
              variant="embedded"
              title={t("kb.search.titles")}
            />
          ) : null}
          {inText ? (
            <div className="nd-kb-search-group" data-kb-search-text="" aria-busy={text.status === "loading"}>
              <div className="nd-kb-picker-head">
                <Icon name="search" size={14} />
                <span className="nd-kb-picker-head-title">{t("kb.search.inText")}</span>
                {text.status === "ok" ? <span className="nd-kb-picker-count">{tp("kb.search.passages", hits.length)}</span> : null}
                {text.status === "loading" ? <span className="nd-kb-spin" role="status" aria-label={t("kb.search.searching")} /> : null}
              </div>
              {text.status === "loading" && hits.length === 0 ? (
                <div className="nd-kb-hit-skeleton" aria-hidden="true">
                  {[0, 1, 2].map((i) => (
                    <span key={i} className="nd-kb-hit-skeleton-row">
                      <span />
                      <span />
                    </span>
                  ))}
                </div>
              ) : null}
              {text.status === "error" ? <p className="nd-kb-picker-note">{t("kb.search.error")}</p> : null}
              {text.status === "ok" && hits.length === 0 ? <p className="nd-kb-picker-note">{t("kb.search.noText", { query: q })}</p> : null}
              {hits.length > 0 ? (
                <ul id={textListId} role="listbox" aria-label={t("kb.search.inText")} className="nd-kb-picker-list nd-kb-hit-list" data-stale={text.status === "loading" ? "true" : undefined}>
                  {hits.map((hit, i) => (
                    <HitRow
                      key={`${hit.doc}#${hit.anchor}`}
                      hit={hit}
                      doc={docs.find((d) => d.id === hit.doc)}
                      id={`${textListId}-hit-${i}`}
                      active={active === titles.items.length + i}
                      onActive={() => setActive(titles.items.length + i)}
                      onChoose={() => chooseHit(hit)}
                    />
                  ))}
                </ul>
              ) : null}
            </div>
          ) : sectionOf === null && q !== "" ? (
            <p className="nd-kb-picker-note nd-kb-search-min">{t("kb.search.minChars", { min: KB_SEARCH_MIN_CHARS })}</p>
          ) : null}
        </>
      )}
      {variant === "popover" ? (
        <div className="nd-kb-picker-foot" aria-hidden="true">
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> {t("kb.search.hintMove")}
          </span>
          <span>
            <kbd>Enter</kbd> {t("kb.search.hintOpen")}
          </span>
          {sectionOf === null ? (
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
  ) : null;

  return (
    <div className="nd-kb-search" data-variant={variant} data-open={open ? "true" : undefined} data-kb-search="">
      <div className="nd-kb-search-field" role="search">
        <Icon name="search" size={14} className="nd-kb-search-icon" />
        {sectionOf !== null ? (
          <span className="nd-kb-search-scope" title={t("kb.search.sectionOf", { doc: sectionOf.doc.title })}>
            <Icon name={kindIcon(sectionOf.doc)} size={14} />
            <span className="nd-kb-search-scope-title">{sectionOf.doc.title}</span>
            <Icon name="chevron-right" size={14} />
          </span>
        ) : null}
        <input
          ref={input}
          className="nd-kb-search-input"
          type="text"
          role="combobox"
          aria-label={t("kb.search.label")}
          aria-expanded={open}
          aria-controls={open ? listId : undefined}
          aria-activedescendant={activeId}
          aria-autocomplete="list"
          autoComplete="off"
          spellCheck={false}
          enterKeyHint="search"
          placeholder={sectionOf === null ? t("kb.search.placeholder") : t("kb.picker.wholeDoc")}
          value={sectionOf === null ? query : sectionOf.query}
          onChange={(e) => {
            const value = e.target.value;
            if (sectionOf === null) setQuery(value);
            else setSectionOf({ doc: sectionOf.doc, query: value });
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => close()}
          onKeyDown={onKeyDown}
        />
        {query !== "" || sectionOf !== null ? (
          <button
            type="button"
            className="nd-wbtn nd-kb-search-clear"
            aria-label={t("kb.search.clear")}
            title={t("kb.search.clear")}
            onPointerDown={(e) => e.preventDefault()}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => {
              setQuery("");
              setSectionOf(null);
              input.current?.focus();
            }}
          >
            <Icon name="close" size={14} />
          </button>
        ) : variant === "popover" ? (
          <kbd className="nd-kb-search-key" aria-hidden="true">
            /
          </kbd>
        ) : null}
      </div>
      {variant === "panel" && open ? (
        <button
          type="button"
          className="nd-btn nd-btn-sm nd-kb-search-cancel"
          data-variant="ghost"
          onPointerDown={(e) => e.preventDefault()}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            setQuery("");
            close();
            input.current?.blur();
          }}
        >
          {t("common.cancel")}
        </button>
      ) : null}
      {panel}
    </div>
  );
}
