/* Avatar por papel (SVG inline, tom dos tokens) e o anel de estado das bolhas e da Atividade. */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AgentAvatar, avatarStateForActivity } from "./AgentAvatar";

afterEach(cleanup);

describe("AgentAvatar", () => {
  it.each([
    ["architect", "Arquiteto"],
    ["developer", "Desenvolvedor"],
    ["reviewer", "Revisor"],
    ["system", "Sistema"],
  ])("papel %s tem avatar acessível com rótulo %s", (kind, label) => {
    render(<AgentAvatar kind={kind} />);
    const avatar = screen.getByRole("img", { name: label });
    expect(avatar.getAttribute("data-kind")).toBe(kind);
    expect(avatar.querySelector("svg")).not.toBeNull();
  });

  it("papel desconhecido vira avatar neutro genérico sem quebrar", () => {
    render(<AgentAvatar kind="tester-desconhecido" />);
    const avatar = screen.getByRole("img", { name: "Agente" });
    expect(avatar.getAttribute("data-kind")).toBe("unknown");
    expect(avatar.querySelector("svg")).not.toBeNull();
  });

  it("modo decorativo fica oculto para leitor de tela (o nome já está no texto ao lado)", () => {
    const { container } = render(<AgentAvatar kind="architect" decorative />);
    expect(screen.queryByRole("img")).toBeNull();
    expect(container.querySelector("[data-kind]")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("usa só SVG inline: sem imagens externas", () => {
    const { container } = render(<AgentAvatar kind="developer" />);
    expect(container.querySelector("img, image")).toBeNull();
    expect(container.querySelectorAll("[href], [src], [xlink\\:href]")).toHaveLength(0);
  });
});

describe("anel de estado", () => {
  it.each(["thinking", "working", "waiting"] as const)("estado %s mostra o anel e fica aria-hidden", (state) => {
    const { container } = render(<AgentAvatar kind="developer" state={state} />);
    const avatar = container.querySelector("[data-kind='developer']");
    expect(avatar?.getAttribute("aria-hidden")).toBe("true");
    expect(avatar?.getAttribute("role")).toBeNull();
    expect(container.querySelector(`[data-avatar-ring='${state}']`)).not.toBeNull();
  });

  it("sem estado não há anel", () => {
    const { container } = render(<AgentAvatar kind="developer" decorative />);
    expect(container.querySelector("[data-avatar-ring]")).toBeNull();
  });

  it("bolha sem comando pensa; com comando trabalha", () => {
    expect(avatarStateForActivity({})).toBe("thinking");
    expect(avatarStateForActivity({ command: "cargo test" })).toBe("working");
  });

  it("o anel só gira sem movimento reduzido; com a aba oculta, toda animação pausa", () => {
    const apps = readFileSync(resolve(process.cwd(), "src/apps/shared/apps.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const motion = apps.indexOf("@media (prefers-reduced-motion: no-preference)");
    expect(motion).toBeGreaterThan(-1);
    const ring = apps.indexOf('.nd-avatar-ring[data-avatar-ring="thinking"]');
    const animated = apps.slice(ring, apps.indexOf("}", ring));
    expect(ring).toBeGreaterThan(motion);
    expect(animated).toContain("animation");
    const base = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
    expect(base).toMatch(/html\.page-hidden \*[\s\S]*animation-play-state: paused/);
  });
});
