/*
 * Parser puro e tolerante do bloco "Contexto" anexado pelo servidor à análise de escalada (spec 071).
 *
 * Spec 142 T07: os rótulos e as linhas fixas do servidor são reconhecidos pelas formas do
 * dicionário em todos os idiomas (`recoveryText.*`), como em `recoveryAnalysis.ts`; os valores
 * fixos (opções, contagem, causa, código de saída, "sem saída") saem no idioma `lang`. O que o
 * modelo ou o comando escreveram fica como veio.
 */
import { DICTIONARIES, type Lang, type MessageKey } from "./i18n";
import { localizeAttempted, localizeEscalationOptions, localizeExitCode, localizeFixedLine, localizeRecoveryCause } from "./recoveryAnalysis";

export interface EscalationContext {
  /** Texto antes do bloco "Contexto" (causa raiz, decisão). */
  summary: string;
  rootCause: string | null;
  /** Pergunta objetiva do Arquiteto numa decisão ASK (spec 124 T03). */
  question: string | null;
  command: string | null;
  code: string | null;
  excerpt: string | null;
  attempted: string | null;
  whyStopped: string | null;
  options: string[];
}

/** Formas da chave em todos os idiomas do dicionário, sem repetição. */
function forms(key: MessageKey): string[] {
  const out: string[] = [];
  for (const dictionary of Object.values(DICTIONARIES)) {
    const form = dictionary[key];
    if (!out.includes(form)) out.push(form);
  }
  return out;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function norm(label: string): string {
  return label.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase().trim();
}

/** Rótulo de uma linha fixa `- rótulo: ...` (ou `- rótulo:`): o texto entre o traço e os dois pontos. */
function labelOf(form: string): string {
  const body = form.replace(/^\s*-\s*/, "");
  const at = body.indexOf(":");
  return (at < 0 ? body : body.slice(0, at)).trim();
}

/** Começo de uma linha fixa até o primeiro `(` ou `#` (`- Rodada autônoma (…) #{n}` vira `rodada autonoma`). */
function startOf(form: string): string {
  const body = form.replace(/^\s*-\s*/, "");
  const at = body.search(/[(#]/);
  return norm(at < 0 ? body : body.slice(0, at));
}

function alternatives(labels: readonly string[]): string {
  return labels.map(escapeRegExp).join("|");
}

const CONTEXT_RE = new RegExp(`^[ \\t]*-[ \\t]*(?:${alternatives(forms("recoveryText.context").map(labelOf))}):[ \\t]*$`, "m");
const ROOT_CAUSE_RE = new RegExp(`^[ \\t]*-?[ \\t]*(?:${alternatives(forms("recoveryText.rootCause"))}):[ \\t]*`, "i");
const QUESTION_RE = new RegExp(`^[ \\t]*-[ \\t]*(?:${alternatives(forms("recoveryText.question"))}):[ \\t]*`);
const FIELD_RE = /^ {1,4}-[ \t]*([^:\n]+?):[ \t]*(.*)$/;

/** Linhas de metadados do servidor (contagem, rodada autônoma, correção de ambiente): nunca fazem parte de um campo. */
const META_STARTS: readonly string[] = [
  ...forms("recoveryText.notCounted").map((f) => `${norm(labelOf(f))}:`),
  ...forms("recoveryText.autonomousRound").map(startOf),
  ...forms("recoveryText.envFix").map(startOf),
];

function isMeta(line: string): boolean {
  const body = norm(line).replace(/^-\s*/, "");
  return line.trimStart().startsWith("-") && META_STARTS.some((start) => start !== "" && body.startsWith(start));
}

type Field = "command" | "code" | "excerpt" | "attempted" | "whyStopped" | "options";

/** Rótulo normalizado de cada campo, em todos os idiomas. */
const FIELDS: ReadonlyMap<string, Field> = new Map(
  (
    [
      ["command", forms("recoveryText.command")],
      ["code", forms("recoveryText.exitCode")],
      ["excerpt", forms("recoveryText.excerpt").map(labelOf)],
      ["attempted", forms("recoveryText.attempted").map(labelOf)],
      ["whyStopped", forms("recoveryText.whyNot")],
      ["options", forms("recoveryText.options.proposed").map(labelOf)],
    ] as const
  ).flatMap(([field, labels]) => labels.map((label): [string, Field] => [norm(label), field])),
);

/** Separa "(1) a; (2) b; (3) c" em itens; sem marcadores devolve o texto inteiro como um item. */
export function parseOptions(text: string): string[] {
  const t = text.trim();
  if (t === "") return [];
  const parts = t
    .split(/(?:^|[;\s])\(\d+\)\s*/)
    .map((p) => p.replace(/;\s*$/, "").trim())
    .filter((p) => p !== "");
  return parts.length > 0 ? parts : [t];
}

/** Bloco que começa em `headLines[idx]` (sem o rótulo `re`) até a próxima linha `- `. */
function block(headLines: readonly string[], idx: number, re: RegExp): string {
  const parts = [(headLines[idx] ?? "").replace(re, "").trim()];
  for (let k = idx + 1; k < headLines.length; k += 1) {
    const l = headLines[k] ?? "";
    if (/^-\s/.test(l)) break;
    parts.push(l.trim());
  }
  return parts.join("\n").trim();
}

/** `null` quando a análise não traz o bloco "Contexto" (dados antigos). Valores fixos do servidor em `lang`. */
export function parseEscalation(text: string, lang: Lang): EscalationContext | null {
  const m = CONTEXT_RE.exec(text);
  if (m === null) return null;
  const head = text.slice(0, m.index);
  const body = text.slice(m.index + m[0].length).split("\n");
  const ctx: EscalationContext = {
    summary: head.trim(),
    rootCause: null,
    question: null,
    command: null,
    code: null,
    excerpt: null,
    attempted: null,
    whyStopped: null,
    options: [],
  };
  const headLines = head.split("\n");
  const rcIdx = headLines.findIndex((l) => ROOT_CAUSE_RE.test(l));
  if (rcIdx >= 0) ctx.rootCause = block(headLines, rcIdx, ROOT_CAUSE_RE);
  const qIdx = headLines.findIndex((l) => QUESTION_RE.test(l));
  if (qIdx >= 0) {
    const q = block(headLines, qIdx, QUESTION_RE);
    ctx.question = q === "" ? null : q;
  }

  let current: Field | null = null;
  const values = new Map<Field, string[]>();
  for (const line of body) {
    if (isMeta(line)) {
      current = null;
      continue;
    }
    const f = FIELD_RE.exec(line);
    const field = f === null ? undefined : FIELDS.get(norm(f[1] ?? ""));
    if (f !== null && field !== undefined && !/^ {5,}/.test(line)) {
      current = field;
      values.set(field, (f[2] ?? "").trim() === "" ? [] : [(f[2] ?? "").trim()]);
    } else if (current !== null) {
      const keep = current === "excerpt" ? line.replace(/^ {1,6}/, "") : line.trim();
      values.get(current)?.push(keep);
    }
  }
  const val = (k: Field): string | null => {
    const v = values.get(k);
    if (v === undefined) return null;
    const j = v.join("\n").replace(/\s+$/, "");
    return j === "" ? null : j;
  };
  const command = val("command");
  const code = val("code");
  const excerpt = val("excerpt");
  const attempted = val("attempted");
  const whyStopped = val("whyStopped");
  ctx.command = command;
  ctx.code = code === null ? null : localizeExitCode(code, lang);
  ctx.excerpt = excerpt === null ? null : localizeFixedLine(excerpt, lang);
  ctx.attempted = attempted === null ? null : localizeAttempted(attempted, lang);
  ctx.whyStopped = whyStopped === null ? null : localizeRecoveryCause(whyStopped, lang);
  ctx.options = parseOptions(localizeEscalationOptions(val("options") ?? "", lang));
  return ctx;
}
