/*
 * Regras de foco do gerenciador (spec 093, decisão 4; WCAG 2.4.11):
 * - minimizar ou fechar → foco no botão N da barra de cima (spec 137/T02: o dock saiu; as janelas
 *   minimizadas ficam no menu do N, e é dele que a janela volta);
 * - restaurar → o último elemento focado dentro da janela (senão a barra de título);
 * - minimizada = `hidden` + `inert` (nunca `aria-hidden` por sobreposição);
 * - F6/Shift+F6 percorrem as janelas visíveis na ordem dos apps (`APP_ORDER`).
 */
import { isShown, WINDOW_KINDS, type Layout, type WindowKind, type WindowState } from "./windows";

/** Ids estáveis de DOM usados pelas regras de foco. */
/** Botão N (abre o menu do N): destino do foco ao minimizar ou fechar e da animação de minimizar. */
export const N_BUTTON_ID = "nd-nbutton";
export const windowTitleBarId = (kind: WindowKind): string => `window-${kind}-titlebar`;
export const windowTitleId = (kind: WindowKind): string => `window-${kind}-title`;

export type FocusTarget =
  | { type: "nButton" }
  | { type: "element"; element: HTMLElement }
  /**
   * `ring`: a navegação foi por teclado (F6, Esc) e o anel aparece. Sem ele (abrir pelo menu do N, pelo
   * lançador, restaurar), o foco é programático e a barra NÃO mostra anel (spec 095, decisão 2).
   */
  | { type: "titleBar"; kind: WindowKind; ring?: boolean };

/** Atributo do foco programático sem anel (some no primeiro toque de tecla ou ao sair). */
export const QUIET_FOCUS_ATTR = "data-quiet-focus";

/**
 * Marca o foco programático como silencioso: o CSS não desenha o anel enquanto o atributo existir.
 * Qualquer tecla (o usuário passou a navegar pelo teclado) ou a saída do foco o remove.
 */
function markQuiet(element: HTMLElement): void {
  element.setAttribute(QUIET_FOCUS_ATTR, "true");
  const clear = (): void => {
    element.removeAttribute(QUIET_FOCUS_ATTR);
    element.removeEventListener("keydown", clear);
    element.removeEventListener("blur", clear);
  };
  element.addEventListener("keydown", clear);
  element.addEventListener("blur", clear);
}

/** Para onde vai o foco depois de minimizar ou fechar. */
export function focusAfterHide(): FocusTarget {
  return { type: "nButton" };
}

/** Último elemento focado em cada janela (só referências; nada vai para o estado salvo). */
interface FocusMemory {
  remember(kind: WindowKind, element: HTMLElement): void;
  recall(kind: WindowKind): HTMLElement | null;
  forget(kind: WindowKind): void;
}

export function createFocusMemory(): FocusMemory {
  const map = new Map<WindowKind, HTMLElement>();
  return {
    remember: (kind, element) => {
      map.set(kind, element);
    },
    recall: (kind) => map.get(kind) ?? null,
    forget: (kind) => {
      map.delete(kind);
    },
  };
}

/** Para onde vai o foco ao restaurar: o último elemento (se ainda estiver no documento), senão a barra. */
export function focusAfterRestore(kind: WindowKind, memory: FocusMemory): FocusTarget {
  const element = memory.recall(kind);
  if (element !== null && element.isConnected) return { type: "element", element };
  return { type: "titleBar", kind };
}

/** Resolve o alvo no documento e foca (sem rolar). Devolve se conseguiu. */
export function applyFocus(target: FocusTarget, doc: Document = document): boolean {
  const element =
    target.type === "element"
      ? target.element
      : target.type === "nButton"
        ? doc.getElementById(N_BUTTON_ID)
        : doc.getElementById(windowTitleBarId(target.kind));
  if (element === null) return false;
  if (target.type === "titleBar" && target.ring !== true) {
    // Já focada: nada muda (o anel de quem já navegava pelo teclado fica).
    if (doc.activeElement === element) return true;
    markQuiet(element);
  }
  element.focus({ preventScroll: true });
  if (doc.activeElement !== element) element.removeAttribute(QUIET_FOCUS_ATTR);
  return doc.activeElement === element;
}

/** Atributos de ocultação de uma janela: minimizada ou fechada = `hidden` + `inert`. */
export function windowHiddenAttrs(win: WindowState | undefined): { hidden: boolean; inert: boolean } {
  const hidden = !isShown(win);
  return { hidden, inert: hidden };
}

/** Próxima/anterior janela visível na ordem dos apps (F6/Shift+F6), com volta. */
export function cycleWindow(
  layout: Layout,
  current: WindowKind | null,
  direction: 1 | -1,
  order: readonly WindowKind[] = WINDOW_KINDS,
): WindowKind | null {
  const shown = order.filter((kind) => isShown(layout.windows[kind]));
  if (shown.length === 0) return null;
  const index = current === null ? -1 : shown.indexOf(current);
  if (index === -1) return direction === 1 ? (shown[0] ?? null) : (shown[shown.length - 1] ?? null);
  return shown[(index + direction + shown.length) % shown.length] ?? null;
}
