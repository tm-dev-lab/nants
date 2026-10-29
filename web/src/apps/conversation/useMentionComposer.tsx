/*
 * Menções com `@` na caixa da Conversa (spec 136 T06, P3, C-11).
 *
 * O campo segue um `textarea` (o teclado do celular, o corretor, colar imagem, o Enter e os testes
 * continuam os mesmos). As etiquetas são desenhadas por uma camada atrás do campo, com a mesma
 * fonte, quebra e rolagem; com alguma menção, o texto do campo fica transparente (o cursor e a
 * seleção continuam dele) e a camada mostra o texto com as etiquetas. O `@` abre o seletor da base
 * (`KbPicker`): ancorado no `@` no desktop, numa faixa logo acima do campo (e do teclado) no
 * celular. O texto enviado leva `kb:<id>` ou `kb:<id>#<âncora>`.
 */
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode, type RefObject } from "react";
import type { KbDoc } from "../../api/kb";
import { Icon } from "../../desktop/Icon";
import { useMobileMode } from "../../desktop/viewport";
import { useT } from "../../i18n/react";
import { KbPicker, kbPickerKey, kbPickerOptionId, useKbPicker, type KbPickerChoice } from "../knowledge/KbPicker";
import { useKb } from "../knowledge/kbStore";
import { loadKbOutline } from "../knowledge/kbCiteStore";
import { splitCitations } from "../knowledge/knowledgeModel";
import { decodeMentions, encodeMentions, findTrigger, insertMention, reconcileMentions, removeMentionAt, snapCaret, type DraftMention } from "./mentions";

const POPOVER_WIDTH = 420;

interface Options {
  projectId: string;
  draft: string;
  setDraft: (text: string) => void;
  areaRef: RefObject<HTMLTextAreaElement | null>;
  disabled: boolean;
  /** Guarda o rascunho (o texto codificado, com `kb:`). */
  persist: (encoded: string) => void;
}

/** Teto da consulta da seção (a parte depois de `#`). */
const MAX_SECTION_QUERY = 80;

/**
 * Passo da seção, guardado como estado (não relido do texto): o `@` em `start`, o documento e a
 * consulta dos documentos de antes (para voltar a ela). O título pode ser longo ou ter `@`.
 */
interface SectionPick {
  start: number;
  doc: KbDoc;
  prevQuery: string;
}

function sectionPrefix(doc: KbDoc): string {
  return `@${doc.title.replace(/\s+/g, " ")} #`;
}

export function useMentionComposer({ projectId, draft, setDraft, areaRef, disabled, persist }: Options) {
  const { t, tp } = useT();
  const mobile = useMobileMode();
  const listId = useId();
  // A lista da base só é lida quando a menção pode ser usada: o campo focado, um `@` ou um `kb:`.
  const [wantKb, setWantKb] = useState(false);
  const kb = useKb(wantKb || draft.includes("@") || draft.includes("kb:") ? projectId : null);
  const docs = useMemo(() => (kb?.status === "ok" ? kb.list.docs : []), [kb]);
  const ready = docs.some((doc) => doc.status === "ready");
  const [mentions, setMentions] = useState<DraftMention[]>([]);
  const [caret, setCaret] = useState<number | null>(null);
  const [dismissed, setDismissed] = useState<number | null>(null);
  const [activeState, setActiveState] = useState<{ key: string; index: number } | null>(null);
  const [sectionPick, setSectionPick] = useState<SectionPick | null>(null);
  const pendingCaret = useRef<number | null>(null);
  /** O último texto que o próprio hook escreveu: outro texto (o lançador, o envio) zera as menções. */
  const ownText = useRef(draft);
  const backdropRef = useRef<HTMLDivElement | null>(null);
  const markerRef = useRef<HTMLSpanElement | null>(null);
  const [anchor, setAnchor] = useState<{ left: number; bottom: number } | null>(null);

  // O rascunho guardado leva `kb:`; com a lista da base (e os sumários das seções citadas), as
  // citações voltam como etiquetas. Documento ou seção que saiu da base fica como texto comum.
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const restored = useRef(false);
  useEffect(() => {
    // Com a lista falhando, tenta de novo quando ela for relida (o `kb_updated`).
    if (restored.current || kb === null || kb.status !== "ok") return undefined;
    restored.current = true;
    const original = draftRef.current;
    const cites = splitCitations(original).flatMap((part) => (part.kind === "cite" ? [part.cite] : []));
    if (cites.length === 0) return undefined;
    const ready = new Map(kb.list.docs.filter((d) => d.status === "ready").map((d) => [d.id, d]));
    const withSection = [...new Set(cites.filter((c) => c.section !== null && ready.has(c.doc)).map((c) => c.doc))];
    let alive = true;
    void Promise.all(withSection.map(async (doc) => [doc, await loadKbOutline(projectId, doc)] as const)).then((loaded) => {
      if (!alive || draftRef.current !== original) return;
      const outlines = new Map(loaded);
      const decoded = decodeMentions(original, (id, anchor) => {
        const doc = ready.get(id);
        if (doc === undefined) return null;
        if (anchor === null) return { title: doc.title, section: null };
        const outline = outlines.get(id);
        const section = outline?.status === "ok" ? outline.sections.find((s) => s.anchor === anchor) : undefined;
        return section === undefined ? null : { title: doc.title, section: section.title };
      });
      if (decoded.mentions.length === 0) return;
      pendingCaret.current = decoded.text.length;
      ownText.current = decoded.text;
      setMentions(decoded.mentions);
      setDraft(decoded.text);
    });
    return () => {
      alive = false;
    };
  }, [kb, projectId, setDraft]);

  const encoded = encodeMentions(draft, mentions);
  useEffect(() => {
    persist(encoded);
  }, [encoded, persist]);

  // O passo da seção vale enquanto o texto ainda tem `@<título> #` no lugar e o cursor depois dele.
  const sectionQuery = ((): string | null => {
    if (disabled || !ready || caret === null || sectionPick === null) return null;
    const prefix = sectionPrefix(sectionPick.doc);
    const from = sectionPick.start + prefix.length;
    if (!draft.startsWith(prefix, sectionPick.start) || caret < from) return null;
    if (mentions.some((m) => sectionPick.start >= m.start && sectionPick.start < m.end)) return null;
    const query = draft.slice(from, caret);
    return query.length > MAX_SECTION_QUERY || query.includes("\n") ? null : query;
  })();
  const section = sectionPick !== null && sectionQuery !== null ? { doc: sectionPick.doc, query: sectionQuery } : null;
  const trigger =
    sectionPick !== null && section !== null
      ? { start: sectionPick.start, query: draft.slice(sectionPick.start + 1, caret ?? sectionPick.start + 1) }
      : !disabled && ready && caret !== null
        ? findTrigger(draft, caret, mentions)
        : null;
  const picker = useKbPicker(projectId, docs, section === null ? (trigger?.query ?? "") : "", section);
  const open = trigger !== null && dismissed !== trigger.start && (section !== null || picker.items.length > 0);
  const key = trigger === null ? "" : `${trigger.start}\n${section === null ? "d" : "s"}\n${trigger.query}`;
  // Enter só é capturado com uma opção destacada (C-11): no `@` sozinho, nada destacado.
  const fallback = trigger !== null && (trigger.query !== "" || section !== null) ? 0 : -1;
  const active = activeState !== null && activeState.key === key ? Math.min(activeState.index, picker.items.length - 1) : Math.min(fallback, picker.items.length - 1);

  useLayoutEffect(() => {
    const area = areaRef.current;
    if (pendingCaret.current !== null && area !== null) {
      area.setSelectionRange(pendingCaret.current, pendingCaret.current);
      setCaret(pendingCaret.current);
      pendingCaret.current = null;
    }
  }, [draft, areaRef]);

  // O rascunho trocado por fora (pedido do lançador, envio): as menções e o passo da seção saem.
  useLayoutEffect(() => {
    if (draft === ownText.current) return;
    ownText.current = draft;
    setMentions((list) => (list.length === 0 ? list : []));
    setSectionPick(null);
  }, [draft]);

  // Posição do seletor no desktop: ancorado no `@`, acima da linha.
  useLayoutEffect(() => {
    if (!open || mobile) return;
    const marker = markerRef.current;
    const backdrop = backdropRef.current;
    if (marker === null || backdrop === null) return;
    const field = backdrop.parentElement;
    const width = field?.clientWidth ?? POPOVER_WIDTH;
    // Acima da caixa, na altura do `@`: a lista não cobre o texto que está sendo escrito.
    const left = Math.max(-40, Math.min(marker.offsetLeft - 12, width - POPOVER_WIDTH + 40));
    const bottom = (field?.clientHeight ?? backdrop.clientHeight) + 14;
    setAnchor((prev) => (prev !== null && prev.left === left && prev.bottom === bottom ? prev : { left, bottom }));
  }, [open, mobile, key, draft]);

  const apply = useCallback(
    (text: string, nextCaret: number, list: DraftMention[]) => {
      pendingCaret.current = nextCaret;
      ownText.current = text;
      setMentions(list);
      setDraft(text);
    },
    [setDraft],
  );

  const choose = (choice: KbPickerChoice): void => {
    if (trigger === null || caret === null) return;
    const edit = insertMention(draft, trigger.start, caret, mentions, {
      doc: choice.doc.id,
      anchor: choice.section?.anchor ?? null,
      title: choice.doc.title,
      section: choice.section?.title ?? null,
    });
    setActiveState(null);
    setSectionPick(null);
    apply(edit.text, edit.caret, edit.mentions);
  };

  const replaceQuery = (text: string): void => {
    if (trigger === null || caret === null) return;
    const next = `${draft.slice(0, trigger.start)}${text}${draft.slice(caret)}`;
    apply(next, trigger.start + text.length, reconcileMentions(draft, next, mentions));
  };
  const enterSections = (doc: KbDoc): void => {
    if (trigger === null) return;
    setSectionPick({ start: trigger.start, doc, prevQuery: trigger.query });
    replaceQuery(sectionPrefix(doc));
  };
  const back = (): void => {
    if (sectionPick === null) return;
    const prev = sectionPick.prevQuery;
    setSectionPick(null);
    replaceQuery(`@${prev}`);
  };

  /** Teclas do campo antes do envio; `true` quando a menção tratou a tecla. */
  const onKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>): boolean => {
    if (event.nativeEvent.isComposing) return false;
    if (open) {
      const action = kbPickerKey(event, picker, active);
      if (action !== null) {
        event.preventDefault();
        if (action.kind === "move") setActiveState({ key, index: action.index });
        else if (action.kind === "choose") choose(action.item.kind === "section" ? { doc: action.item.doc, section: action.item.section } : { doc: action.item.doc, section: null });
        else if (action.kind === "sections") enterSections(action.doc);
        else if (action.kind === "back") back();
        else if (trigger !== null) setDismissed(trigger.start);
        return true;
      }
    }
    if (event.key === "Backspace" || event.key === "Delete") {
      const area = event.currentTarget;
      const edit = removeMentionAt(draft, area.selectionStart, area.selectionEnd, event.key, mentions);
      if (edit !== null) {
        event.preventDefault();
        apply(edit.text, edit.caret, edit.mentions);
        return true;
      }
    }
    return false;
  };

  const onChange = (next: string, nextCaret: number): void => {
    ownText.current = next;
    setMentions(reconcileMentions(draft, next, mentions));
    setCaret(nextCaret);
    if (dismissed !== null && (findTrigger(next, nextCaret, [])?.start ?? null) !== dismissed) setDismissed(null);
    setDraft(next);
  };

  const onSelect = (area: HTMLTextAreaElement): void => {
    const { selectionStart, selectionEnd } = area;
    if (selectionStart === selectionEnd) {
      const snapped = snapCaret(selectionStart, mentions);
      if (snapped !== selectionStart) area.setSelectionRange(snapped, snapped);
      setCaret(snapped);
    } else setCaret(null);
  };

  const onScroll = (area: HTMLTextAreaElement): void => {
    if (backdropRef.current !== null) backdropRef.current.scrollTop = area.scrollTop;
  };

  /** Camada atrás do campo: o texto com as etiquetas e a marca do `@` (para ancorar o seletor). */
  const backdrop: ReactNode = (
    <div ref={backdropRef} className="nd-composer-input nd-mention-backdrop" data-visible={mentions.length > 0 ? "true" : undefined} aria-hidden="true">
      {renderBackdrop(draft, mentions, trigger?.start ?? null, markerRef)}
      {"​"}
    </div>
  );

  const pickerNode: ReactNode = open ? (
    <KbPicker
      state={picker}
      listId={listId}
      active={active}
      onActive={(index) => setActiveState({ key, index })}
      onChoose={choose}
      onSections={enterSections}
      onBack={back}
      variant={mobile ? "panel" : "popover"}
      {...(!mobile && anchor !== null ? { style: { left: anchor.left, bottom: anchor.bottom, width: POPOVER_WIDTH } } : {})}
    />
  ) : null;

  return {
    mentions,
    hasMentions: mentions.length > 0,
    mobile,
    open,
    backdrop,
    picker: pickerNode,
    status: open ? (section === null ? tp("kb.picker.results", picker.items.length) : t("kb.picker.sectionsLabel", { doc: section.doc.title })) : "",
    hint: ready ? t("conversation.mentionHint") : "",
    areaProps: {
      "aria-autocomplete": ready ? ("list" as const) : undefined,
      "aria-controls": open ? listId : undefined,
      "aria-activedescendant": open && active >= 0 ? kbPickerOptionId(listId, active) : undefined,
      "data-mentions": mentions.length > 0 ? "true" : undefined,
    },
    onKeyDown,
    onChange,
    onSelect,
    onScroll,
    onFocus: (area: HTMLTextAreaElement) => {
      setWantKb(true);
      setCaret(area.selectionStart === area.selectionEnd ? area.selectionStart : null);
    },
    onBlur: () => setCaret(null),
    /** Texto para enviar, com as citações. */
    encode: (): string => encodeMentions(draft, mentions),
    clear: (): void => setMentions([]),
  };
}

function renderBackdrop(text: string, mentions: readonly DraftMention[], markAt: number | null, markerRef: RefObject<HTMLSpanElement | null>): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  const plain = (from: number, to: number): void => {
    if (markAt !== null && markAt >= from && markAt < to) {
      if (markAt > from) out.push(text.slice(from, markAt));
      out.push(
        <span key={`m${markAt}`} ref={markerRef} className="nd-mention-marker">
          {text.charAt(markAt)}
        </span>,
      );
      if (markAt + 1 < to) out.push(text.slice(markAt + 1, to));
    } else if (to > from) out.push(text.slice(from, to));
  };
  for (const m of [...mentions].sort((a, b) => a.start - b.start)) {
    plain(last, m.start);
    const label = text.slice(m.start, m.end);
    const cut = m.section === null ? -1 : label.lastIndexOf(" #");
    out.push(
      <span key={`k${m.start}`} className="nd-mention" data-section={m.section !== null ? "true" : undefined}>
        <span className="nd-mention-at">
          @
          <Icon name="knowledge" size={14} />
        </span>
        {cut < 0 ? label.slice(1) : label.slice(1, cut)}
        {cut < 0 ? null : <span className="nd-mention-section">{label.slice(cut)}</span>}
      </span>,
    );
    last = m.end;
  }
  plain(last, text.length);
  return out;
}
