/*
 * Tema do CodeMirror, claro e escuro, a partir de `CODE_THEMES` (spec 093, decisão 13).
 * Usado pela janela Arquivos. Como o editor vive num shadow root
 * (CSP), o tema usa só `EditorView.theme` e variáveis CSS herdadas, nunca classes Tailwind.
 */
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { CODE_THEMES, type ThemeName } from "./tokens";

/** Extensões de tema e realce do CodeMirror para o tema. */
export function codeMirrorTheme(theme: ThemeName): Extension[] {
  const c = CODE_THEMES[theme];
  const view = EditorView.theme(
    {
      "&": { color: c.text, backgroundColor: "transparent", fontSize: "var(--fs-mono)", height: "100%" },
      ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.25rem", overflow: "auto" },
      ".cm-content": { caretColor: c.caret, padding: "var(--sp-2) 0" },
      "&.cm-focused": { outline: `2px solid ${c.focus}` },
      ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": { backgroundColor: c.selection },
      ".cm-gutters": { backgroundColor: "transparent", color: c.gutter, border: "none" },
      ".cm-lineNumbers .cm-gutterElement": { padding: "0 var(--sp-3) 0 var(--sp-2)" },
    },
    { dark: theme === "dark" },
  );
  const highlight = HighlightStyle.define([
    { tag: [t.keyword, t.operatorKeyword, t.modifier], color: c.keyword },
    { tag: [t.string, t.special(t.string), t.regexp], color: c.string },
    { tag: [t.number, t.bool, t.null, t.atom], color: c.number },
    { tag: [t.comment, t.lineComment, t.blockComment], color: c.comment, fontStyle: "italic" },
    { tag: [t.function(t.variableName), t.function(t.propertyName), t.labelName], color: c.function },
    { tag: [t.typeName, t.className, t.namespace], color: c.type },
    { tag: [t.propertyName, t.attributeName], color: c.property },
    { tag: [t.tagName, t.heading], color: c.tag },
    { tag: [t.operator, t.punctuation, t.meta], color: c.operator },
    { tag: t.link, color: c.link, textDecoration: "underline" },
  ]);
  return [view, syntaxHighlighting(highlight)];
}
