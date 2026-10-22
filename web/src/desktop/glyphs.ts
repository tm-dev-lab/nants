/*
 * Ícones, nunca caracteres (spec 093, decisão 14). Caracteres proibidos como conteúdo de
 * controle, item de menu ou selo, e a varredura do DOM montado que os acha. A varredura do
 * código-fonte fica em `checks.test.ts`; esta, usada pelos testes dos componentes, olha o que
 * de fato aparece num botão, item de menu ou selo.
 *
 * O texto dentro de `<kbd>` é rótulo de tecla (por exemplo "Ctrl+K" ou "Alt+1…9"), não um
 * caractere no lugar de um ícone, e por isso fica de fora.
 */

/** `× ✕ — □ ▢ ▾ ▸ → ← ✓ ✔ • + …` (por código, para não depender da fonte do editor). */
export const FORBIDDEN_GLYPHS: readonly string[] = [
  "×", // ×
  "✕", // ✕
  "—", // —
  "□", // □
  "▢", // ▢
  "▾", // ▾
  "▸", // ▸
  "→", // →
  "←", // ←
  "✓", // ✓
  "✔", // ✔
  "•", // •
  "+",
  "…", // …
];

/** Emoji (pictográficos), também proibidos. */
export const EMOJI = /\p{Extended_Pictographic}/u;

/** Primeiro caractere proibido do texto, ou `null`. */
export function forbiddenGlyphIn(text: string): string | null {
  for (const glyph of FORBIDDEN_GLYPHS) if (text.includes(glyph)) return glyph;
  const emoji = EMOJI.exec(text);
  return emoji === null ? null : emoji[0];
}

/** Controles, itens de menu e selos (o que não pode ter caractere no lugar de ícone). */
const GLYPH_CHECKED_SELECTOR = [
  "button",
  "[role='button']",
  "[role='menuitem']",
  "[role='menuitemradio']",
  "[role='menuitemcheckbox']",
  "[role='tab']",
  "[role='option']",
  "[data-badge]",
].join(", ");

interface GlyphViolation {
  element: Element;
  glyph: string;
  text: string;
}

/** Texto visível do elemento, sem o de `<kbd>` e sem o de SVG. */
function visibleText(element: Element): string {
  let out = "";
  for (const node of Array.from(element.childNodes)) {
    if (node.nodeType === Node.TEXT_NODE) out += node.textContent ?? "";
    else if (node instanceof Element) {
      const tag = node.tagName.toLowerCase();
      if (tag === "kbd" || tag === "svg") continue;
      out += visibleText(node);
    }
  }
  return out;
}

/** Caracteres proibidos em controles, itens de menu e selos dentro de `root`. */
export function findForbiddenGlyphs(root: ParentNode): GlyphViolation[] {
  const out: GlyphViolation[] = [];
  for (const element of Array.from(root.querySelectorAll(GLYPH_CHECKED_SELECTOR))) {
    const text = visibleText(element);
    const glyph = forbiddenGlyphIn(text);
    if (glyph !== null) out.push({ element, glyph, text });
  }
  return out;
}
