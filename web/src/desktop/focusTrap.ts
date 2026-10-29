/*
 * Tab preso num painel modal (o `Dialog` e a folha da prévia da citação, spec 136 T04): do último
 * controle o Tab volta ao primeiro, e do primeiro o Shift+Tab vai ao último.
 */
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

/** Controles que recebem foco pelo Tab. */
export const FOCUSABLE = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled]):not([type='hidden'])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(", ");

/** Trata o Tab dentro de `panel`; outras teclas passam. */
export function trapTab(event: ReactKeyboardEvent<HTMLElement>, panel: HTMLElement | null): void {
  if (event.key !== "Tab" || panel === null) return;
  const items = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE));
  const first = items[0];
  const last = items[items.length - 1];
  if (first === undefined || last === undefined) return;
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}
