/* Conjunto de ícones da interface: todo ícone é decorativo, com traço 1.8 e um desenho de verdade. */
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { Icon, ICON_NAMES } from "./Icon";

afterEach(cleanup);

describe("Icon", () => {
  it("o conjunto tem os ícones dos controles mais usados", () => {
    expect(ICON_NAMES.length).toBeGreaterThan(20);
    for (const name of ["close", "check", "copy", "refresh", "stop", "play", "globe"] as const) expect(ICON_NAMES).toContain(name);
  });

  it.each(ICON_NAMES)("%s: decorativo, traço 1.8, tamanho pedido e com desenho", (name) => {
    const { container } = render(<Icon name={name} size={20} />);
    const svg = container.querySelector("svg");
    expect(svg?.getAttribute("aria-hidden")).toBe("true");
    expect(svg?.getAttribute("stroke-width")).toBe("1.8");
    expect(svg?.getAttribute("width")).toBe("20");
    expect(svg?.getAttribute("data-icon")).toBe(name);
    expect(svg?.querySelectorAll("path,circle,rect,line,polyline,polygon,ellipse").length).toBeGreaterThan(0);
  });
});
