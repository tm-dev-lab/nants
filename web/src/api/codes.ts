/*
 * `code` e `params` das mensagens do servidor (spec 102): validação do formato, sem dependências
 * (o cliente da API e o fluxo de eventos usam; a tradução fica em `i18n/serverCodes`).
 */

/** Parâmetros de um `code`: textos e números. */
export type CodeParams = Readonly<Record<string, string | number>>;

/** `code` estável: `[a-z0-9_.]`, de 1 a 64 caracteres, começando por letra (igual ao servidor). */
const CODE_RE = /^[a-z][a-z0-9_.]{0,63}$/;
/** Nome de parâmetro: `[a-z0-9_]`, até 32 caracteres. */
const PARAM_NAME_RE = /^[a-z0-9_]{1,32}$/;
/** Teto de parâmetros e de cada texto (o servidor já corta em 512 bytes). */
const MAX_PARAMS = 8;
const MAX_PARAM_CHARS = 512;

/** Texto com `code` e `params` neutros de idioma (rótulo da atividade, etapa). */
export interface Coded {
  code: string;
  params: CodeParams;
}

/** `code` no formato fixo com os `params` válidos (vazios quando ausentes), ou `undefined`. */
export function parseCoded(code: unknown, params: unknown): Coded | undefined {
  if (!isMessageCode(code)) return undefined;
  return { code, params: parseCodeParams(params) ?? {} };
}

export function isMessageCode(value: unknown): value is string {
  return typeof value === "string" && CODE_RE.test(value);
}

/**
 * `params` válidos ou `null`: objeto com nomes no formato fixo e valores texto ou número finito.
 * Valores de outro tipo são descartados (o resto continua valendo).
 */
export function parseCodeParams(value: unknown): CodeParams | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const out: Record<string, string | number> = {};
  let count = 0;
  for (const [name, v] of Object.entries(value as Record<string, unknown>)) {
    if (count >= MAX_PARAMS) break;
    if (!PARAM_NAME_RE.test(name)) continue;
    if (typeof v === "string") out[name] = v.slice(0, MAX_PARAM_CHARS);
    else if (typeof v === "number" && Number.isFinite(v)) out[name] = v;
    else continue;
    count += 1;
  }
  return out;
}
