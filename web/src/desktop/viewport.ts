/*
 * Viewport do celular (spec 093, decisão 17): o modo celular vale abaixo de 1024 px de largura
 * (o iPad em paisagem, com 1024 ou mais, fica no desktop). Inclui `--app-height` (viewport
 * visual), o modo "teclado aberto" (`<html data-keyboard="open">`, que esconde a barra compacta e
 * as abas) e a revelação do campo focado quando o teclado virtual sobe.
 */
import { useCallback, useEffect, useRef, useState } from "react";

/** Abaixo de 1024 px: modo celular (alternador). */
export const MOBILE_QUERY = "(max-width: 1023.98px)";

/** Largura mínima do desktop (px): o iPad em paisagem (1024) fica no desktop. */
export const DESKTOP_MIN_WIDTH = 1024;

/** Regra pura: a largura (px) é de celular. */
export function isMobileWidth(width: number): boolean {
  return width < DESKTOP_MIN_WIDTH;
}

/** Sem `matchMedia` (ambiente sem layout) assume desktop. */
function mobileNow(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia(MOBILE_QUERY).matches;
}

/** Acompanha o modo celular (`MOBILE_QUERY`): girar o aparelho troca o modo sem recarregar. */
export function useMobileMode(): boolean {
  const [mobile, setMobile] = useState(mobileNow);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return undefined;
    const query = window.matchMedia(MOBILE_QUERY);
    const onChange = (): void => setMobile(query.matches);
    onChange();
    if (typeof query.addEventListener !== "function") return undefined;
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, []);
  return mobile;
}

/** Variável CSS com a altura visível (px) quando o teclado virtual encolhe só a viewport visual. */
export const APP_HEIGHT_VAR = "--app-height";

/**
 * Grava `--app-height` em `<html>` com a altura da viewport visual (spec 082/T06). No iOS e nos
 * navegadores que ignoram `interactive-widget=resizes-content`, o teclado encolhe só a viewport
 * visual e o `100dvh` não muda; o shell usa esta variável e encolhe junto. Atualiza no máximo uma vez
 * por frame. Sem `visualViewport`, não faz nada (vale o `100dvh`). Devolve a função de remoção.
 */
export function installAppHeight(): () => void {
  const vv = typeof window === "undefined" ? undefined : window.visualViewport;
  if (vv === undefined || vv === null) return () => undefined;
  const root = document.documentElement;
  let frame: number | null = null;
  const write = (): void => {
    frame = null;
    // Com zoom de pinça a altura visual diminui; `scale` a devolve ao tamanho do layout.
    const scale = vv.scale > 0 ? vv.scale : 1;
    const height = Math.round(vv.height * scale);
    if (height > 0) root.style.setProperty(APP_HEIGHT_VAR, `${height}px`);
  };
  const schedule = (): void => {
    if (frame !== null) return;
    frame = window.requestAnimationFrame(write);
  };
  write();
  vv.addEventListener("resize", schedule);
  vv.addEventListener("scroll", schedule);
  return () => {
    vv.removeEventListener("resize", schedule);
    vv.removeEventListener("scroll", schedule);
    if (frame !== null) window.cancelAnimationFrame(frame);
    frame = null;
    root.style.removeProperty(APP_HEIGHT_VAR);
  };
}

/** Instala `--app-height` uma vez no shell; remove os listeners ao desmontar. */
export function useAppHeight(): void {
  useEffect(() => installAppHeight(), []);
}

/** Marca em `<html>` (`data-keyboard="open"`) enquanto o teclado virtual está aberto na conversa. */
const KEYBOARD_DATASET_KEY = "keyboard";

/** Atributo dos contêineres de digitação da conversa (caixa de mensagem, formulário de perguntas). */
export const KEYBOARD_AREA_ATTR = "data-keyboard-area";

/** Abaixo desta fração da maior altura vista, o teclado conta como aberto. */
const KEYBOARD_OPEN_RATIO = 0.75;

/** Acima desta fração da maior altura vista, o teclado conta como fechado (histerese). */
const KEYBOARD_CLOSED_RATIO = 0.9;

/** Aparelho de toque (sem mouse): focar um campo de digitar abre o teclado virtual. */
const TOUCH_QUERY = "(hover: none) and (pointer: coarse)";

function touchNow(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" && window.matchMedia(TOUCH_QUERY).matches;
}

function inKeyboardArea(target: EventTarget | null): boolean {
  return opensKeyboard(target) && target.closest(`[${KEYBOARD_AREA_ATTR}]`) !== null;
}

/**
 * Modo "teclado aberto" no celular (spec 083; 141/T11): com foco num campo de digitar dentro de um
 * contêiner `[data-keyboard-area]`, marca `<html data-keyboard="open">`; o CSS esconde a barra de
 * cima, as abas e o cabeçalho da janela para a conversa e o campo ocuparem a altura.
 *
 * - **Aparelho de toque ({@link TOUCH_QUERY}):** liga já no foco. Medir a viewport visual não basta:
 *   no Chrome do iPhone o modo não ligava (141/T11), e a única entrada dele era a viewport visual
 *   encolher abaixo de {@link KEYBOARD_OPEN_RATIO} da maior altura vista, com a mesma largura.
 * - **Sem toque:** liga só quando a altura visível cai abaixo de {@link KEYBOARD_OPEN_RATIO}.
 * - **Desliga** quando o foco sai da área, ou quando a altura, depois de ter encolhido, volta acima de
 *   {@link KEYBOARD_CLOSED_RATIO} (o teclado fechou com o foco no campo); aí só um novo encolhimento ou
 *   um novo foco religa.
 *
 * No desktop, nunca marca. Sem `visualViewport`, vale só o foco num aparelho de toque. Devolve a remoção.
 */
export function installKeyboardMode(): () => void {
  if (typeof window === "undefined") return () => undefined;
  const vv = window.visualViewport ?? null;
  const root = document.documentElement;
  let focused = false;
  let shrunk = false;
  let dismissed = false;
  let maxHeight = 0;
  let width: number | undefined = vv?.width;
  const height = (): number => {
    if (vv === null) return 0;
    const scale = vv.scale > 0 ? vv.scale : 1;
    return vv.height * scale;
  };
  const setOpen = (open: boolean): void => {
    if (open) root.dataset[KEYBOARD_DATASET_KEY] = "open";
    else delete root.dataset[KEYBOARD_DATASET_KEY];
  };
  const evaluate = (): void => {
    // Rotação muda a altura de referência: recomeça a medir na nova largura.
    if (vv !== null && vv.width !== width) {
      width = vv.width;
      maxHeight = 0;
    }
    const current = height();
    if (current > maxHeight) maxHeight = current;
    if (!focused || !mobileNow()) {
      setOpen(false);
      return;
    }
    const measured = maxHeight > 0;
    if (measured && current < maxHeight * KEYBOARD_OPEN_RATIO) {
      shrunk = true;
      dismissed = false;
      setOpen(true);
    } else if (measured && shrunk && current > maxHeight * KEYBOARD_CLOSED_RATIO) {
      shrunk = false;
      dismissed = true;
      setOpen(false);
    } else if (touchNow() && !dismissed) {
      setOpen(true);
    }
    // Entre as duas frações (ou sem toque, sem encolher), o estado fica como está (histerese).
  };
  const onFocusIn = (event: FocusEvent): void => {
    focused = inKeyboardArea(event.target);
    shrunk = false;
    dismissed = false;
    evaluate();
  };
  const onFocusOut = (event: FocusEvent): void => {
    focused = inKeyboardArea(event.relatedTarget);
    evaluate();
  };
  evaluate();
  vv?.addEventListener("resize", evaluate);
  document.addEventListener("focusin", onFocusIn);
  document.addEventListener("focusout", onFocusOut);
  return () => {
    vv?.removeEventListener("resize", evaluate);
    document.removeEventListener("focusin", onFocusIn);
    document.removeEventListener("focusout", onFocusOut);
    setOpen(false);
  };
}

/** Instala o modo "teclado aberto" uma vez no shell; remove listeners e marca ao desmontar. */
export function useKeyboardMode(): void {
  useEffect(() => installKeyboardMode(), []);
}

/** Espera máxima (ms) pelo `resize` da viewport visual depois do foco (teclado abrindo). */
export const KEYBOARD_SETTLE_MS = 300;

/** Silêncio (ms) depois do último `resize` da viewport visual para considerar o teclado assentado. */
export const KEYBOARD_RESIZE_DEBOUNCE_MS = 150;

/** Tipos de `<input>` que abrem o teclado virtual (rádio, caixa e arquivo não abrem). */
const TYPING_INPUTS = new Set(["text", "search", "email", "url", "tel", "password", "number"]);

function opensKeyboard(target: EventTarget | null): target is HTMLElement {
  if (target instanceof HTMLTextAreaElement) return true;
  return target instanceof HTMLInputElement && TYPING_INPUTS.has(target.type);
}

/** Qual elemento trazer à vista: o contêiner inteiro (caixa de mensagem) ou só o campo focado. */
type RevealTarget = "container" | "field";

/**
 * Ao focar um campo de digitar dentro de `container` no celular, espera o teclado assentar e traz à
 * vista o contêiner (campo + botões de anexar e enviar) ou, com `"field"`, só o campo focado, com
 * `scrollIntoView({ block: "nearest" })`, que não salta quando já está visível (spec 083).
 *
 * O teclado sobe em vários `resize` da viewport visual; cada um reagenda a revelação para
 * {@link KEYBOARD_RESIZE_DEBOUNCE_MS} depois do último, e ela roda no quadro seguinte (depois do rAF
 * que aplica o `--app-height`). Sem nenhum `resize`, revela uma vez após {@link KEYBOARD_SETTLE_MS}.
 * Enquanto o foco continuar no contêiner, `resize` posteriores revelam de novo. `focusout` para fora,
 * novo `focusin` ou a remoção limpam timer, quadro e listener. No desktop (≥ 1024 px) não faz nada.
 */
export function installKeyboardReveal(container: HTMLElement, reveal: RevealTarget = "container"): () => void {
  const vv = window.visualViewport ?? null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let frame: number | null = null;
  let field: HTMLElement | null = null;
  const cancelPending = (): void => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (frame !== null && typeof window.cancelAnimationFrame === "function") window.cancelAnimationFrame(frame);
    frame = null;
  };
  const clear = (): void => {
    cancelPending();
    field = null;
    if (vv !== null) vv.removeEventListener("resize", onResize);
  };
  const show = (): void => {
    frame = null;
    const element = reveal === "field" ? field : container;
    if (element === null || !element.isConnected || typeof element.scrollIntoView !== "function") return;
    element.scrollIntoView({ block: "nearest" });
  };
  /** Revela no próximo quadro, depois do rAF que aplica o `--app-height`. */
  const revealAfterFrame = (): void => {
    timer = null;
    if (typeof window.requestAnimationFrame !== "function") {
      show();
      return;
    }
    frame = window.requestAnimationFrame(show);
  };
  const schedule = (delay: number): void => {
    cancelPending();
    timer = setTimeout(revealAfterFrame, delay);
  };
  function onResize(): void {
    schedule(KEYBOARD_RESIZE_DEBOUNCE_MS);
  }
  const onFocusIn = (event: FocusEvent): void => {
    clear();
    if (!opensKeyboard(event.target) || !mobileNow()) return;
    field = event.target;
    schedule(KEYBOARD_SETTLE_MS);
    if (vv !== null) vv.addEventListener("resize", onResize);
  };
  const onFocusOut = (event: FocusEvent): void => {
    const next = event.relatedTarget;
    if (next instanceof Node && container.contains(next)) return;
    clear();
  };
  container.addEventListener("focusin", onFocusIn);
  container.addEventListener("focusout", onFocusOut);
  return () => {
    container.removeEventListener("focusin", onFocusIn);
    container.removeEventListener("focusout", onFocusOut);
    clear();
  };
}

/**
 * Hook de {@link installKeyboardReveal}: devolve um callback ref para o contêiner. Instala quando o
 * elemento monta e remove os listeners quando ele sai (vale também para o formulário de perguntas,
 * que aparece depois).
 */
export function useKeyboardReveal(reveal: RevealTarget = "container"): (element: HTMLElement | null) => void {
  const cleanup = useRef<(() => void) | null>(null);
  return useCallback(
    (element: HTMLElement | null) => {
      cleanup.current?.();
      cleanup.current = element === null ? null : installKeyboardReveal(element, reveal);
    },
    [reveal],
  );
}
