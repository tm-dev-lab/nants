// @vitest-environment node
/*
 * Higiene permanente do código de `web/src` (spec 093, decisão 18 e G8):
 * 1. Uma interface só. Nenhum texto, nome ou comentário trata de outra versão da interface:
 *    - `V1`/`V2` em qualquer texto (fora dos dados de path de SVG);
 *    - "interface/tela/layout/visual antiga|anterior|legada|nova", "nova interface" e
 *      "duplicação temporária" em qualquer texto;
 *    - em comentários, o histórico de onde o código veio: "porte de", "portado de", "cópia
 *      convertida", "cópia das regras", "cópia de `X`", "igual ao antigo", "no lugar do antigo",
 *      "antigo `X`" e as referências ao "Mapa" da spec;
 *    - os nomes dos componentes e módulos que não existem mais;
 *    - rotas de cliente (`/projects/...`, `/settings`) em literais passados a
 *      `navigate`/`pushState`/`replaceState` (o URL só tem `/?space=<id>`).
 * 2. Nenhum export sem import: todo export de um módulo de `web/src` é importado por outro arquivo
 *    (código ou teste), inclusive a reexportação (`export { a } from "./b"`), que conta como export
 *    do módulo que reexporta e precisa de um consumidor dele. Exceção: o ponto de entrada (`main.tsx`).
 * 3. `noUnusedLocals` e `noUnusedParameters` ligados no `tsconfig.json`: o `tsc --noEmit` do
 *    `npm run build` falha com import, variável ou parâmetro sem uso.
 *
 * A varredura usa a AST (parser do Rolldown, que já vem com o Vite) para separar texto, comentário
 * e import/export. Isenções: só este arquivo (que contém os padrões); `.sdd/**` fica fora de `web/`.
 * Calibragem: "antigo" sozinho é legítimo quando fala de DADOS (mensagens mais antigas, servidor
 * antigo, snapshot antigo, formato antigo) e por isso só conta perto de uma referência de código
 * ou de "igual ao"/"no lugar do"; "anterior" (versão anterior do plano, GET anterior) também.
 */
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { parseAst } from "rolldown/parseAst";
import { describe, expect, it } from "vitest";

const WEB = process.cwd();
const SRC = resolve(WEB, "src");
const SELF = "codebase.test.ts";
/** Pontos de entrada: ninguém os importa e eles não precisam exportar nada. */
const ENTRY_POINTS: ReadonlySet<string> = new Set(["main.tsx"]);

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.(ts|tsx|css)$/.test(name)) out.push(full);
  }
  return out;
}

const rel = (full: string): string => relative(SRC, full).split(sep).join("/");
const isTest = (file: string): boolean => /\.test\.tsx?$/.test(file);

type Node = Record<string, unknown> & { type?: string; start?: number; end?: number };

function parse(file: string, source: string): Node {
  return parseAst(source, { lang: file.endsWith(".tsx") ? "tsx" : "ts" }) as unknown as Node;
}

function visit(node: unknown, fn: (n: Node) => void): void {
  if (Array.isArray(node)) {
    for (const child of node) visit(child, fn);
    return;
  }
  if (typeof node !== "object" || node === null) return;
  const n = node as Node;
  if (typeof n.type === "string") fn(n);
  for (const [key, value] of Object.entries(n)) {
    if (key !== "start" && key !== "end" && key !== "type") visit(value, fn);
  }
}

// --- 1. Uma interface só ---

/** Dados de path de SVG (`"M4 12h16"`): podem ter `V1`, `V2` sem ser texto. */
const SVG_PATH = /^\s*[Mm][\d\s.,eMmZzLlHhVvCcSsQqTtAa-]*$/;

interface SourceTexts {
  /** Textos do código: literais, templates e texto JSX (sem os dados de path de SVG). */
  strings: string[];
  /** Comentários (`//` e `/* *\/`). */
  comments: string[];
  /** O arquivo sem os literais (para os nomes, que valem também em identificadores e imports). */
  code: string;
}

/** Separa texto, comentário e código. CSS: tudo é código, com os comentários à parte. */
function sourceTexts(file: string, source: string): SourceTexts {
  if (file.endsWith(".css")) {
    const comments = source.match(/\/\*[\s\S]*?\*\//g) ?? [];
    return { strings: [], comments, code: source.replace(/\/\*[\s\S]*?\*\//g, " ") };
  }
  const strings: string[] = [];
  const spans: Array<[number, number]> = [];
  visit(parse(file, source), (n) => {
    const literal =
      (n.type === "Literal" && (typeof n.value === "string" || n.regex !== undefined)) ||
      n.type === "TemplateElement" ||
      n.type === "JSXText";
    if (!literal || typeof n.start !== "number" || typeof n.end !== "number") return;
    spans.push([n.start, n.end]);
    if (n.type === "Literal" && typeof n.value === "string") {
      if (!SVG_PATH.test(n.value)) strings.push(n.value);
    } else if (n.type === "TemplateElement") {
      const value = n.value as { cooked?: string | null; raw?: string } | undefined;
      strings.push(value?.cooked ?? value?.raw ?? "");
    } else if (n.type === "JSXText") {
      strings.push(String(n.value));
    }
  });
  // Sem os literais, o que sobra de `//` e `/* */` é comentário.
  let blank = source;
  for (const [start, end] of spans) blank = blank.slice(0, start) + " ".repeat(end - start) + blank.slice(end);
  const comments = blank.match(/\/\*[\s\S]*?\*\/|\/\/[^\n]*/g) ?? [];
  let code = blank;
  for (const comment of comments) code = code.replace(comment, " ");
  // Os literais de módulo (imports) contam como código: um import de componente removido é rastro.
  visit(parse(file, source), (n) => {
    if ((n.type === "ImportDeclaration" || n.type === "ExportNamedDeclaration" || n.type === "ExportAllDeclaration") && n.source) {
      code += `\n${String((n.source as Node).value)}`;
    }
  });
  return { strings, comments, code };
}

/** Versão da interface em qualquer texto (strings e comentários). */
const VERSION = /\bV[12]\b/;
/** Outra interface, em qualquer texto. */
const OTHER_UI =
  /\b(?:interface|tela|telas|layout|visual) (?:antig[oa]s?|anterior(?:es)?|legad[oa]s?|nova)\b|\bnova interface\b|\bduplicação temporária\b/i;
/** Histórico em comentário. */
const HISTORY =
  /\bporte (?:de|do|da|dos|das)\b|\bportad[oa]s? (?:de|do|da)\b|\bc[óo]pia (?:convertida|das regras|da carga)\b|\bcopiad[oa]s? d[oa] `|\b(?:igual|como) (?:a[oà]?|n[oa]) antig|\bno lugar d[oa] antig|\bantig[oa]s? `|`[^`\n]+` antig/i;
/** Referência ao "Mapa" da spec (de onde o código veio), em comentário. */
const MAP_REF = /\bMapa(?::| da T\d+)/;
/** Componentes e módulos que não existem (nomes do G8 e os demais removidos). */
const REMOVED_NAMES =
  /\b(?:Sidebar|ProjectView|PlanningView|ExecutionView|WindowsLayout|WindowFrame|BlockStack|BlockRoots|ActionRequiredModal|ConversationPanel|KanbanBoard|FileExplorer|SettingsView|ConnectionsGrid|ConnectionModal|AgentsPanel|PricingPanel|PrereqsPanel|LimitsModal|YoloModal|InternetSwitch|RemoveProjectModal|NewProjectModal|RunPanel|StopRunButton|UsageIndicator|ToolActionsDialog|ModalShell|useUnseen|viewState|windowLayout|useWindowDrag|useWindowModes|useWindowOrder|useResizableWidth|useResizableHeight|useTaskUsage|LiveStream)\b|ui-reference|\bcomponents\//;
/**
 * Conceitos da interface removida, sem diferenciar maiúsculas: barra lateral, luz ambiente,
 * kanban (a tela de tarefas é o Quadro), acordeão, tela/cabeçalho do projeto e a faixa
 * de concluído. Isento: `staleStorage.ts` e o teste dele (as chaves que a limpeza apaga).
 */
const REMOVED_CONCEPTS =
  /\bside-?bar\b|\bbarra lateral\b|\bluz ambiente\b|\bambient[- ]?(?:glow|light)\b|\bkanban\b|\bacorde[ãa]o\b|\btela d[oe] projeto\b|\bcabeçalho do projeto\b|\bfaixa de (?:projeto )?concluído\b|\bglass-surface\b|\bblock-?stack\b/i;
const CONCEPT_EXEMPT: ReadonlySet<string> = new Set(["staleStorage.ts", "staleStorage.test.ts"]);
/** Rota de cliente em literal de navegação. */
const OLD_ROUTE = /\b(?:navigate|pushState|replaceState)\s*\([^)]*?["'`]\/(?:projects|settings)\b/;

interface TraceIssue {
  file: string;
  problem: string;
}

/** Rastros de outra interface num arquivo (usado também pelos exemplos abaixo). */
function traceIssues(file: string, source: string): TraceIssue[] {
  const issues: TraceIssue[] = [];
  const { strings, comments, code } = sourceTexts(file, source);
  const add = (problem: string, sample: string): void => {
    issues.push({ file, problem: `${problem}: "${sample.trim().slice(0, 80)}"` });
  };
  for (const text of [...strings, ...comments]) {
    if (VERSION.test(text)) add("versão da interface", text);
    if (OTHER_UI.test(text)) add("outra interface", text);
  }
  for (const comment of comments) {
    if (HISTORY.test(comment) || MAP_REF.test(comment)) add("histórico em comentário", comment);
  }
  for (const text of [...strings, ...comments, code]) {
    const name = REMOVED_NAMES.exec(text);
    if (name !== null) add(`nome removido "${name[0]}"`, text.slice(Math.max(0, name.index - 30), name.index + 40));
  }
  if (!CONCEPT_EXEMPT.has(file)) {
    for (const text of [...strings, ...comments, code]) {
      const concept = REMOVED_CONCEPTS.exec(text);
      if (concept !== null) add(`conceito removido "${concept[0]}"`, text.slice(Math.max(0, concept.index - 30), concept.index + 40));
    }
  }
  if (OLD_ROUTE.test(source)) add("rota de cliente", OLD_ROUTE.exec(source)?.[0] ?? "");
  return issues;
}

describe("uma interface só (G8)", () => {
  it("pega versão, outra interface, histórico, nome removido e rota; ignora path de SVG e texto legítimo", () => {
    const bad = [
      'const a = "Nova V2";',
      "// layout da V1\nconst b = 1;",
      'const c = <p>Como na interface antiga</p>;',
      "/* cópia convertida do painel */\nconst d = 1;",
      "// porte do arquivo de viewport\nconst e = 1;",
      "// igual ao antigo\nconst f = 1;",
      "// selo (no lugar do antigo `useUnseen`)\nconst f2 = 1;",
      "/* Janela Quadro (Mapa: `Kanban`) */\nconst f3 = 1;",
      'const f4 = "na interface nova";',
      'import { Sidebar } from "./Sidebar";',
      "const g = <ExecutionView />;",
      'window.history.pushState({}, "", "/projects/p1");',
      'navigate("/settings");',
      ".a { color: var(--ink); } /* layout legado */",
      "// aviso da sidebar\nconst h = 1;",
      'const i = "Recolher a barra lateral";',
      "/* luz ambiente esmeralda */\nconst j = 1;",
      'const k = <div className="ambient-glow" />;',
      "// selo do kanban\nconst l = 1;",
      'const m = { title: "Kanban" };',
      "// atualiza a tela do projeto\nconst n = 1;",
      "// faixa de projeto concluído\nconst o = 1;",
    ];
    for (const source of bad) {
      const file = source.startsWith(".a") ? "x.css" : "x.tsx";
      expect({ source, found: traceIssues(file, source).length > 0 }).toEqual({ source, found: true });
    }
    const good = [
      'const p = <path d="M4 12V2h16V1z" />;',
      'const q = { d: "M2 2 V1 L4 4" };',
      'it("GET antigo nunca sobrescreve o estado novo", () => {});',
      "// resposta anterior ao POST é descartada\nconst r = 1;",
      'window.history.replaceState({}, "", "/?space=p1");',
      'const s = "Plano anterior aprovado";',
      "// as mensagens mais antigas saem primeiro\nconst t = 1;",
      "/** Ausente em servidores antigos: a UI trata como `chat`. */\nconst u = 1;",
      "/** Cópia de `data` com a seção trocada. */\nconst v = 1;",
      'const w = <h2>Colunas do quadro</h2>;',
      "// cartões de tarefas nas colunas do Quadro\nconst x = 1;",
      "// a tela cheia do celular\nconst y = 1;",
      "// efeito de glow no foco do botão\nconst g1 = 1;",
      'const g2 = "Abrir a gaveta de ferramentas";',
    ];
    for (const source of good) expect({ source, issues: traceIssues("x.tsx", source) }).toEqual({ source, issues: [] });
  });

  it("nenhum arquivo de web/src tem rastro de outra interface", () => {
    const issues: string[] = [];
    for (const full of walk(SRC)) {
      const file = rel(full);
      if (file === SELF) continue;
      for (const issue of traceIssues(file, readFileSync(full, "utf8"))) issues.push(`${issue.file} ${issue.problem}`);
    }
    expect(issues).toEqual([]);
  });

  it("index.html tem só o título da marca e o viewport com interactive-widget", () => {
    const html = readFileSync(resolve(WEB, "index.html"), "utf8");
    expect(html).toContain("<title>Nants</title>");
    expect(html).toMatch(/<meta name="viewport" content="[^"]*interactive-widget=resizes-content/);
    expect(VERSION.test(html) || OTHER_UI.test(html)).toBe(false);
  });
});

// --- 2. Nenhum export sem import ---

const MODULE_EXT = ["", ".ts", ".tsx", "/index.ts", "/index.tsx"];

function resolveModule(from: string, spec: string): string | null {
  if (!spec.startsWith(".")) return null;
  const base = resolve(dirname(from), spec);
  for (const ext of MODULE_EXT) {
    const candidate = base + ext;
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  return null;
}

function nameOf(node: unknown): string | null {
  const n = node as Node | null;
  if (n === null || typeof n !== "object") return null;
  if (n.type === "Identifier") return String(n.name);
  if (n.type === "Literal") return String(n.value);
  return null;
}

/**
 * Nomes exportados por um módulo: declarações, `export { a, b as c }` e reexportações
 * (`export { a } from "./b"`), que também precisam de quem as importe deste módulo.
 */
function exportedNames(file: string, source: string): string[] {
  const names: string[] = [];
  for (const statement of (parse(file, source).body as Node[]) ?? []) {
    if (statement.type === "ExportDefaultDeclaration") names.push("default");
    if (statement.type !== "ExportNamedDeclaration") continue;
    const declaration = statement.declaration as Node | null;
    if (declaration) {
      if (declaration.type === "VariableDeclaration") {
        for (const d of (declaration.declarations as Node[]) ?? []) {
          const id = nameOf(d.id);
          if (id !== null) names.push(id);
        }
      } else {
        const id = nameOf(declaration.id);
        if (id !== null) names.push(id);
      }
    }
    for (const specifier of (statement.specifiers as Node[]) ?? []) {
      const exported = nameOf(specifier.exported);
      if (exported !== null) names.push(exported);
    }
  }
  return names;
}

/** Usos de exports: `módulo → nomes importados` (`*` = o módulo inteiro). */
function importsOf(full: string, source: string, into: Map<string, Set<string>>): void {
  const use = (spec: unknown, name: string): void => {
    const target = typeof spec === "string" ? resolveModule(full, spec) : null;
    if (target === null) return;
    const set = into.get(target) ?? new Set<string>();
    set.add(name);
    into.set(target, set);
  };
  /** `import("x")` (direto ou com `await`) do inicializador, ou `null`. */
  const dynamicImport = (init: unknown): Node | null => {
    const n = init as Node | null;
    if (n?.type === "ImportExpression") return n;
    if (n?.type === "AwaitExpression" && (n.argument as Node | null)?.type === "ImportExpression") return n.argument as Node;
    return null;
  };
  const handled = new Set<Node>();
  /** `const m = await import("x")`: os usos são os `m.<nome>` do arquivo. */
  const aliases = new Map<string, unknown>();
  const program = parse(full, source);
  visit(program, (n) => {
    if (n.type === "VariableDeclarator") {
      const expression = dynamicImport(n.init);
      const id = n.id as Node;
      if (expression === null) return;
      if (id.type === "ObjectPattern") {
        handled.add(expression);
        for (const property of (id.properties as Node[]) ?? []) use((expression.source as Node).value, nameOf(property.key) ?? "*");
      } else if (id.type === "Identifier") {
        handled.add(expression);
        aliases.set(String(id.name), (expression.source as Node).value);
      }
    } else if (n.type === "ImportDeclaration") {
      const spec = (n.source as Node).value;
      const specifiers = (n.specifiers as Node[]) ?? [];
      if (specifiers.length === 0) use(spec, "*side-effect");
      for (const s of specifiers) {
        if (s.type === "ImportDefaultSpecifier") use(spec, "default");
        else if (s.type === "ImportNamespaceSpecifier") use(spec, "*");
        else use(spec, nameOf(s.imported) ?? "*");
      }
    } else if (n.type === "ExportNamedDeclaration" && n.source) {
      for (const s of (n.specifiers as Node[]) ?? []) use((n.source as Node).value, nameOf(s.local) ?? "*");
    } else if (n.type === "ExportAllDeclaration") {
      use((n.source as Node).value, "*");
    } else if (n.type === "ImportExpression" && !handled.has(n)) {
      // `import("./x").then(...)` (carga sob demanda): o módulo inteiro.
      use((n.source as Node).value, "*");
    }
  });
  if (aliases.size === 0) return;
  visit(program, (n) => {
    if (n.type !== "MemberExpression") return;
    const object = n.object as Node;
    if (object.type !== "Identifier" || !aliases.has(String(object.name))) return;
    use(aliases.get(String(object.name)), nameOf(n.property) ?? "*");
  });
}

describe("nenhum export sem import", () => {
  it("lê exports e imports pela AST", () => {
    expect(exportedNames("x.ts", "export const a = 1, b = 2; export function c() {} export type D = 1; export interface E {}; const f = 1; export { f as g };")).toEqual([
      "a",
      "b",
      "c",
      "D",
      "E",
      "g",
    ]);
    // Reexportação conta como export de quem reexporta (o tipo também).
    expect(exportedNames("x.ts", 'export { a, b as c } from "./y"; export type { T } from "./z";')).toEqual(["a", "c", "T"]);
    const used = new Map<string, Set<string>>();
    const here = resolve(SRC, "codebase.test.ts");
    importsOf(here, 'const { a } = await import("./x"); const m = await import("./y"); m.b(); import("./z");', used);
    // Módulos inexistentes não resolvem: nada é contado.
    expect(used.size).toBe(0);
  });

  it("todo export de web/src é importado por outro arquivo (exceto o ponto de entrada)", () => {
    const files = walk(SRC).filter((f) => /\.tsx?$/.test(f));
    const used = new Map<string, Set<string>>();
    const sources = new Map<string, string>();
    for (const full of files) {
      const source = readFileSync(full, "utf8");
      sources.set(full, source);
      importsOf(full, source, used);
    }
    const orphans: string[] = [];
    for (const full of files) {
      const file = rel(full);
      if (isTest(file) || ENTRY_POINTS.has(file)) continue;
      const names = used.get(full) ?? new Set<string>();
      if (names.has("*")) continue;
      for (const name of exportedNames(file, sources.get(full) ?? "")) {
        if (!names.has(name)) orphans.push(`${file}: ${name}`);
      }
    }
    expect(orphans).toEqual([]);
  });

});

// --- 3. tsc sem local nem parâmetro sem uso ---

describe("tsc sem import, local ou parâmetro sem uso", () => {
  it("tsconfig liga noUnusedLocals e noUnusedParameters (o build roda tsc --noEmit)", () => {
    const config = JSON.parse(readFileSync(resolve(WEB, "tsconfig.json"), "utf8")) as { compilerOptions?: Record<string, unknown> };
    expect(config.compilerOptions?.noUnusedLocals).toBe(true);
    expect(config.compilerOptions?.noUnusedParameters).toBe(true);
    const pkg = JSON.parse(readFileSync(resolve(WEB, "package.json"), "utf8")) as { scripts?: Record<string, string> };
    expect(pkg.scripts?.build).toMatch(/^tsc --noEmit && /);
  });
});
