/*
 * Tabela única de atalhos do desktop (spec 093, decisão 4). Alimenta `aria-keyshortcuts`, a
 * folha de atalhos e o lançador; `matchShortcut` decide qual atalho uma tecla aciona.
 * Sem Alt+Espaço nem Ctrl+` (não funcionam no Windows nem no ABNT2).
 */
import { t, type MessageKey } from "../i18n";
import { isTypingTarget } from "../typing";

export type ShortcutId =
  | "launcher"
  | "nextWindow"
  | "previousWindow"
  | "dockSlot"
  | "shortcutSheet"
  | "undoLayout"
  | "windowMenu";

interface Shortcut {
  id: ShortcutId;
  /** Valor de `aria-keyshortcuts` (sintaxe ARIA; vários separados por espaço). */
  aria: string;
  /** Rótulo exibido na folha de atalhos e no lançador. */
  keys: string;
  /** No idioma ativo (spec 101). */
  readonly description: string;
  /** Dispara mesmo com o foco num campo de texto (só o lançador). */
  allowWhileTyping: boolean;
}

/** Atalho com a descrição lida no idioma ativo. */
function shortcut(id: ShortcutId, aria: string, keys: string, description: MessageKey, allowWhileTyping: boolean): Shortcut {
  return {
    id,
    aria,
    keys,
    get description() {
      return t(description);
    },
    allowWhileTyping,
  };
}

export const SHORTCUTS: readonly Shortcut[] = [
  shortcut("launcher", "Control+K", "Ctrl+K", "shortcut.launcher", true),
  shortcut("nextWindow", "F6", "F6", "shortcut.nextWindow", false),
  shortcut("previousWindow", "Shift+F6", "Shift+F6", "shortcut.previousWindow", false),
  shortcut("dockSlot", "Alt+1", "Alt+1…9", "shortcut.dockSlot", false),
  shortcut("shortcutSheet", "Shift+/", "Shift+/", "shortcut.shortcutSheet", false),
  shortcut("undoLayout", "Control+Z", "Ctrl+Z", "shortcut.undoLayout", false),
  shortcut("windowMenu", "Shift+F10 ContextMenu", "Shift+F10", "shortcut.windowMenu", false),
];

const SHORTCUT_BY_ID: ReadonlyMap<ShortcutId, Shortcut> = new Map(SHORTCUTS.map((s) => [s.id, s]));

/** Entrada da tabela; `undefined` só se a tabela perder um id (coberto por teste). */
export function shortcutById(id: ShortcutId): Shortcut | undefined {
  return SHORTCUT_BY_ID.get(id);
}

/** `aria-keyshortcuts` do ícone do dock na posição `slot` (1..9); fora disso, `undefined`. */
export function dockSlotAria(slot: number): string | undefined {
  return Number.isInteger(slot) && slot >= 1 && slot <= 9 ? `Alt+${slot}` : undefined;
}

/** Subconjunto de `KeyboardEvent` usado pelo casamento (fácil de montar em teste). */
export interface KeyLike {
  key: string;
  code?: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

export interface ShortcutContext {
  /** Foco num campo de texto (`isTypingTarget`). */
  typing: boolean;
  /** Foco no desktop (fora de qualquer conteúdo de janela que tenha o próprio desfazer). */
  onDesktop: boolean;
  /** Foco numa barra de título (Shift+F10/Menu abre o menu dela). */
  onTitleBar: boolean;
}

interface ShortcutMatch {
  id: ShortcutId;
  /** Posição do dock (1..9) em `dockSlot`. */
  slot?: number;
}

/** Contexto a partir do elemento em foco. */
export function shortcutContext(active: Element | null): ShortcutContext {
  // `isContentEditable` não existe no jsdom: o atributo também conta.
  const editable = active !== null && active.closest('[contenteditable=""], [contenteditable="true"]') !== null;
  const typing = Boolean(isTypingTarget(active)) || editable;
  // O slot de ferramentas fica dentro da barra, mas não é a barra (C-07): Shift+F10 lá é do controle.
  const onTitleBar = active !== null && active.closest("[data-window-titlebar]") !== null && active.closest("[data-window-tools]") === null;
  const inContent = active !== null && active.closest("[data-window-content]") !== null;
  return { typing, onTitleBar, onDesktop: !typing && !inContent };
}

function digitOf(event: KeyLike): number | null {
  // Com Alt, alguns layouts trocam o `key`; o `code` físico é estável.
  const fromCode = event.code !== undefined ? /^(?:Digit|Numpad)([1-9])$/.exec(event.code) : null;
  if (fromCode?.[1] !== undefined) return Number(fromCode[1]);
  return /^[1-9]$/.test(event.key) ? Number(event.key) : null;
}

/** Qual atalho a tecla aciona no contexto, ou `null`. */
export function matchShortcut(event: KeyLike, ctx: ShortcutContext): ShortcutMatch | null {
  const ctrl = event.ctrlKey || event.metaKey;
  const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;

  if (ctrl && !event.altKey && !event.shiftKey && key === "k") return { id: "launcher" };
  if (ctx.typing) return null;

  if (key === "F6" && !ctrl && !event.altKey) {
    return { id: event.shiftKey ? "previousWindow" : "nextWindow" };
  }
  if (event.altKey && !ctrl && !event.shiftKey) {
    const slot = digitOf(event);
    if (slot !== null) return { id: "dockSlot", slot };
  }
  if (event.shiftKey && !ctrl && !event.altKey && (key === "?" || key === "/" || event.code === "Slash")) {
    return { id: "shortcutSheet" };
  }
  if (ctrl && !event.altKey && !event.shiftKey && key === "z" && ctx.onDesktop) return { id: "undoLayout" };
  if (ctx.onTitleBar && ((event.shiftKey && key === "F10" && !ctrl && !event.altKey) || key === "ContextMenu")) {
    return { id: "windowMenu" };
  }
  return null;
}
