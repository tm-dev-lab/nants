/*
 * Botões e controles neutros (spec 095, decisão 9; pedido do usuário: "muito colorido… mais
 * neutro"):
 * - conjunto único de variantes de `.nd-btn` (primária violeta, secundária neutra, fantasma/ícone
 *   e perigo rosa suave), usado por todas as janelas, pela barra e pelas sobreposições; os demais
 *   botões são controles especializados (abas, menus, dock…), numa lista fechada com o motivo;
 * - no máximo UMA primária visível por janela nos estados testados;
 * - YOLO, Internet e Layout neutros (a cor só no pino do interruptor quando ligado);
 * - pílulas de estado com fundo neutro (o tom só no ponto e no rótulo);
 * - ícones do dock neutros (o tom fica só no ícone da barra de título).
 */
import { act, cleanup, fireEvent } from "@testing-library/react";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState } from "../api/types";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import { applyAttention } from "./attention";
import { presetWindows } from "./presets";
import { renderShell, settle } from "./shell-test-utils";
import { cssRuleBody, cssRules } from "./test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  applyAttention(0);
});

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");

function walk(dir: string, ext: RegExp): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full, ext));
    else if (ext.test(name)) out.push(full);
  }
  return out;
}

const rel = (full: string): string => relative(SRC, full).split(sep).join("/");
const sources = (): { file: string; text: string }[] =>
  walk(SRC, /\.tsx$/)
    .filter((f) => !f.endsWith(".test.tsx") && !f.endsWith("test-utils.tsx"))
    .map((full) => ({ file: rel(full), text: readFileSync(full, "utf8") }));
const allCss = (): { file: string; css: string }[] => walk(SRC, /\.css$/).map((full) => ({ file: rel(full), css: readFileSync(full, "utf8") }));

/** Variantes do conjunto único (`data-variant` de `.nd-btn`); a secundária é a ausência dele. */
const VARIANTS = ["primary", "ghost", "danger"] as const;

/**
 * Botões que NÃO são do conjunto (controles especializados, com forma e estado próprios), com o
 * motivo. Lista fechada: um botão novo fora do `.nd-btn` precisa entrar aqui.
 */
const SPECIAL: ReadonlyMap<string, string> = new Map([
  ["nd-wbtn", "controles da janela e de fechar: só o ícone, fundo no hover"],
  ["nd-menu-item", "item de menu (menus, sino, lançador)"],
  ["nd-dock-btn", "ícone do dock (neutro, estado pelo ponto)"],
  ["nd-tab", "aba (Atividade e outras janelas)"],
  ["nd-settings-tab", "aba de Configurações"],
  ["nd-segment-btn", "segmento de controle segmentado (Arquivos)"],
  ["nd-choice", "opção visual em grade (Aparência)"],
  ["nd-conn-method", "cartão de como conectar (chave de API ou plano ChatGPT, spec 116)"],
  ["nd-link", "link em linha (trilha de pastas)"],
  ["nd-link-btn", "link em linha dentro do texto"],
  ["nd-disclosure-btn", "abre e fecha uma seção (mostrar mais)"],
  ["nd-board-card", "cartão clicável do Quadro"],
  ["nd-step", "passo do assistente de Primeiros passos"],
  ["nd-switcher-item", "item do Alternador do celular"],
  ["nd-mobile-tab", "aba do celular"],
  ["nd-folder-item", "item da lista de pastas"],
  ["nd-draft-thumb", "miniatura de imagem do rascunho"],
  ["nd-thumb-btn", "miniatura de imagem que amplia"],
]);

/** Abertura de cada `<button …>` no fonte (com expressões `{…}` que podem conter `>`). */
function buttonTags(text: string): string[] {
  const out: string[] = [];
  let at = text.indexOf("<button");
  while (at !== -1) {
    let depth = 0;
    let quote: string | null = null;
    let i = at + "<button".length;
    for (; i < text.length; i += 1) {
      const c = text[i];
      if (quote !== null) {
        if (c === quote) quote = null;
      } else if (c === '"' || c === "'" || c === "`") quote = c;
      else if (c === "{") depth += 1;
      else if (c === "}") depth -= 1;
      else if (c === ">" && depth === 0) break;
    }
    out.push(text.slice(at, i + 1));
    at = text.indexOf("<button", i);
  }
  return out;
}

/** Valor de um atributo: o literal sem aspas, ou a expressão começando com `{`; `null` se ausente. */
function attr(tag: string, name: string): string | null {
  const start = new RegExp(`\\s${name}=`).exec(tag);
  if (start === null) return null;
  const from = start.index + start[0].length;
  if (tag[from] === '"') return tag.slice(from + 1, tag.indexOf('"', from + 1));
  let depth = 0;
  for (let i = from; i < tag.length; i += 1) {
    if (tag[i] === "{") depth += 1;
    else if (tag[i] === "}") {
      depth -= 1;
      if (depth === 0) return tag.slice(from, i);
    }
  }
  return null;
}

/** Variantes que um `data-variant` pode valer: o literal, ou os resultados (`? "x"`/`: "y"`) da expressão. */
function variantValues(raw: string): string[] | null {
  if (!raw.startsWith("{")) return [raw];
  const results = [...raw.matchAll(/[?:]\s*"([^"]*)"/g)].map((m) => m[1] ?? "");
  return results.length > 0 || /\bundefined\b/.test(raw) ? results : null;
}

describe("conjunto único de botões", () => {
  it("todo <button> é .nd-btn com uma variante do conjunto, ou um controle especializado da lista", () => {
    const offenders: string[] = [];
    let total = 0;
    for (const { file, text } of sources()) {
      for (const tag of buttonTags(text)) {
        total += 1;
        const className = attr(tag, "className") ?? "";
        const classes = new Set(className.match(/nd-[\w-]+/g) ?? []);
        if (classes.has("nd-btn")) {
          const variant = attr(tag, "data-variant");
          const literals = variant === null ? [] : variantValues(variant);
          if (literals === null) {
            offenders.push(`${file}: variante não literal ${variant ?? ""}`);
            continue;
          }
          for (const v of literals) if (!(VARIANTS as readonly string[]).includes(v)) offenders.push(`${file}: variante "${v}" fora do conjunto`);
          // Cor por tom só nos interruptores (no pino), nunca no botão inteiro.
          if (attr(tag, "data-tone") !== null && !classes.has("nd-toggle")) offenders.push(`${file}: .nd-btn com data-tone`);
          continue;
        }
        if (![...classes].some((c) => SPECIAL.has(c))) offenders.push(`${file}: ${className === "" ? "<sem classe>" : className}`);
      }
    }
    expect(total).toBeGreaterThan(100);
    expect(offenders).toEqual([]);
    for (const [, reason] of SPECIAL) expect(reason.length).toBeGreaterThan(10);
  });

  it("a checagem pega variante fora do conjunto, cor no botão e botão sem classe", () => {
    const tags = buttonTags('<button className="nd-btn" data-variant="success" onClick={() => a > b}>x</button><button type="button">y</button>');
    expect(tags).toHaveLength(2);
    expect(attr(tags[0] ?? "", "data-variant")).toBe("success");
    expect(attr(tags[0] ?? "", "onClick")).toBe("{() => a > b");
    expect(variantValues('{decision === "deny" ? "ghost" : undefined}')).toEqual(["ghost"]);
    expect(variantValues("{kind}")).toBeNull();
    expect(attr(tags[1] ?? "", "className")).toBeNull();
  });

  it("CSS: só as variantes do conjunto existem, cada uma com a cor da decisão 9", () => {
    const desktop = read("desktop/theme/desktop.css");
    const base = cssRuleBody(desktop, ".nd-btn");
    expect(base).toContain("background: var(--control);");
    expect(base).toContain("color: var(--ink);");
    expect(base).toContain("border: 0;");
    expect(cssRuleBody(desktop, '.nd-btn[data-variant="primary"]')).toMatch(/background: var\(--accent\);[\s\S]*color: var\(--accent-ink\);/);
    expect(cssRuleBody(desktop, '.nd-btn[data-variant="ghost"]')).toContain("background: transparent;");
    expect(cssRuleBody(desktop, '.nd-btn[data-variant="danger"]')).toMatch(/background: var\(--danger-soft\);[\s\S]*color: var\(--danger-ink\);/);
    const variants = new Set<string>();
    for (const { css } of allCss()) {
      for (const rule of cssRules(css)) for (const s of rule.selectors) for (const m of s.matchAll(/\.nd-btn\[data-variant="([^"]+)"\]/g)) variants.add(m[1] ?? "");
    }
    expect([...variants].sort()).toEqual([...VARIANTS].sort());
    // Nenhuma regra fora do desktop.css pinta o fundo ou a cor de um `.nd-btn` (só o desabilitado da primária).
    const painters: string[] = [];
    for (const { file, css } of allCss()) {
      for (const rule of cssRules(css)) {
        if (!rule.selectors.some((s) => /\.nd-btn\b/.test(s))) continue;
        if (file === "desktop/theme/desktop.css") continue;
        if (rule.selectors.every((s) => s === '.nd-root .nd-btn[data-variant="primary"]:disabled')) continue;
        if (/(?:^|;)\s*(?:background(?:-color)?|color):/.test(rule.body)) painters.push(`${file}: ${rule.selectors.join(", ")}`);
      }
    }
    expect(painters).toEqual([]);
  });

  it("Aprovação automática, Internet e Layout: botões neutros; ligado, o tom só no pino do interruptor", () => {
    const shell = read("desktop/theme/shell.css");
    const toggles = read("desktop/topbar/ProjectToggles.tsx");
    expect(toggles.match(/className="nd-btn nd-bar-btn nd-toggle"/g)).toHaveLength(2);
    expect(toggles).not.toMatch(/nd-chip/);
    expect(read("desktop/topbar/LayoutMenu.tsx")).toMatch(/className="nd-btn nd-bar-btn"\s+data-variant="ghost"/);
    expect(cssRuleBody(shell, '.nd-toggle[data-tone="amber"] .nd-switch[data-on="true"]::after')).toContain("background: var(--icon-amber);");
    expect(cssRuleBody(shell, '.nd-toggle[data-tone="violet"] .nd-switch[data-on="true"]::after')).toContain("background: var(--icon-violet);");
    // O botão inteiro nunca ganha o tom.
    for (const rule of cssRules(shell)) {
      if (!rule.selectors.some((s) => /^\.nd-toggle(\[[^\]]*\])*$/.test(s))) continue;
      expect(rule.body).not.toMatch(/background|color:/);
    }
    // O trilho desligado é neutro.
    expect(cssRuleBody(shell, ".nd-switch::after")).toContain("background: var(--muted);");
  });

  it("pílulas de estado: fundo neutro; o tom só no rótulo (e no ponto)", () => {
    const shell = read("desktop/theme/shell.css");
    const apps = read("apps/shared/apps.css");
    const settings = read("apps/settings/settings.css");
    expect(cssRuleBody(shell, ".nd-run-pill")).toContain("background: var(--control);");
    expect(cssRuleBody(apps, ".nd-pill")).toContain("background: var(--control);");
    expect(cssRuleBody(settings, ".nd-chip-static")).toContain("background: var(--control);");
    const toned = [
      ...cssRules(shell).filter((r) => r.selectors.some((s) => s.startsWith(".nd-run-pill[data-tone="))),
      ...cssRules(apps).filter((r) => r.selectors.some((s) => s.startsWith(".nd-pill[data-tone="))),
      ...cssRules(settings).filter((r) => r.selectors.some((s) => s.startsWith(".nd-chip-static[data-tone="))),
    ];
    expect(toned.length).toBeGreaterThanOrEqual(10);
    for (const rule of toned) expect({ s: rule.selectors.join(", "), body: rule.body }).toEqual({ s: rule.selectors.join(", "), body: expect.not.stringMatching(/background|border/) });
  });

  it("dock: ícones neutros (--ink aberta, --muted fechada), sem tom por aplicativo", () => {
    const desktop = read("desktop/theme/desktop.css");
    expect(cssRuleBody(desktop, ".nd-dock-btn")).toContain("color: var(--muted);");
    for (const state of ["open", "minimized", "focused"]) {
      const rule = cssRules(desktop).find((r) => r.selectors.includes(`.nd-dock-btn[data-state="${state}"]`) && /(?:^|;)\s*color:/.test(r.body));
      expect({ state, body: rule?.body }).toEqual({ state, body: expect.stringContaining("color: var(--ink);") });
    }
    for (const { file, css } of allCss()) expect({ file, tone: /\.nd-dock-btn\[data-tone/.test(css) }).toEqual({ file, tone: false });
    expect(read("desktop/Dock.tsx")).not.toMatch(/data-tone/);
    // O tom do aplicativo fica no ícone da barra de título.
    expect(read("desktop/Window.tsx")).toMatch(/className="nd-titlebar-icon" style=\{\{ color: `var\(--icon-\$\{meta\.tone\}\)` \}\}/);
  });
});

describe("contraste alto (prefers-contrast: more)", () => {
  it("os controles de preenchimento neutro sem borda ganham o contorno --edge-strong; a fantasma fica de fora", () => {
    const more = cssRules(read("desktop/theme/desktop.css")).filter((r) => r.at?.includes("prefers-contrast: more") === true);
    const outlined = more.filter((r) => r.body.includes("outline: 1px solid var(--edge-strong)") && r.body.includes("outline-offset: -1px")).flatMap((r) => r.selectors);
    const button = '.nd-root .nd-btn:not([data-variant="ghost"]):not([data-variant="primary"]):not(:focus-visible)';
    for (const s of [button, ".nd-root .nd-pill", ".nd-root .nd-chip-static", ".nd-root kbd", ".nd-root .nd-run-pill"]) expect(outlined).toContain(s);
    // Secundária (sem variante) e perigo entram; a fantasma não; com foco, o anel duplo continua.
    expect(button).not.toContain('"danger"');
    expect(outlined.filter((s) => s.includes(".nd-btn"))).toEqual([button]);
  });
});

/** Botões primários visíveis (fora de `hidden`/`inert`) dentro de `root`. */
function visiblePrimaries(root: Element): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('[data-variant="primary"]')].filter((el) => el.closest("[hidden], [inert]") === null);
}

/** Abre todas as janelas fechadas pelo dock. */
async function openAllWindows(): Promise<void> {
  for (const button of [...document.querySelectorAll<HTMLElement>('.nd-dock-btn[data-state="closed"]')]) {
    await act(async () => {
      fireEvent.click(button);
    });
  }
  await settle();
}

const P1 = makeProject("p1", "Loja");

const STATES: Readonly<Record<string, () => Record<string, ProjectState>>> = {
  "sem plano": () => ({ p1: makeProjectState({ id: "p1", name: "Loja" }) }),
  "plano aguardando aprovação": () => ({
    p1: makeProjectState({
      id: "p1",
      name: "Loja",
      specs: [
        {
          ...makeSpec("s1", "Loja online", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Carrinho", "TODO")]),
          plan_review: { status: "ok", issues: [{ task: "T01", kind: "untestable", text: "sem critério verificável" }] },
        },
      ],
    }),
  }),
  executando: () => ({
    p1: makeProjectState({
      id: "p1",
      name: "Loja",
      running: true,
      run: { spec_id: "s1", status: "running" },
      specs: [makeSpec("s1", "Carrinho", "IN_PROGRESS", [makeTask("T01", "a", "DONE"), makeTask("T02", "b", "IN_PROGRESS")])],
    }),
  }),
  concluído: () => ({ p1: makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec("s1", "Blog", "COMPLETED", [makeTask("T01", "a", "DONE")])] }) }),
};

describe("no máximo uma primária visível por janela", () => {
  it.each(Object.keys(STATES))("%s: todas as janelas abertas", async (name) => {
    const make = STATES[name];
    if (make === undefined) throw new Error("estado ausente");
    await renderShell({ url: "/?space=p1", projects: [P1], states: make() });
    await openAllWindows();
    const windows = [...document.querySelectorAll<HTMLElement>("[data-window]")].filter((w) => !w.hidden);
    expect(windows.length).toBeGreaterThanOrEqual(10);
    const counts = windows.map((w) => ({ window: w.getAttribute("data-window"), primaries: visiblePrimaries(w).length }));
    expect(counts.filter((c) => c.primaries > 1)).toEqual([]);
    // A barra superior não tem primária (só controles neutros).
    const bar = document.querySelector("[data-topbar]");
    expect(bar === null ? 0 : visiblePrimaries(bar).length).toBe(0);
  });

  it("095/T07 A-10: concluído tem só duas primárias no desktop todo, uma no App e uma na Execução; o Relatório nenhuma", async () => {
    const make = STATES["concluído"];
    if (make === undefined) throw new Error("estado ausente");
    await renderShell({ url: "/?space=p1", projects: [P1], states: make() });
    await openAllWindows();
    // As janelas do preset Concluído (Relatório, App, Conversa e Execução) e a barra superior.
    const preset = new Set<string>(presetWindows("done"));
    const all = visiblePrimaries(document.body).filter((el) => {
      const kind = el.closest("[data-window]")?.getAttribute("data-window");
      return kind === undefined || kind === null || preset.has(kind);
    });
    const where = all.map((el) => el.closest("[data-window]")?.getAttribute("data-window") ?? "fora de janela").sort();
    expect(where).toEqual(["app", "execution"]);
    const report = document.querySelector<HTMLElement>('[data-window="report"]');
    expect(report === null ? -1 : visiblePrimaries(report).length).toBe(0);
  });

  it("plano aguardando aprovação: a primária do Plano é a aprovação; 'Pedir correção' é secundária", async () => {
    const make = STATES["plano aguardando aprovação"];
    if (make === undefined) throw new Error("estado ausente");
    await renderShell({ url: "/?space=p1", projects: [P1], states: make() });
    await openAllWindows();
    const plan = document.querySelector<HTMLElement>('[data-window="plan"]');
    if (plan === null) throw new Error("Plano ausente");
    const primaries = visiblePrimaries(plan);
    expect(primaries).toHaveLength(1);
    expect(primaries[0]?.hasAttribute("data-approve")).toBe(true);
    const fix = plan.querySelector<HTMLElement>("[data-plan-fix]");
    expect(fix).not.toBeNull();
    expect(fix?.getAttribute("data-variant")).toBeNull();
  });
});
