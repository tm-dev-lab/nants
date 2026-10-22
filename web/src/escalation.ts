/** Parser puro e tolerante do bloco "Contexto" anexado pelo servidor à análise de escalada (spec 071). */
export interface EscalationContext {
  /** Texto antes do bloco "Contexto" (causa raiz, decisão). */
  summary: string;
  rootCause: string | null;
  command: string | null;
  code: string | null;
  excerpt: string | null;
  attempted: string | null;
  whyStopped: string | null;
  options: string[];
}

const CONTEXT_RE = /^[ \t]*-[ \t]*Contexto:[ \t]*$/m;
const FIELD_RE = /^ {1,4}-[ \t]*([^:\n]+?):[ \t]*(.*)$/;

/** Linhas de metadados do servidor ("- Contagem: …", "- Rodada autônoma…"): nunca fazem parte de um campo. */
const META_RE = /^\s*-\s*(contagem:|rodada autonoma\b|correcao de ambiente\b)/;

function norm(label: string): string {
  return label.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase().trim();
}

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

/** `null` quando a análise não traz o bloco "Contexto" (dados antigos). */
export function parseEscalation(text: string): EscalationContext | null {
  const m = CONTEXT_RE.exec(text);
  if (m === null) return null;
  const head = text.slice(0, m.index);
  const body = text.slice(m.index + m[0].length).split("\n");
  const ctx: EscalationContext = {
    summary: head.trim(),
    rootCause: null,
    command: null,
    code: null,
    excerpt: null,
    attempted: null,
    whyStopped: null,
    options: [],
  };
  const headLines = head.split("\n");
  const rcIdx = headLines.findIndex((l) => /^[ \t]*-?[ \t]*Causa raiz:/i.test(l));
  if (rcIdx >= 0) {
    const first = (headLines[rcIdx] ?? "").replace(/^[ \t]*-?[ \t]*Causa raiz:[ \t]*/i, "");
    const parts = [first.trim()];
    for (let k = rcIdx + 1; k < headLines.length; k += 1) {
      const l = headLines[k] ?? "";
      if (/^-\s/.test(l)) break;
      parts.push(l.trim());
    }
    ctx.rootCause = parts.join("\n").trim();
  }

  const known = new Set(["comando", "codigo", "trecho do erro", "o que foi tentado", "por que nao resolveu", "opcoes"]);
  let current: string | null = null;
  const values = new Map<string, string[]>();
  for (const line of body) {
    if (META_RE.test(norm(line))) {
      current = null;
      continue;
    }
    const f = FIELD_RE.exec(line);
    const key = f === null ? null : norm(f[1] ?? "");
    if (f !== null && key !== null && known.has(key) && !/^ {5,}/.test(line)) {
      current = key;
      values.set(key, (f[2] ?? "").trim() === "" ? [] : [(f[2] ?? "").trim()]);
    } else if (current !== null) {
      const keep = current === "trecho do erro" ? line.replace(/^ {1,6}/, "") : line.trim();
      values.get(current)?.push(keep);
    }
  }
  const val = (k: string): string | null => {
    const v = values.get(k);
    if (v === undefined) return null;
    const j = v.join("\n").replace(/\s+$/, "");
    return j === "" ? null : j;
  };
  ctx.command = val("comando");
  ctx.code = val("codigo");
  ctx.excerpt = val("trecho do erro");
  ctx.attempted = val("o que foi tentado");
  ctx.whyStopped = val("por que nao resolveu");
  ctx.options = parseOptions(val("opcoes") ?? "");
  return ctx;
}
