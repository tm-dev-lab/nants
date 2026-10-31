/*
 * Animações FLIP das janelas (spec 093, decisão 4): só `transform` (e opacidade); a geometria
 * `left/top/width/height` já está no estado final. O estado NUNCA depende de `animationend`:
 * `waitForAnimations` resolve pelas `getAnimations()` ou, sem elas, na hora; e há teto de tempo.
 */
import type { Rect } from "./windows";

type AnimationKind = "geometry" | "minimize" | "restore";

interface FlipAnimation {
  keyframes: Keyframe[];
  options: KeyframeAnimationOptions;
}

export const ANIMATION_MS: Readonly<Record<AnimationKind, number>> = {
  geometry: 220,
  minimize: 280,
  restore: 260,
};

const EASING = "cubic-bezier(0.2, 0, 0, 1)";
/** Folga além da duração antes de considerar a animação encerrada. */
const WAIT_SLACK_MS = 80;

/** `transform` que leva a caixa `box` (com origem no canto superior esquerdo) ao retângulo `r`. */
export function transformFor(r: Rect, box: Rect): string {
  const tx = Math.round((r.x - box.x) * 100) / 100;
  const ty = Math.round((r.y - box.y) * 100) / 100;
  const sx = box.w > 0 ? Math.round((r.w / box.w) * 10_000) / 10_000 : 1;
  const sy = box.h > 0 ? Math.round((r.h / box.h) * 10_000) / 10_000 : 1;
  return `translate(${tx}px, ${ty}px) scale(${sx}, ${sy})`;
}

const IDENTITY = "translate(0px, 0px) scale(1, 1)";

const sameRect = (a: Rect, b: Rect): boolean => a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

/**
 * Animação de `from` para `to`. `reduced` (prefers-reduced-motion) desliga todas: `null`.
 * - geometry (maximizar/restaurar/encaixar): a caixa já está em `to`; parte do inverso de `from`.
 * - minimize: a caixa continua em `from` (a janela); vai até o botão N (`to`) e some.
 * - restore: a caixa está em `to` (a janela); vem do botão N (`from`) e aparece.
 */
export function animationFor(from: Rect, to: Rect, reduced: boolean, kind: AnimationKind = "geometry"): FlipAnimation | null {
  if (reduced) return null;
  const options: KeyframeAnimationOptions = { duration: ANIMATION_MS[kind], easing: EASING, fill: "none" };
  switch (kind) {
    case "geometry":
      if (sameRect(from, to)) return null;
      return {
        keyframes: [{ transform: transformFor(from, to) }, { transform: IDENTITY }],
        options,
      };
    case "minimize":
      return {
        keyframes: [
          { transform: IDENTITY, opacity: 1 },
          { transform: transformFor(to, from), opacity: 0 },
        ],
        options,
      };
    case "restore":
      return {
        keyframes: [
          { transform: transformFor(from, to), opacity: 0 },
          { transform: IDENTITY, opacity: 1 },
        ],
        options,
      };
  }
}

/** Elemento que pode (ou não) animar: em jsdom não há `animate` nem `getAnimations`. */
interface Animatable {
  animate?: (keyframes: Keyframe[], options: KeyframeAnimationOptions) => Animation;
  getAnimations?: () => Animation[];
}

/** Toca a animação, se o ambiente suportar. */
export function playAnimation(element: Animatable, animation: FlipAnimation | null): void {
  if (animation === null || typeof element.animate !== "function") return;
  element.animate(animation.keyframes, animation.options);
}

interface WaitOptions {
  timeoutMs?: number;
  setTimer?: (fn: () => void, ms: number) => unknown;
}

/**
 * Espera as animações do elemento terminarem. Sem `getAnimations` ou sem animações, resolve na
 * hora; com elas, no fim de todas (canceladas também contam) ou no teto de tempo, o que vier antes.
 */
export function waitForAnimations(element: Animatable, opts: WaitOptions = {}): Promise<void> {
  const animations = typeof element.getAnimations === "function" ? element.getAnimations() : [];
  if (animations.length === 0) return Promise.resolve();
  const timeoutMs = opts.timeoutMs ?? Math.max(...Object.values(ANIMATION_MS)) + WAIT_SLACK_MS;
  const setTimer = opts.setTimer ?? ((fn: () => void, ms: number) => setTimeout(fn, ms));
  const finished = Promise.all(animations.map((a) => a.finished.catch(() => undefined))).then(() => undefined);
  const timeout = new Promise<void>((resolve) => {
    setTimer(resolve, timeoutMs);
  });
  return Promise.race([finished, timeout]);
}

/** `prefers-reduced-motion: reduce` (falso sem `matchMedia`). */
export function prefersReducedMotion(win: Pick<Window, "matchMedia"> | undefined = globalThis.window): boolean {
  if (win === undefined || typeof win.matchMedia !== "function") return false;
  return win.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Acompanha o `prefers-reduced-motion` (spec 138 T04: o avatar do Nanty para na pose quando a
 * pessoa pede menos movimento e volta a se mexer quando ela desliga). Sem `matchMedia`, nada.
 */
export function subscribeReducedMotion(listener: () => void, win: Pick<Window, "matchMedia"> | undefined = globalThis.window): () => void {
  if (win === undefined || typeof win.matchMedia !== "function") return () => undefined;
  const query = win.matchMedia("(prefers-reduced-motion: reduce)");
  if (typeof query.addEventListener !== "function") return () => undefined;
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}
