/*
 * Editor de documentos da base (spec 136 T11, decisão 8): escrever e editar markdown no Nants.
 *
 * - **Campo:** um `textarea` (o teclado do celular, o corretor, o ditado, a seleção nativa e o
 *   leitor de tela continuam os do sistema), com uma camada de realce atrás (`markdownHighlight`):
 *   os marcadores apagados e títulos, negrito, links e código na cor do papel. A camada dá a altura;
 *   o campo, transparente, fica por cima com o cursor e a seleção.
 * - **Disposição:** no desktop, "Escrever", "Lado a lado" (padrão) e "Ver"; no estreito, as abas
 *   "Escrever" e "Ver". A prévia usa o mesmo renderizador da janela (`MarkdownText`).
 * - **Barra e atalhos:** título, negrito, itálico, lista, lista numerada, citação, código, link e
 *   link para documento da base; Ctrl+B, Ctrl+I, Ctrl+E, Ctrl+K, Ctrl+1..3, Ctrl+Shift+7/8/9;
 *   Enter continua a lista, Tab e Shift+Tab recuam o item.
 * - **Links:** `[[` abre o seletor da base (`KbPicker`); a escolha vira `[título](arquivo.md#seção)`.
 * - **Salvamento automático:** com espera; o estado "Salvo", "Salvando", "Não salvo" ou "Só neste
 *   aparelho"; o texto vai arrumado (`tidyMarkdown`); o que se digita durante uma gravação gera a
 *   seguinte. A falha de rede e a do servidor tentam de novo; a de validação para, com o motivo numa
 *   linha à vista. O 409 mostra "Recarregar" e "Manter o meu". O "Concluir" só fecha com o texto
 *   gravado.
 * - **Rascunho local:** o texto não salvo fica no navegador (`kbDraft`).
 */
import "./editor.css";
import { useCallback, useDeferredValue, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode, type RefObject } from "react";
import type { ApiFailure } from "../../../api/client";
import { getKbContent, postKbWritten, putKbContent, type KbDoc } from "../../../api/kb";
import { Icon, type IconName } from "../../../desktop/Icon";
import { KEYBOARD_AREA_ATTR } from "../../../desktop/viewport";
import type { MessageKey } from "../../../i18n/ptBR";
import { useT } from "../../../i18n/react";
import { apiFailureText } from "../../../i18n/serverCodes";
import { MarkdownText } from "../../shared/ReportMarkdown";
import { KbPicker, kbPickerKey, kbPickerOptionId, useKbPicker, type KbPickerChoice } from "../KbPicker";
import { upsertKbDoc } from "../kbStore";
import { kindIcon } from "../knowledgeModel";
import { clearDraft, readDraft, writeDraft } from "./kbDraft";
import { highlightMarkdown, MAX_HIGHLIGHT_CHARS, type HlSpan } from "./markdownHighlight";
import {
  continueList,
  cycleHeading,
  findLinkTrigger,
  indentList,
  insertDocLink,
  insertLink,
  linkTarget,
  tidyMarkdown,
  toggleBlock,
  toggleCode,
  toggleInline,
  type Edit,
  type Sel,
} from "./markdownEdit";

/** Espera depois da última tecla antes de salvar. */
const SAVE_DELAY_MS = 1200;
/** Nova tentativa depois de uma falha de gravação. */
const RETRY_DELAY_MS = 10_000;
const POPOVER_WIDTH = 380;

type Mode = "write" | "split" | "view";
type SaveState = "saved" | "pending" | "saving" | "needsTitle" | "local" | "failed" | "conflict";

/** Resultado de uma gravação, com o id do documento (o novo ganha id na primeira). */
interface SaveResult {
  status: "saved" | "unchanged" | "pending" | "busy" | "needsTitle" | "local" | "failed" | "conflict";
  id: string | null;
}

interface Conflict {
  content: string;
  sha: string;
}

interface KbEditorProps {
  projectId: string;
  docs: readonly KbDoc[];
  /** O documento aberto; `null` cria um novo. */
  doc: KbDoc | null;
  /** Janela estreita (celular): abas "Escrever" e "Ver". */
  stacked: boolean;
  onDone: (docId: string | null) => void;
  /** Só para o mockup e os testes: estado inicial da exibição. */
  initialMode?: Mode;
}

interface FormatAction {
  id: string;
  icon: IconName;
  label: MessageKey;
  keys?: string;
  run: (sel: Sel) => Edit;
}

const FORMAT_GROUPS: readonly (readonly FormatAction[])[] = [
  [
    { id: "heading", icon: "heading", label: "kb.editor.fmt.heading", keys: "Ctrl+1..3", run: cycleHeading },
    { id: "bold", icon: "bold", label: "kb.editor.fmt.bold", keys: "Ctrl+B", run: (s) => toggleInline(s, "bold") },
    { id: "italic", icon: "italic", label: "kb.editor.fmt.italic", keys: "Ctrl+I", run: (s) => toggleInline(s, "italic") },
  ],
  [
    { id: "bullet", icon: "list", label: "kb.editor.fmt.bullet", keys: "Ctrl+Shift+8", run: (s) => toggleBlock(s, "bullet") },
    { id: "numbered", icon: "list-ordered", label: "kb.editor.fmt.numbered", keys: "Ctrl+Shift+7", run: (s) => toggleBlock(s, "numbered") },
    { id: "quote", icon: "quote", label: "kb.editor.fmt.quote", keys: "Ctrl+Shift+9", run: (s) => toggleBlock(s, "quote") },
  ],
  [
    { id: "code", icon: "code", label: "kb.editor.fmt.code", keys: "Ctrl+E", run: toggleCode },
    { id: "link", icon: "link", label: "kb.editor.fmt.link", keys: "Ctrl+K", run: insertLink },
  ],
];

/** Atalho de teclado da barra, ou `null`. */
function shortcutEdit(event: ReactKeyboardEvent, sel: Sel): Edit | null {
  const mod = event.ctrlKey || event.metaKey;
  if (!mod || event.altKey) return null;
  const key = event.key.toLowerCase();
  if (!event.shiftKey) {
    if (key === "b") return toggleInline(sel, "bold");
    if (key === "i") return toggleInline(sel, "italic");
    if (key === "e") return toggleCode(sel);
    if (key === "k") return insertLink(sel);
    if (key === "1") return toggleBlock(sel, "h1");
    if (key === "2") return toggleBlock(sel, "h2");
    if (key === "3") return toggleBlock(sel, "h3");
    return null;
  }
  if (event.code === "Digit8") return toggleBlock(sel, "bullet");
  if (event.code === "Digit7") return toggleBlock(sel, "numbered");
  if (event.code === "Digit9") return toggleBlock(sel, "quote");
  return null;
}

/**
 * Aplica a troca no campo pelo próprio navegador (`insertText`), o que mantém o desfazer; sem ele
 * (jsdom), `setRangeText`. Devolve o texto novo.
 */
function applyEdit(area: HTMLTextAreaElement, edit: Edit): string {
  area.focus();
  // Troca vazia (nada a inserir nem a apagar): o `delete` com a seleção recolhida apagaria o
  // caractere anterior.
  if (edit.from === edit.to && edit.insert === "") {
    area.setSelectionRange(edit.selStart, edit.selEnd);
    return area.value;
  }
  area.setSelectionRange(edit.from, edit.to);
  let done = false;
  try {
    done = typeof document.execCommand === "function" && document.execCommand(edit.insert === "" ? "delete" : "insertText", false, edit.insert);
  } catch {
    done = false;
  }
  if (!done || area.value.slice(edit.from, edit.from + edit.insert.length) !== edit.insert) {
    area.setRangeText(edit.insert, edit.from, edit.to, "end");
  }
  area.setSelectionRange(edit.selStart, edit.selEnd);
  return area.value;
}

function wordCount(text: string): number {
  const words = text.replace(/[#>*_`[\]()-]/g, " ").match(/[\p{L}\p{N}]+/gu);
  return words === null ? 0 : words.length;
}

const ROLE_CLASS: Record<NonNullable<HlSpan["role"]>, string> = {
  mark: "nd-kbe-hl-mark",
  heading: "nd-kbe-hl-heading",
  strong: "nd-kbe-hl-strong",
  emph: "nd-kbe-hl-emph",
  code: "nd-kbe-hl-code",
  link: "nd-kbe-hl-link",
  url: "nd-kbe-hl-url",
  quote: "nd-kbe-hl-quote",
  fence: "nd-kbe-hl-code",
};

/** A camada de realce, com o marcador (posição do `[[`) para ancorar o seletor. */
function Highlight({ text, marker, markerRef }: { text: string; marker: number | null; markerRef: RefObject<HTMLSpanElement | null> }) {
  const lines = useMemo(() => highlightMarkdown(text), [text]);
  let offset = 0;
  const out: ReactNode[] = [];
  lines.forEach((spans, li) => {
    const parts: ReactNode[] = [];
    spans.forEach((span, si) => {
      const cls = span.role === null ? undefined : ROLE_CLASS[span.role];
      const end = offset + span.text.length;
      if (marker !== null && marker >= offset && marker < end) {
        const cut = marker - offset;
        if (cut > 0) parts.push(<span key={`${si}a`} className={cls}>{span.text.slice(0, cut)}</span>);
        parts.push(<span key="m" ref={markerRef} className="nd-kbe-marker" />);
        parts.push(<span key={`${si}b`} className={cls}>{span.text.slice(cut)}</span>);
      } else {
        parts.push(
          <span key={si} className={cls}>
            {span.text}
          </span>,
        );
      }
      offset = end;
    });
    if (marker !== null && marker === offset && !parts.some((p) => (p as { key?: string } | null)?.key === "m")) {
      parts.push(<span key="m" ref={markerRef} className="nd-kbe-marker" />);
    }
    out.push(
      <div key={li} className="nd-kbe-line" data-heading={spans.some((s) => s.role === "heading") ? "true" : undefined}>
        {parts.length === 0 ? "​" : parts}
      </div>,
    );
    offset += 1; // a quebra de linha
  });
  return <>{out}</>;
}

function SaveIndicator({ state }: { state: SaveState }) {
  const { t } = useT();
  const text: Record<SaveState, string> = {
    saved: t("kb.editor.state.saved"),
    pending: t("kb.editor.state.pending"),
    saving: t("kb.editor.state.saving"),
    needsTitle: t("kb.editor.state.needsTitle"),
    local: t("kb.editor.state.local"),
    failed: t("kb.editor.state.pending"),
    conflict: t("kb.editor.state.pending"),
  };
  return (
    <span className="nd-kbe-state" data-state={state} role="status">
      {state === "saving" ? <span className="nd-kb-spin" aria-hidden="true" /> : state === "saved" ? <Icon name="check" size={14} /> : <span className="nd-kbe-dot" aria-hidden="true" />}
      <span>{text[state]}</span>
    </span>
  );
}

export function KbEditor({ projectId, docs, doc, stacked, onDone, initialMode }: KbEditorProps) {
  const { t, tp } = useT();
  const fieldId = useId();
  const titleId = useId();
  const listId = useId();
  const [docId, setDocId] = useState<string | null>(doc?.id ?? null);
  const [title, setTitle] = useState(doc?.title ?? "");
  /**
   * O documento novo só é criado com o título confirmado (o foco saiu do campo, ou Enter): sem
   * isso, a espera do salvamento criaria "Check" no meio de "Checklist".
   */
  const [titleDone, setTitleDone] = useState(doc !== null);
  const [content, setContent] = useState("");
  const [loaded, setLoaded] = useState(doc === null);
  const [loadFailure, setLoadFailure] = useState<ApiFailure | null>(null);
  const [mode, setMode] = useState<Mode>(initialMode ?? (stacked ? "write" : "split"));
  const [saveState, setSaveState] = useState<SaveState>(doc === null ? "needsTitle" : "saved");
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [restored, setRestored] = useState(false);
  const base = useRef(doc?.sha256 ?? "");
  const saved = useRef<string | null>(doc === null ? null : "");
  const area = useRef<HTMLTextAreaElement | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  const markerRef = useRef<HTMLSpanElement | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const busy = useRef(false);
  /** A pessoa mudou algo (o "Concluir" sem mudança não grava nem arruma o texto). */
  const edited = useRef(false);

  // --- Carga e rascunho ---
  useEffect(() => {
    if (doc === null) {
      const draft = readDraft(projectId, null);
      if (draft !== null && (draft.content !== "" || draft.title !== "")) {
        setTitle(draft.title);
        setTitleDone(draft.title.trim() !== "");
        setContent(draft.content);
        edited.current = true;
        setRestored(true);
      }
      return undefined;
    }
    let live = true;
    // O original (não a nota extraída): é ele que a gravação troca.
    void getKbContent(projectId, doc.id).then((result) => {
      if (!live) return;
      if (!result.ok) {
        setLoadFailure(result.error);
        return;
      }
      const current = result.data.content;
      const sha = result.data.sha256;
      base.current = sha;
      saved.current = current;
      const draft = readDraft(projectId, doc.id);
      if (draft !== null && draft.content !== current) {
        setContent(draft.content);
        if (draft.base === sha) {
          edited.current = true;
          setRestored(true);
          setSaveState("pending");
        } else {
          edited.current = true;
          setConflict({ content: current, sha });
          setSaveState("conflict");
        }
      } else {
        setContent(current);
      }
      setLoaded(true);
    });
    return () => {
      live = false;
    };
  }, [projectId, doc]);

  // --- Salvamento ---
  // A gravação lê o texto, o título e o conflito pelos refs: um `save()` que termina depois de
  // outras teclas vê o estado de agora, não o da hora em que começou.
  const contentRef = useRef(content);
  contentRef.current = content;
  const titleRef = useRef(title);
  titleRef.current = title;
  const titleDoneRef = useRef(titleDone);
  titleDoneRef.current = titleDone;
  const conflictRef = useRef(conflict);
  conflictRef.current = conflict;
  const titleInput = useRef<HTMLInputElement | null>(null);
  /** Um `save()` chegou com outro em andamento: grava de novo quando ele terminar. */
  const pendingSave = useRef(false);

  const schedule = useCallback((ms: number): void => {
    if (timer.current !== null) clearTimeout(timer.current);
    timer.current = setTimeout(() => void saveRef.current(), ms);
  }, []);

  /** Falhou: a de rede e a do servidor (5xx) tentam de novo; a de validação para e pede ação. */
  const fail = useCallback((error: ApiFailure): SaveResult => {
    setFailure(error);
    if (error.status === 0 || error.status >= 500) {
      setSaveState("local");
      return { status: "local", id: null };
    }
    setSaveState("failed");
    if (error.code?.startsWith("kb_title_") === true) {
      setTitleDone(false);
      titleInput.current?.focus();
    }
    return { status: "failed", id: null };
  }, []);

  /** Depois de gravar: se o texto mudou durante a gravação, rascunho com a versão nova e outra gravação. */
  const settled = useCallback(
    (id: string, text: string): SaveResult => {
      setFailure(null);
      const now = contentRef.current;
      if (tidyMarkdown(now) === text && !pendingSave.current) {
        clearDraft(projectId, id);
        setSaveState("saved");
        return { status: "saved", id };
      }
      pendingSave.current = false;
      writeDraft(projectId, id, { title: titleRef.current, content: now, base: base.current, at: Date.now() });
      setSaveState("pending");
      schedule(SAVE_DELAY_MS);
      return { status: "pending", id };
    },
    [projectId, schedule],
  );

  const save = useCallback(async (): Promise<SaveResult> => {
    if (conflictRef.current !== null) return { status: "conflict", id: docId };
    if (busy.current) {
      pendingSave.current = true;
      return { status: "busy", id: docId };
    }
    const text = tidyMarkdown(contentRef.current);
    if (docId === null) {
      const name = titleRef.current.trim();
      if (name === "" || !titleDoneRef.current) {
        setSaveState("needsTitle");
        return { status: "needsTitle", id: null };
      }
      busy.current = true;
      setSaveState("saving");
      const result = await postKbWritten(projectId, name, text);
      busy.current = false;
      if (!result.ok) return fail(result.error);
      const id = result.data.doc.id;
      setDocId(id);
      base.current = result.data.doc.sha256 ?? "";
      saved.current = text;
      upsertKbDoc(projectId, result.data.doc);
      clearDraft(projectId, null);
      return settled(id, text);
    }
    if (text === saved.current) {
      setFailure(null);
      setSaveState("saved");
      clearDraft(projectId, docId);
      return { status: "unchanged", id: docId };
    }
    busy.current = true;
    setSaveState("saving");
    const result = await putKbContent(projectId, docId, text, base.current);
    busy.current = false;
    if (!result.ok) {
      if (result.error.code === "kb_edit_conflict") {
        const current = await getKbContent(projectId, docId);
        if (current.ok) {
          setFailure(null);
          setConflict({ content: current.data.content, sha: current.data.sha256 });
          setSaveState("conflict");
          return { status: "conflict", id: docId };
        }
      }
      const failed = fail(result.error);
      if (failed.status === "local" && pendingSave.current) pendingSave.current = false;
      return { ...failed, id: docId };
    }
    base.current = result.data.doc.sha256 ?? "";
    saved.current = text;
    upsertKbDoc(projectId, result.data.doc);
    return settled(docId, text);
  }, [docId, fail, projectId, settled]);

  const saveRef = useRef(save);
  saveRef.current = save;

  // Cada mudança: rascunho local na hora e gravação com espera.
  const firstRun = useRef(true);
  useEffect(() => {
    if (!loaded) return undefined;
    if (firstRun.current) {
      firstRun.current = false;
      if (!restored) return undefined;
    }
    const dirty = docId === null ? content !== "" || title !== "" : tidyMarkdown(content) !== saved.current;
    if (!dirty) return undefined;
    writeDraft(projectId, docId, { title, content, base: base.current, at: Date.now() });
    if (conflict !== null) return undefined;
    setSaveState(docId === null && (title.trim() === "" || !titleDone) ? "needsTitle" : "pending");
    schedule(SAVE_DELAY_MS);
    return () => {
      if (timer.current !== null) clearTimeout(timer.current);
    };
  }, [content, title, titleDone, loaded, docId, projectId, conflict, restored, schedule]);

  // Falha passageira (rede, servidor): tenta de novo depois de um tempo. A de validação espera a pessoa.
  useEffect(() => {
    if (saveState !== "local") return undefined;
    const retry = setTimeout(() => void saveRef.current(), RETRY_DELAY_MS);
    return () => clearTimeout(retry);
  }, [saveState]);

  function reloadTheirs(): void {
    if (conflict === null) return;
    base.current = conflict.sha;
    saved.current = conflict.content;
    setContent(conflict.content);
    setConflict(null);
    setSaveState("saved");
    clearDraft(projectId, docId);
  }

  function keepMine(): void {
    if (conflict === null) return;
    base.current = conflict.sha;
    saved.current = conflict.content;
    conflictRef.current = null;
    setConflict(null);
    setSaveState("pending");
    void saveRef.current();
  }

  function discardDraft(): void {
    clearDraft(projectId, docId);
    setRestored(false);
    setFailure(null);
    if (docId === null) {
      setTitle("");
      setContent("");
      setSaveState("needsTitle");
      return;
    }
    setContent(saved.current ?? "");
    setSaveState("saved");
  }

  /**
   * "Concluir": fecha só com o texto gravado (ou sem mudança). Com conflito aberto, falha ou
   * título faltando, o editor fica aberto e o motivo continua à vista.
   */
  async function done(): Promise<void> {
    if (conflict !== null) return;
    if (!edited.current) {
      onDone(docId);
      return;
    }
    if (timer.current !== null) clearTimeout(timer.current);
    let result = await save();
    // Uma gravação em andamento: espera ela terminar e grava o que faltar.
    while (result.status === "busy" || result.status === "pending") {
      await new Promise((resolve) => setTimeout(resolve, 50));
      if (!busy.current) {
        if (timer.current !== null) clearTimeout(timer.current);
        pendingSave.current = false;
        result = await saveRef.current();
      }
    }
    if (result.status === "saved" || result.status === "unchanged") onDone(result.id);
  }

  // --- Campo ---
  const [caret, setCaret] = useState<number | null>(null);
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState<number | null>(null);
  const [sectionDoc, setSectionDoc] = useState<KbDoc | null>(null);
  const [active, setActive] = useState(0);
  const trigger = focused && caret !== null ? findLinkTrigger(content, caret) : null;
  const pickerOpen = trigger !== null && trigger.start !== dismissed;
  const linkable = useMemo(() => docs.filter((d) => d.status === "ready" && d.id !== docId && (d.file_name !== null || d.source_url !== null)), [docs, docId]);
  const picker = useKbPicker(projectId, linkable, trigger?.query ?? "", sectionDoc !== null ? { doc: sectionDoc, query: "" } : null);
  const pickerKey = `${trigger?.start ?? -1}:${trigger?.query ?? ""}:${sectionDoc?.id ?? ""}`;
  const lastKey = useRef(pickerKey);
  if (lastKey.current !== pickerKey) {
    lastKey.current = pickerKey;
    if (active !== 0) setActive(0);
  }
  useEffect(() => {
    if (trigger === null && sectionDoc !== null) setSectionDoc(null);
  }, [trigger, sectionDoc]);

  const highlight = content.length <= MAX_HIGHLIGHT_CHARS;
  const [anchor, setAnchor] = useState<{ left: number; top: number } | null>(null);
  useLayoutEffect(() => {
    if (!pickerOpen || stacked) {
      setAnchor(null);
      return;
    }
    const mark = markerRef.current;
    const box = scroller.current;
    if (mark === null || box === null) return;
    const m = mark.getBoundingClientRect();
    const b = box.getBoundingClientRect();
    const left = Math.max(8, Math.min(m.left - b.left, b.width - POPOVER_WIDTH - 8));
    setAnchor({ left, top: m.bottom - b.top + box.scrollTop + 6 });
  }, [pickerOpen, stacked, content, caret]);

  const selection = (): Sel => {
    const el = area.current;
    return { text: el?.value ?? content, start: el?.selectionStart ?? 0, end: el?.selectionEnd ?? 0 };
  };

  function run(edit: Edit | null): boolean {
    const el = area.current;
    if (edit === null || el === null) return false;
    edited.current = true;
    setContent(applyEdit(el, edit));
    setCaret(edit.selEnd);
    return true;
  }

  function choose(choice: KbPickerChoice): void {
    const el = area.current;
    if (trigger === null || el === null) return;
    const target = choice.doc.file_name !== null ? linkTarget(choice.doc.file_name, choice.section?.anchor ?? null) : (choice.doc.source_url ?? "");
    const label = choice.section !== null ? choice.section.title : choice.doc.title;
    run(insertDocLink(el.value, trigger, el.selectionStart, label, target));
    setSectionDoc(null);
  }

  function onKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>): void {
    if (event.nativeEvent.isComposing) return;
    if (pickerOpen) {
      const action = kbPickerKey(event, picker, active);
      if (action !== null) {
        event.preventDefault();
        event.stopPropagation();
        if (action.kind === "move") setActive(action.index);
        else if (action.kind === "close") setDismissed(trigger?.start ?? null);
        else if (action.kind === "sections") setSectionDoc(action.doc);
        else if (action.kind === "back") setSectionDoc(null);
        else choose(action.item.kind === "section" ? { doc: action.item.doc, section: action.item.section } : { doc: action.item.doc, section: null });
        return;
      }
    }
    const shortcut = shortcutEdit(event, selection());
    if (shortcut !== null) {
      event.preventDefault();
      // Ctrl+K no campo é o link (o lançador fica fora do campo).
      event.stopPropagation();
      run(shortcut);
      return;
    }
    if (event.key === "Enter" && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey) {
      if (run(continueList(selection()))) event.preventDefault();
      return;
    }
    if (event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) {
      if (run(indentList(selection(), event.shiftKey))) event.preventDefault();
    }
  }

  function startDocLink(): void {
    const el = area.current;
    if (el === null) return;
    const s = selection();
    run({ from: s.start, to: s.end, insert: "[[", selStart: s.start + 2, selEnd: s.start + 2 });
    setDismissed(null);
  }

  const deferred = useDeferredValue(content);
  const showWrite = mode !== "view";
  const showView = mode !== "write";
  const words = wordCount(content);

  const field = (
    <div ref={scroller} className="nd-kbe-write nd-kb-scroll" data-highlight={highlight ? "true" : "false"}>
      <div className="nd-kbe-page">
        {docId === null ? (
          <input
            ref={titleInput}
            id={titleId}
            className="nd-kbe-title"
            value={title}
            placeholder={t("kb.editor.titlePlaceholder")}
            aria-label={t("kb.editor.titleLabel")}
            autoComplete="off"
            onChange={(e) => {
              edited.current = true;
              setTitle(e.target.value);
            }}
            onBlur={() => setTitleDone(title.trim() !== "")}
            onKeyDown={(e) => {
              if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
              e.preventDefault();
              setTitleDone(title.trim() !== "");
              area.current?.focus();
            }}
          />
        ) : doc === null ? (
          <h1 className="nd-kbe-title" data-fixed="true">
            {title}
          </h1>
        ) : null}
        <div className="nd-kbe-field">
          {highlight ? (
            <div className="nd-kbe-backdrop" aria-hidden="true">
              <Highlight text={content} marker={pickerOpen ? (trigger?.start ?? null) : null} markerRef={markerRef} />
            </div>
          ) : null}
          <textarea
            ref={area}
            id={fieldId}
            className="nd-kbe-input"
            value={content}
            placeholder={t("kb.editor.placeholder")}
            aria-label={t("kb.editor.bodyLabel")}
            aria-controls={pickerOpen ? listId : undefined}
            aria-activedescendant={pickerOpen && picker.items.length > 0 ? kbPickerOptionId(listId, active) : undefined}
            aria-autocomplete="list"
            spellCheck
            autoCapitalize="sentences"
            onChange={(e) => {
              edited.current = true;
              setContent(e.target.value);
              setCaret(e.target.selectionStart);
            }}
            onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
            onFocus={() => setFocused(true)}
            onBlur={() => setFocused(false)}
            onKeyDown={onKeyDown}
          />
        </div>
      </div>
      {pickerOpen && !stacked && anchor !== null ? (
        <KbPicker
          state={picker}
          listId={listId}
          active={active}
          onActive={setActive}
          onChoose={choose}
          onSections={setSectionDoc}
          onBack={() => setSectionDoc(null)}
          variant="popover"
          title={t("kb.editor.linkPicker")}
          chooseHint={t("kb.editor.hintLink")}
          style={{ position: "absolute", left: anchor.left, top: anchor.top, width: POPOVER_WIDTH }}
        />
      ) : null}
    </div>
  );

  const preview = (
    <div className="nd-kbe-view nd-kb-scroll" aria-label={t("kb.editor.preview")} role="region">
      <div className="nd-kbe-page nd-kbe-rendered">
        {title.trim() !== "" && (docId === null || doc === null) ? <h1 className="nd-md-h1 nd-kbe-view-title">{title}</h1> : null}
        {deferred.trim() === "" ? <p className="nd-muted">{t("kb.editor.previewEmpty")}</p> : <MarkdownText text={deferred} />}
      </div>
    </div>
  );

  const bar = (
    <div role="toolbar" aria-label={t("kb.editor.toolbar")} aria-controls={fieldId} className="nd-kbe-bar" data-kbe-bar="">
      {FORMAT_GROUPS.map((group, gi) => (
        <div key={gi} className="nd-kbe-bar-group">
          {group.map((a) => {
            const label = t(a.label);
            const tip = a.keys !== undefined && !stacked ? t("kb.editor.fmt.withKeys", { label, keys: a.keys }) : label;
            return (
              <button
                key={a.id}
                type="button"
                className="nd-btn nd-iconbtn nd-btn-sm"
                data-variant="ghost"
                data-fmt={a.id}
                aria-label={label}
                title={tip}
                disabled={!showWrite}
                onPointerDown={(e) => e.preventDefault()}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => run(a.run(selection()))}
              >
                <Icon name={a.icon} size={16} />
              </button>
            );
          })}
        </div>
      ))}
      <div className="nd-kbe-bar-group">
        <button
          type="button"
          className="nd-btn nd-iconbtn nd-btn-sm"
          data-variant="ghost"
          data-fmt="docLink"
          aria-label={t("kb.editor.fmt.docLink")}
          title={stacked ? t("kb.editor.fmt.docLink") : t("kb.editor.fmt.withKeys", { label: t("kb.editor.fmt.docLink"), keys: "[[" })}
          disabled={!showWrite}
          onPointerDown={(e) => e.preventDefault()}
          onMouseDown={(e) => e.preventDefault()}
          onClick={startDocLink}
        >
          <Icon name="knowledge" size={16} />
        </button>
      </div>
      {!stacked ? <span className="nd-kbe-words">{tp("kb.editor.words", words)}</span> : null}
    </div>
  );

  const modes: Mode[] = stacked ? ["write", "view"] : ["write", "split", "view"];
  const shownDoc = doc ?? docs.find((d) => d.id === docId) ?? null;

  return (
    <div className="nd-kbe" data-kbe="" data-mode={mode} data-stacked={stacked ? "true" : undefined} {...{ [KEYBOARD_AREA_ATTR]: "" }}>
      <header className="nd-kbe-head">
        {stacked ? (
          <button type="button" className="nd-btn nd-btn-sm" onClick={() => void done()}>
            <Icon name="arrow-left" size={14} />
            {t("kb.editor.done")}
          </button>
        ) : null}
        <div className="nd-kbe-name">
          <Icon name={shownDoc !== null ? kindIcon(shownDoc) : "file-plus"} size={16} className="nd-kb-kind" />
          <span className="nd-kbe-name-text">{title.trim() !== "" ? title : t("kb.editor.new")}</span>
          {shownDoc?.file_name ? <span className="nd-kbe-file">{shownDoc.file_name}</span> : null}
        </div>
        <SaveIndicator state={saveState} />
        <span className="nd-kb-spacer" />
        <div role="group" aria-label={t("kb.editor.mode")} className="nd-segment nd-chrome nd-kbe-modes">
          {modes.map((m) => (
            <button key={m} type="button" className="nd-segment-item" aria-pressed={mode === m} onClick={() => setMode(m)}>
              {m === "split" ? <Icon name="columns" size={14} /> : null}
              <span>{t(`kb.editor.mode.${m}`)}</span>
            </button>
          ))}
        </div>
        {!stacked ? (
          <button type="button" className="nd-btn nd-btn-sm" data-variant="primary" onClick={() => void done()}>
            <Icon name="check" size={14} />
            {t("kb.editor.done")}
          </button>
        ) : null}
      </header>
      {failure !== null && saveState !== "saving" ? (
        <p className="nd-kbe-failure" role="alert" data-kbe-failure={saveState}>
          <Icon name="alert" size={14} />
          <span>
            {saveState === "local"
              ? t("kb.editor.state.localHint", { message: apiFailureText(failure) })
              : t("kb.editor.failed", { message: apiFailureText(failure) })}
          </span>
        </p>
      ) : null}
      {conflict !== null ? (
        <div className="nd-kbe-conflict" role="alert" data-kbe-conflict="">
          <Icon name="alert" size={16} />
          <div className="nd-kbe-conflict-text">
            <strong>{t("kb.editor.conflict.title")}</strong>
            <span>{t("kb.editor.conflict.text")}</span>
          </div>
          <div className="nd-row nd-wrap">
            <button type="button" className="nd-btn nd-btn-sm" onClick={reloadTheirs}>
              <Icon name="refresh" size={14} />
              {t("kb.editor.conflict.reload")}
            </button>
            <button type="button" className="nd-btn nd-btn-sm" onClick={keepMine}>
              <Icon name="save" size={14} />
              {t("kb.editor.conflict.keep")}
            </button>
          </div>
        </div>
      ) : null}
      {restored && conflict === null ? (
        <p className="nd-kbe-notice" role="status">
          <span>{t("kb.editor.draftRestored")}</span>
          <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" onClick={discardDraft}>
            <Icon name="undo" size={14} />
            {t("kb.editor.draftDiscard")}
          </button>
        </p>
      ) : null}
      {!stacked && showWrite ? bar : null}
      {loadFailure !== null ? (
        <p role="alert" className="nd-field-error nd-kb-note">
          {t("kb.editor.loadError", { message: apiFailureText(loadFailure) })}
        </p>
      ) : !loaded ? (
        <p role="status" className="nd-muted nd-kb-note">
          {t("kb.editor.loading")}
        </p>
      ) : (
        <div className="nd-kbe-panes">
          {showWrite ? field : null}
          {showView ? preview : null}
        </div>
      )}
      {stacked && showWrite && pickerOpen ? (
        <KbPicker
          state={picker}
          listId={listId}
          active={active}
          onActive={setActive}
          onChoose={choose}
          onSections={setSectionDoc}
          onBack={() => setSectionDoc(null)}
          variant="panel"
          title={t("kb.editor.linkPicker")}
        />
      ) : null}
      {stacked && showWrite ? bar : null}
    </div>
  );
}
