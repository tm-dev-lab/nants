/*
 * Barra superior, dock e lançador conforme o mockup (095/T02; `reference/Main.dc.html` e
 * `reference/Janelas.dc.html`):
 * - barra: o SÍMBOLO REAL do Nants (o asset da marca, não um quadrado colorido), nome e slogan; o
 *   botão de busca largo ("Buscar ou pedir" + o atalho em `<kbd>` sem serifa); no celular (390 px,
 *   C-16), a busca é só o ícone;
 * - dock: botões de 44 px com ícone de 20 px, `--dock-h` coerente com `metrics.ts` e o ícone de
 *   Execução na biblioteca;
 * - lançador: campo de 56 px sem borda, texto de 17 px, raio 16 e resultados em linhas de 44 px.
 * O jsdom não aplica CSS: as medidas são lidas do CSS-fonte.
 */
import { cleanup, render, screen, within } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { ICON_NAMES } from "../Icon";
import { DOCK_BUTTON, DOCK_HEIGHT } from "../metrics";
import { shortcutById } from "../shortcuts";
import { cssRuleBody, cssRules } from "../test-utils";
import { FONT_SIZES, RADII } from "../theme/tokens";
import { TopBar } from "./TopBar";

afterEach(cleanup);

const read = (path: string): string => readFileSync(resolve(process.cwd(), "src", path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const shellCss = read("desktop/theme/shell.css");

const noop = (): void => undefined;

function renderBar(compact = false) {
  return render(
    <TopBar
      spaceId={null}
      space={null}
      projects={[]}
      attention={null}
      attentionCount={0}
      setupIncomplete={false}
      bellOpen={false}
      compact={compact}
      onSwitchSpace={noop}
      onNewProject={noop}
      onOpenSetup={noop}
      onOpenLauncher={noop}
      onToggleBell={noop}
    />,
  );
}

describe("barra superior (Main.dc.html)", () => {
  it("a marca usa o símbolo real do Nants (as três peças da marca), com nome e slogan", () => {
    renderBar();
    const brand = document.querySelector<HTMLElement>("[data-brand]");
    const symbol = brand?.querySelector("svg[data-brand-symbol]");
    expect(symbol).not.toBeNull();
    expect(symbol?.querySelectorAll("path")).toHaveLength(3);
    // O mesmo desenho do asset da marca (`public/brand/nants-symbol-color-dark.svg`).
    const asset = read("../public/brand/nants-symbol-color-dark.svg");
    const assetPaths = [...asset.matchAll(/<path fill="([^"]+)" d="([^"]+)"/g)].map((m) => [m[1], m[2]]);
    const drawn = [...(symbol?.querySelectorAll("path") ?? [])].map((p) => [p.getAttribute("fill"), p.getAttribute("d")]);
    expect(assetPaths).toHaveLength(3);
    expect(drawn).toEqual(assetPaths);
    expect(symbol?.getAttribute("height")).toBe("22");
    expect(brand?.querySelector(".nd-brand-name")?.textContent).toBe("Nants");
    expect(brand?.querySelector("[data-brand-slogan]")?.textContent).not.toBe("");
    // Nome 14/600, sem o 700.
    const name = cssRuleBody(shellCss, ".nd-brand-name");
    expect(name).toContain("font-size: var(--fs-body);");
    expect(name).toContain("font-weight: var(--fw-semibold);");
  });

  it("busca larga: 'Buscar ou pedir' e o atalho em <kbd> sem serifa", () => {
    renderBar();
    const search = document.querySelector<HTMLElement>("[data-search]");
    if (search === null) throw new Error("busca ausente");
    expect(search.querySelector(".nd-search-text")?.textContent).toBe("Buscar ou pedir");
    expect(search.querySelector("kbd")?.textContent).toBe(shortcutById("launcher")?.keys);
    expect(search.getAttribute("aria-keyshortcuts")).toBe("Control+K");
    expect(search.getAttribute("data-variant")).toBeNull();
    const body = cssRuleBody(desktopCss, ".nd-search-btn");
    expect(body).toContain("width: 220px;");
    expect(body).toContain("color: var(--muted);");
    expect(cssRuleBody(desktopCss, ".nd-root kbd")).toContain("font-family: var(--font-sans);");
  });

  it("celular (C-16): a busca mostra só o ícone, com o nome acessível", () => {
    renderBar(true);
    const search = screen.getByRole("button", { name: "Buscar ou pedir" });
    expect(search.getAttribute("data-compact")).toBe("true");
    // Alvo de toque de 44 × 44 (F-01): botão de ícone, coberto pela regra de toque do celular.
    expect(search.classList.contains("nd-iconbtn")).toBe(true);
    expect(search.textContent).toBe("");
    expect(search.querySelector("svg[data-icon='search']")).not.toBeNull();
    expect(cssRuleBody(desktopCss, '.nd-search-btn[data-compact="true"]')).toContain("width: 30px;");
  });

  it("sino: botão neutro do conjunto, com o nome acessível da contagem", () => {
    renderBar();
    const bell = within(document.body).getByRole("button", { name: /Precisa de você/ });
    expect(bell.classList.contains("nd-btn")).toBe(true);
    expect(bell.getAttribute("data-variant")).toBeNull();
  });
});

describe("dock (Main.dc.html)", () => {
  it("botões de 44 px com ícone de 20 px; --dock-h é o botão mais o padding (metrics.ts)", () => {
    expect(DOCK_BUTTON).toBe(44);
    expect(DOCK_HEIGHT).toBe(60);
    const btn = cssRuleBody(desktopCss, ".nd-dock-btn");
    expect(btn).toContain(`width: ${DOCK_BUTTON}px;`);
    expect(btn).toContain(`height: ${DOCK_BUTTON}px;`);
    expect(cssRuleBody(desktopCss, ".nd-root")).toContain(`--dock-h: ${DOCK_HEIGHT}px;`);
    expect(cssRuleBody(desktopCss, ".nd-dock")).toContain("padding: 0 var(--sp-2);");
    expect(read("desktop/Dock.tsx")).toMatch(/<Icon name=\{meta\.icon\} size=\{20\} \/>/);
    // O ponto da janela aberta.
    expect(cssRuleBody(desktopCss, ".nd-dock-indicator")).toContain("width: 4px;");
    // Menus e dock mantêm --elev-3 e --elev-1 (C-16).
    expect(cssRuleBody(desktopCss, ".nd-dock")).toContain("box-shadow: var(--bevel), var(--elev-1);");
    expect(cssRuleBody(desktopCss, ".nd-menu")).toContain("box-shadow: var(--bevel), var(--elev-3);");
  });

  it("o ícone de Execução está na biblioteca", () => {
    expect(ICON_NAMES).toContain("execution");
  });
});

describe("pílula de execução (095/T07, A-01)", () => {
  it("o título se esconde quando cabem menos de ~12 caracteres (encaixe que quebra e corta a 2ª linha)", () => {
    const slot = cssRuleBody(shellCss, ".nd-run-title-slot");
    expect(slot).toContain("flex-wrap: wrap;");
    expect(slot).toContain("overflow: hidden;");
    expect(slot).toContain("min-width: 0;");
    expect(slot).toMatch(/height: (\d+)px;[\s\S]*line-height: \1px;/);
    // O `::before` de largura zero segura a primeira linha; o título desce inteiro quando não cabe.
    expect(cssRuleBody(shellCss, ".nd-run-title-slot::before")).toContain("width: 0;");
    const title = cssRuleBody(shellCss, ".nd-run-title");
    expect(title).toContain("flex: 1 1 12ch;");
    expect(title).toContain("min-width: 12ch;");
  });

  it("fundo neutro, sem contorno, raio --r-lg; a cor só no ponto e no rótulo (o destaque de integrar fica no ponto)", () => {
    const pill = cssRuleBody(shellCss, ".nd-run-pill");
    expect(pill).toContain("border-radius: var(--r-lg);");
    expect(pill).toContain("background: var(--control);");
    expect(pill).not.toMatch(/(?:^|;)\s*(?:border(?!-radius)|outline|box-shadow)/);
    const states = cssRules(shellCss).filter((r) => r.selectors.some((s) => /^\.nd-run-pill\[[^\]]*\]$/.test(s)));
    expect(states).toEqual([]);
  });

  it("spec 123 T02: os detalhes encolhem com reticências; o botão e a fase não encolhem (sem 'Continuar' sobre 'Automático')", () => {
    const details = cssRuleBody(shellCss, ".nd-run-details");
    expect(details).toContain("min-width: 0;");
    expect(details).toContain("overflow: hidden;");
    expect(details).toContain("text-overflow: ellipsis;");
    expect(details).toContain("flex: 0 1 auto;");
    // Encaixe como o do título: abaixo de ~10 caracteres os detalhes saem inteiros (nunca "T...").
    const slot = cssRuleBody(shellCss, ".nd-run-details-slot");
    expect(slot).toContain("flex-wrap: wrap;");
    expect(slot).toContain("overflow: hidden;");
    expect(slot).toContain("min-width: 0;");
    expect(cssRuleBody(shellCss, ".nd-run-details-slot > .nd-run-details")).toContain("min-width: 10ch;");
    expect(cssRuleBody(shellCss, ".nd-run-pill")).toContain("min-width: 0;");
    expect(cssRuleBody(desktopCss, ".nd-pill-btn")).toContain("flex-shrink: 0;");
    expect(cssRuleBody(shellCss, ".nd-run-op")).toContain("flex-shrink: 0;");
  });
});

describe("lançador (Janelas.dc.html)", () => {
  it("campo de 56 px sem borda, texto de 17 px, raio 16 e linhas de 44 px com o contexto à direita", () => {
    const panel = cssRuleBody(shellCss, ".nd-launcher");
    expect(panel).toContain("border-radius: var(--r-xl);");
    expect(RADII.xl).toBe(16);
    expect(panel).not.toMatch(/(?:^|;)\s*border:/);
    const field = cssRuleBody(shellCss, ".nd-launcher-search");
    expect(field).toContain("height: 56px;");
    expect(field).not.toMatch(/(?:^|;)\s*border:/);
    expect(field).not.toMatch(/background/);
    expect(cssRuleBody(shellCss, ".nd-launcher-search input")).toContain("var(--fs-heading)");
    expect(FONT_SIZES.heading).toBe("17px");
    // 095/T07, A-02: nenhum anel nem contorno no campo ou no contêiner dele, com ou sem foco.
    const searchRules = cssRules(shellCss).filter((r) => r.selectors.some((s) => s.includes(".nd-launcher-search")));
    for (const r of searchRules.filter((x) => x.selectors.some((s) => /:focus/.test(s)))) {
      expect(r.body).toMatch(/outline: none;/);
      expect(r.body).toMatch(/box-shadow: none;/);
    }
    expect(searchRules.filter((r) => r.selectors.some((s) => s.includes(".nd-launcher-search:focus")))).toEqual([]);
    const item = cssRuleBody(shellCss, ".nd-launcher-item");
    expect(item).toContain("min-height: 44px;");
    expect(item).toContain("font-size: var(--fs-body);");
    // O contexto ("Abrir janela", "Configurações") à direita, em rótulo.
    expect(cssRuleBody(shellCss, ".nd-launcher-item .nd-menu-hint")).toContain("font-size: var(--fs-label);");
    expect(cssRuleBody(shellCss, ".nd-menu-hint")).toContain("color: var(--muted);");
  });
});
