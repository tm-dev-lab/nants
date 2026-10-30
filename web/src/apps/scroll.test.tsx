/*
 * Uma barra de rolagem por janela (spec 094, decisão 4). O jsdom não aplica CSS: o contrato é
 * conferido de forma ESTÁTICA no CSS-fonte e no código de cada app do registro, e a ligação
 * registro → janela no shell montado.
 * - `AppDefinition.scroll` é `"app"` ou `"window"` em todo app;
 * - uma regra CSS única zera o `overflow` do conteúdo da janela quando o app rola sozinho;
 * - o app `"app"` tem exatamente um contêiner com a classe `nd-app-scroll`; o `"window"`, nenhum.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { act, cleanup, fireEvent } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { openNMenu, renderShell, settle } from "../desktop/shell-test-utils";
import { cssRules } from "../desktop/test-utils";
import { WINDOW_KINDS } from "../desktop/windows";
import { makeProject, makeProjectState } from "../test-utils";
import { DEFAULT_APPS } from "./registry";

afterEach(cleanup);

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");

/** Arquivos `.css` de `web/src` (relativos a `src/`). */
function cssFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(resolve(SRC, dir))) {
    const path = join(dir, name);
    if (statSync(resolve(SRC, path)).isDirectory()) out.push(...cssFiles(path));
    else if (name.endsWith(".css")) out.push(path);
  }
  return out;
}

/** Código-fonte (sem testes) de um app: `apps/<tipo>/**.tsx`. */
function appSources(kind: string): string[] {
  const dir = resolve(SRC, "apps", kind);
  const out: string[] = [];
  const walk = (path: string): void => {
    for (const name of readdirSync(path)) {
      const full = join(path, name);
      if (statSync(full).isDirectory()) walk(full);
      else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(full);
    }
  };
  walk(dir);
  return out;
}

/** Contêineres com a classe de rolagem: literais de classe com `nd-app-scroll` (sem comentários). */
function scrollContainers(source: string): number {
  const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  return (code.match(/className=(?:"|\{`|\{")[^"`]*\bnd-app-scroll\b/g) ?? []).length;
}

/** Classes do contêiner `nd-app-scroll` de um app (o literal de `className` dele). */
function scrollContainerClasses(kind: string): string[] {
  for (const file of appSources(kind)) {
    const code = readFileSync(file, "utf8");
    const match = /className=(?:"|\{`|\{")([^"`]*\bnd-app-scroll\b[^"`]*)/.exec(code);
    if (match !== null) return (match[1] ?? "").split(/\s+/).filter((c) => c !== "" && !c.startsWith("$"));
  }
  return [];
}

/** Corpos das regras de nível do arquivo com exatamente o seletor `.classe` (em todo CSS). */
function classBodies(cls: string): string[] {
  return cssFiles(".").flatMap((file) =>
    cssRules(read(file))
      .filter((rule) => rule.at === null && rule.selectors.includes(`.${cls}`))
      .map((rule) => rule.body),
  );
}

/** Declarações de `overflow` de um corpo de regra. */
const overflowOf = (body: string): string[] =>
  body
    .split(";")
    .map((d) => d.trim())
    .filter((d) => /^overflow(?:-[xy])?:/.test(d));

describe("contrato de rolagem (estático)", () => {
  it("todo tipo de janela tem app no registro com scroll 'app' ou 'window'", () => {
    for (const kind of WINDOW_KINDS) {
      const app = DEFAULT_APPS[kind];
      expect({ kind, scroll: app?.scroll }).toEqual({ kind, scroll: expect.stringMatching(/^(?:app|window)$/) });
    }
  });

  it("app 'app': exatamente um contêiner nd-app-scroll; app 'window': nenhum", () => {
    for (const kind of WINDOW_KINDS) {
      const count = appSources(kind).reduce((n, file) => n + scrollContainers(readFileSync(file, "utf8")), 0);
      const expected = DEFAULT_APPS[kind]?.scroll === "app" ? 1 : 0;
      expect({ kind, count }).toEqual({ kind, count: expected });
    }
  });

  it("uma regra única zera o overflow do conteúdo da janela quando o app rola sozinho", () => {
    const all = cssFiles(".").flatMap((file) => cssRules(read(file)).map((rule) => ({ file, rule })));
    const scrollRules = all.filter(({ rule }) => rule.selectors.some((s) => s.includes("data-scroll")));
    expect(scrollRules.map(({ file, rule }) => [file, rule.selectors, overflowOf(rule.body)])).toEqual([
      ["desktop/theme/desktop.css", ['.nd-content[data-scroll="app"]'], ["overflow: hidden"]],
    ]);
    // Nenhuma outra regra devolve a rolagem vertical ao conteúdo da janela (a base rola: a janela
    // "window"); a janela estreita só corta a horizontal.
    const contentOverflow = all.filter(
      ({ rule }) => rule.selectors.some((s) => /\.nd-content\b/.test(s)) && overflowOf(rule.body).length > 0,
    );
    expect(contentOverflow.map(({ rule }) => [rule.selectors, overflowOf(rule.body)])).toEqual([
      [[".nd-content"], ["overflow: auto"]],
      [['.nd-content[data-scroll="app"]'], ["overflow: hidden"]],
      [['.nd-content[data-size="narrow"]'], ["overflow-x: hidden"]],
    ]);
    // A classe de rolagem do app rola.
    const marker = all.filter(({ rule }) => rule.selectors.includes(".nd-app-scroll"));
    expect(marker.map(({ rule }) => overflowOf(rule.body))).toEqual([["overflow: auto"]]);
  });

  it("em cada app 'app', o nd-app-scroll é o elemento flexível principal (ocupa a altura), sem altura limitada", () => {
    for (const kind of WINDOW_KINDS) {
      if (DEFAULT_APPS[kind]?.scroll !== "app") continue;
      const classes = scrollContainerClasses(kind);
      const bodies = classes.flatMap(classBodies).join(";");
      const fills = /(?:^|;)\s*flex:\s*1\b/.test(bodies) || /(?:^|;)\s*height:\s*100%/.test(bodies);
      expect({ kind, classes, fills, limited: /max-height/.test(bodies) }).toEqual({ kind, classes, fills: true, limited: false });
    }
  });

  it("App rola sozinho (094/T05; 095/T03): a raiz é o nd-app-scroll; só o palco, sem barra nem console", () => {
    expect(DEFAULT_APPS.app?.scroll).toBe("app");
    // A raiz do App é o contêiner de rolagem (o elemento principal da janela).
    expect(scrollContainerClasses("app")).toEqual(["nd-app", "nd-app-scroll"]);
    const appCss = read("apps/app/app.css");
    const body = (selector: string): string => cssRules(appCss).find((r) => r.at === null && r.selectors.includes(selector))?.body ?? "";
    expect(body(".nd-app")).toMatch(/height: 100%/);
    expect(body(".nd-app-browser")).toMatch(/flex: 1/);
    expect(body(".nd-app-browser")).not.toMatch(/min-height:\s*0/);
    const stage = body(".nd-app-stage");
    expect(stage).toContain("min-height: 160px");
    expect(stage).toContain("position: relative");
    // Sem `contain: size` no palco: o iframe sai do fluxo e ocupa o palco por posição absoluta.
    expect(stage).not.toMatch(/contain:/);
    const frameBody = body(".nd-app-frame");
    expect(frameBody).toContain("position: absolute");
    expect(frameBody).toContain("inset: 0");
    // O console saiu do App (está na Execução): nenhuma regra do console aqui.
    expect(cssRules(appCss).filter((r) => r.selectors.some((sel) => sel.includes("nd-app-console")))).toEqual([]);
  });

  it("Execução rola sozinha (095/T03): a raiz é o nd-app-scroll; o console ocupa o resto com rolagem própria", () => {
    expect(DEFAULT_APPS.execution?.scroll).toBe("app");
    expect(scrollContainerClasses("execution")).toEqual(["nd-exec", "nd-app-scroll"]);
    const css = read("apps/execution/execution.css");
    const body = (selector: string): string => cssRules(css).find((r) => r.at === null && r.selectors.includes(selector))?.body ?? "";
    expect(body(".nd-exec")).toMatch(/height: 100%/);
    expect(body(".nd-exec-console")).toMatch(/flex: 1/);
    expect(body(".nd-exec-console")).toMatch(/min-height: 180px/);
    expect(body(".nd-app-log")).toContain("overflow-y: auto");
  });

  it("Conversa na altura mínima: a lista vai a zero; o aviso e os anexos rolam; o campo nunca encolhe", () => {
    const css = read("apps/shared/apps.css");
    const body = (selector: string): string => cssRules(css).find((r) => r.at === null && r.selectors.includes(selector))?.body ?? "";
    expect(body(".nd-conversation")).toMatch(/flex-direction: column[\s\S]*height: 100%/);
    expect(body(".nd-scroll-wrap")).toMatch(/flex: 1;[\s\S]*min-height: 0/);
    expect(body(".nd-conversation-scroll")).toMatch(/flex: 1;[\s\S]*min-height: 0/);
    const dock = body(".nd-composer-dock");
    expect(dock).toContain("flex: 0 1 auto");
    expect(dock).toContain("min-height: 0");
    for (const selector of [".nd-composer-dock > .nd-readiness", ".nd-composer-dock .nd-draft-list"]) {
      expect({ selector, body: body(selector).replace(/\s+/g, " ").trim() }).toEqual({
        selector,
        body: "flex: 0 1 auto; min-height: 0; overflow: auto;",
      });
    }
    expect(body(".nd-composer-dock > form")).toMatch(/flex-direction: column[\s\S]*min-height: 0/);
    expect(body(".nd-composer-dock .nd-composer")).toContain("flex-shrink: 0");
    // Nenhuma regra limita a altura do campo nem o tira do fluxo.
    expect(body(".nd-composer")).not.toMatch(/max-height|position: absolute/);
  });

  it("a contagem reconhece className literal e template, e ignora comentário", () => {
    expect(scrollContainers('<div className="a nd-app-scroll" />')).toBe(1);
    expect(scrollContainers("<div className={`a nd-app-scroll ${b}`} />")).toBe(1);
    expect(scrollContainers('// className="nd-app-scroll"\n<div className="nd-app-scrollbar" />')).toBe(0);
  });
});

/** Regras de nível do arquivo que valem para um elemento só pelas classes dele. */
function bodiesForElement(el: Element): string {
  return Array.from(el.classList).flatMap(classBodies).join(";");
}

describe("contrato de rolagem (shell montado)", () => {
  it("em toda janela 'app' aberta, o nd-app-scroll e cada ancestral até a janela ocupam a altura toda", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    });
    for (const kind of WINDOW_KINDS) {
      if (kind === "setup") continue;
      if (document.querySelector(`[data-window="${kind}"]`) !== null) continue;
      // Pelo menu do N; o app fora da grade fecha o menu sem abrir nada.
      const tile = openNMenu().querySelector<HTMLElement>(`.nd-nmenu-tile[data-kind="${kind}"]`);
      await act(async () => {
        if (tile !== null) fireEvent.click(tile);
        else fireEvent.keyDown(window, { key: "Escape" });
      });
    }
    await settle();
    const checked: string[] = [];
    for (const kind of WINDOW_KINDS) {
      if (DEFAULT_APPS[kind]?.scroll !== "app") continue;
      const content = document.querySelector(`[data-window="${kind}"] [data-window-content]`);
      const scroller = content?.querySelector(".nd-app-scroll");
      if (content == null || scroller == null) continue;
      checked.push(kind);
      // Do contêiner de rolagem até o conteúdo da janela: ninguém de altura limitada no caminho.
      for (let el: Element | null = scroller; el !== null && el !== content; el = el.parentElement) {
        const bodies = bodiesForElement(el);
        const fills =
          /(?:^|;)\s*flex:\s*1\b/.test(bodies) ||
          /(?:^|;)\s*height:\s*100%/.test(bodies) ||
          // Invólucros sem regra de altura própria herdam o fluxo do pai (só o próprio contêiner precisa preencher).
          (el !== scroller && !/(?:^|;)\s*(?:max-)?height:/.test(bodies) && el.children.length === 1);
        const limited = /max-height|(?:^|;)\s*height:\s*(?!100%)\d/.test(bodies);
        expect({ kind, element: el.className, fills, limited }).toEqual({ kind, element: el.className, fills: true, limited: false });
      }
    }
    // As janelas que montam o contêiner sem dados de spec (as demais mostram o estado vazio).
    expect(checked).toEqual(expect.arrayContaining(["conversation", "activity", "files", "settings", "project"]));
  });


  it("o registro chega ao conteúdo da janela como data-scroll", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    });
    for (const kind of ["conversation", "plan"] as const) {
      const content = document.querySelector(`[data-window="${kind}"] [data-window-content]`);
      expect(content?.getAttribute("data-scroll")).toBe(DEFAULT_APPS[kind]?.scroll);
    }
    // Exatamente um contêiner de rolagem em cada janela do app.
    expect(document.querySelectorAll('[data-window="conversation"] .nd-app-scroll')).toHaveLength(1);
    // Conversa: a lista (dentro do invólucro flexível) e, depois dela, o composer fixo.
    const conversation = document.querySelector('[data-window="conversation"] .nd-conversation');
    const children = Array.from(conversation?.children ?? []).map((el) => el.className);
    expect(children.indexOf("nd-scroll-wrap")).toBeGreaterThanOrEqual(0);
    expect(children.indexOf("nd-composer-dock")).toBe(children.length - 1);
    expect(conversation?.querySelector(".nd-scroll-wrap > .nd-app-scroll")).not.toBeNull();
    expect(conversation?.querySelector(".nd-composer-dock > form .nd-composer textarea")).not.toBeNull();
  });
});
