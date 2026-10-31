/*
 * Avatar do Nanty (spec 138 T04, critério 1): os cinco estados com o desenho e as animações da
 * referência, as cores da marca pelos tokens e a pose parada com `prefers-reduced-motion`.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { cssRules } from "../../desktop/test-utils";
import { NANTY } from "../../desktop/theme/tokens";
import { NantyAvatar, NANTY_MOODS, type NantyMood } from "./NantyAvatar";

const css = readFileSync(resolve(process.cwd(), "src/apps/nanty/nanty.css"), "utf8");

/** `matchMedia` falso só para o movimento reduzido; `set` simula a pessoa mudar a preferência. */
function installReducedMotion(initial: boolean) {
  let reduce = initial;
  const listeners = new Set<() => void>();
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    media: query,
    get matches() {
      return query === "(prefers-reduced-motion: reduce)" ? reduce : false;
    },
    addEventListener: (_type: string, listener: () => void) => listeners.add(listener),
    removeEventListener: (_type: string, listener: () => void) => listeners.delete(listener),
  })) as unknown as typeof window.matchMedia;
  return {
    set(next: boolean): void {
      reduce = next;
      act(() => listeners.forEach((listener) => listener()));
    },
    restore(): void {
      if (original === undefined) Reflect.deleteProperty(window, "matchMedia");
      else window.matchMedia = original;
    },
  };
}

let media: ReturnType<typeof installReducedMotion> | null = null;

afterEach(() => {
  cleanup();
  media?.restore();
  media = null;
});

function avatar(mood: NantyMood, extra: { still?: boolean; label?: string } = {}): SVGSVGElement {
  const { container } = render(<NantyAvatar mood={mood} size={96} {...extra} />);
  const svg = container.querySelector<SVGSVGElement>("svg[data-nanty-avatar]");
  if (svg === null) throw new Error("avatar ausente");
  return svg;
}

/** Classes animadas que cada estado precisa ter, como na referência. */
const ANIMATED: Readonly<Record<NantyMood, string[]>> = {
  attentive: [".nd-nanty-sway[data-side=left]", ".nd-nanty-sway[data-side=right]", ".nd-nanty-tip", ".nd-nanty-blink"],
  speaking: [".nd-nanty-wiggle[data-side=left]", ".nd-nanty-wiggle[data-side=right]", ".nd-nanty-talk", ".nd-nanty-blink"],
  thinking: [".nd-nanty-tilt", ".nd-nanty-look"],
  sleeping: [".nd-nanty-breathe", ".nd-nanty-droop[data-side=left]", ".nd-nanty-droop[data-side=right]", ".nd-nanty-z[data-z='1']", ".nd-nanty-z[data-z='2']"],
  waiting: [".nd-nanty-perk", ".nd-nanty-wave", ".nd-nanty-dot", ".nd-nanty-blink"],
};

describe("NantyAvatar", () => {
  it("os cinco estados, na ordem da referência", () => {
    expect(NANTY_MOODS).toEqual(["attentive", "speaking", "thinking", "sleeping", "waiting"]);
  });

  it.each(NANTY_MOODS)("%s: o desenho do estado, com as partes que se mexem", (mood) => {
    const svg = avatar(mood);
    expect(svg.getAttribute("data-mood")).toBe(mood);
    expect(svg.getAttribute("viewBox")).toBe("0 0 64 64");
    for (const selector of ANIMATED[mood]) expect({ mood, selector, found: svg.querySelector(selector) !== null }).toEqual({ mood, selector, found: true });
    // Só as partes do próprio estado: nenhum estado leva a animação de outro.
    const others = NANTY_MOODS.filter((m) => m !== mood).flatMap((m) => ANIMATED[m]).filter((s) => !ANIMATED[mood].includes(s));
    for (const selector of others) expect({ mood, selector, found: svg.querySelector(selector) !== null }).toEqual({ mood, selector, found: false });
  });

  it.each(NANTY_MOODS)("%s: a cabeça e as antenas nas três cores da marca, pelos tokens", (mood) => {
    const svg = avatar(mood);
    expect(svg.querySelector("rect")?.getAttribute("fill")).toBe(NANTY.head);
    const strokes = Array.from(svg.querySelectorAll(".nd-nanty-antenna")).map((p) => p.getAttribute("stroke"));
    expect(strokes).toEqual([NANTY.antennaLeft, NANTY.antennaRight]);
  });

  it("dormindo tem os olhos fechados e o z; esperando você tem o ponto âmbar", () => {
    const sleeping = avatar("sleeping");
    expect(sleeping.querySelectorAll("ellipse")).toHaveLength(0);
    expect(Array.from(sleeping.querySelectorAll("text")).map((z) => z.textContent)).toEqual(["z", "z"]);
    cleanup();
    expect(avatar("waiting").querySelector(".nd-nanty-dot")?.getAttribute("fill")).toBe(NANTY.waiting);
  });

  it("com movimento: data-motion live; com prefers-reduced-motion: parado na pose, e acompanha a troca", () => {
    media = installReducedMotion(false);
    const svg = avatar("attentive");
    expect(svg.getAttribute("data-motion")).toBe("live");
    media.set(true);
    expect(svg.getAttribute("data-motion")).toBe("still");
    // A pose continua desenhada (o mesmo desenho, sem animar).
    expect(svg.querySelector(".nd-nanty-sway")).not.toBeNull();
    media.set(false);
    expect(svg.getAttribute("data-motion")).toBe("live");
  });

  it.each(NANTY_MOODS)("%s: com movimento reduzido desde o início, já nasce parado", (mood) => {
    media = installReducedMotion(true);
    expect(avatar(mood).getAttribute("data-motion")).toBe("still");
  });

  it("`still` força a pose parada (a foto do bot)", () => {
    expect(avatar("waiting", { still: true }).getAttribute("data-motion")).toBe("still");
  });

  it("decorativo sem nome; com nome, imagem com o rótulo", () => {
    expect(avatar("attentive").getAttribute("aria-hidden")).toBe("true");
    cleanup();
    const named = avatar("thinking", { label: "Nanty, pensando" });
    expect(named.getAttribute("role")).toBe("img");
    expect(named.getAttribute("aria-label")).toBe("Nanty, pensando");
    expect(named.hasAttribute("aria-hidden")).toBe(false);
  });

  it("CSS: toda animação só vale com data-motion live e sem pedido de menos movimento", () => {
    const rules = cssRules(css);
    const animated = rules.filter((r) => /(^|;)\s*animation:/.test(r.body));
    expect(animated.length).toBe(15);
    for (const rule of animated) {
      expect({ selectors: rule.selectors, at: rule.at }).toEqual({ selectors: rule.selectors, at: "@media (prefers-reduced-motion: no-preference)" });
      for (const selector of rule.selectors) expect(selector.startsWith('.nd-nanty[data-motion="live"] ')).toBe(true);
    }
    // Cada animação usada tem o @keyframes dela.
    for (const rule of animated) {
      const name = /animation:\s*([\w-]+)/.exec(rule.body)?.[1] ?? "";
      expect({ name, defined: css.includes(`@keyframes ${name} `) }).toEqual({ name, defined: true });
    }
  });
});
