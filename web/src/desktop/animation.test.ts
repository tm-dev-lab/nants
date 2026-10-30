import { describe, expect, it, vi } from "vitest";
import { animationFor, playAnimation, prefersReducedMotion, transformFor, waitForAnimations } from "./animation";

const win = { x: 100, y: 50, w: 800, h: 600 };
const max = { x: 0, y: 0, w: 1600, h: 900 };
// Destino de minimizar e origem de restaurar: o botão N da barra de cima (137/T02).
const nButton = { x: 16, y: 4, w: 36, h: 36 };

describe("animationFor (FLIP)", () => {
  it("reduced motion desliga todas", () => {
    expect(animationFor(win, max, true)).toBeNull();
    expect(animationFor(win, nButton, true, "minimize")).toBeNull();
    expect(animationFor(nButton, win, true, "restore")).toBeNull();
  });

  it("maximizar: a caixa já está no destino; parte do inverso da origem até a identidade", () => {
    const a = animationFor(win, max, false);
    expect(a?.keyframes).toEqual([{ transform: "translate(100px, 50px) scale(0.5, 0.6667)" }, { transform: "translate(0px, 0px) scale(1, 1)" }]);
    expect(a?.options.duration).toBeGreaterThan(0);
    // Só transform: nunca left/top/width/height.
    for (const k of a?.keyframes ?? []) expect(Object.keys(k)).toEqual(["transform"]);
  });

  it("mesma geometria não anima", () => {
    expect(animationFor(win, { ...win }, false)).toBeNull();
  });

  it("minimizar: vai da janela até o botão N e some; restaurar é o inverso", () => {
    const m = animationFor(win, nButton, false, "minimize");
    expect(m?.keyframes[0]).toEqual({ transform: "translate(0px, 0px) scale(1, 1)", opacity: 1 });
    expect(m?.keyframes[1]).toEqual({ transform: transformFor(nButton, win), opacity: 0 });
    const r = animationFor(nButton, win, false, "restore");
    expect(r?.keyframes[0]).toEqual({ transform: transformFor(nButton, win), opacity: 0 });
    expect(r?.keyframes[1]).toEqual({ transform: "translate(0px, 0px) scale(1, 1)", opacity: 1 });
  });

  it("caixa de tamanho zero não divide por zero", () => {
    expect(transformFor(win, { x: 0, y: 0, w: 0, h: 0 })).toBe("translate(100px, 50px) scale(1, 1)");
  });
});

describe("waitForAnimations", () => {
  it("sem getAnimations (jsdom) resolve na hora", async () => {
    await expect(waitForAnimations({})).resolves.toBeUndefined();
  });

  it("espera o fim das animações", async () => {
    let finish: () => void = () => undefined;
    const finished = new Promise<Animation>((resolve) => {
      finish = () => resolve({} as Animation);
    });
    const el = { getAnimations: () => [{ finished } as unknown as Animation] };
    let done = false;
    const p = waitForAnimations(el, { setTimer: () => undefined }).then(() => {
      done = true;
    });
    await Promise.resolve();
    expect(done).toBe(false);
    finish();
    await p;
    expect(done).toBe(true);
  });

  it("fallback por tempo quando a animação nunca termina (estado nunca depende de animationend)", async () => {
    const timers: (() => void)[] = [];
    const el = { getAnimations: () => [{ finished: new Promise<Animation>(() => undefined) } as unknown as Animation] };
    const p = waitForAnimations(el, { timeoutMs: 50, setTimer: (fn) => timers.push(fn) });
    expect(timers).toHaveLength(1);
    timers[0]?.();
    await expect(p).resolves.toBeUndefined();
  });

  it("animação cancelada também libera", async () => {
    const el = { getAnimations: () => [{ finished: Promise.reject(new DOMException("cancel", "AbortError")) } as unknown as Animation] };
    await expect(waitForAnimations(el, { setTimer: () => undefined })).resolves.toBeUndefined();
  });
});

describe("playAnimation e reduced motion", () => {
  it("toca só se houver animação e suporte", () => {
    const animate = vi.fn();
    playAnimation({ animate }, animationFor(win, max, false));
    expect(animate).toHaveBeenCalledTimes(1);
    playAnimation({ animate }, null);
    expect(animate).toHaveBeenCalledTimes(1);
    expect(() => playAnimation({}, animationFor(win, max, false))).not.toThrow();
  });

  it("prefersReducedMotion lê o matchMedia (falso sem ele)", () => {
    expect(prefersReducedMotion({ matchMedia: (() => ({ matches: true })) as unknown as Window["matchMedia"] })).toBe(true);
    expect(prefersReducedMotion(undefined)).toBe(false);
  });
});
