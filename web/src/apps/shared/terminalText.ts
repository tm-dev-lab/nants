/*
 * Saída de terminal como texto limpo (spec 113, T03). Os processos do app (Vite, npm, compose)
 * escrevem sequências ANSI: cores (SGR), movimentos de cursor, limpeza de linha, títulos (OSC).
 * O navegador desenha o ESC (0x1b) como um glifo visível, e o registro aparecia como
 * "Ø[32mVITE...". Aqui as sequências saem e o efeito delas na linha é aplicado:
 * - SGR (cores, negrito, esmaecido, 256 cores, 24 bits) e modos privados (`?25l`): removidos;
 * - `\r` volta ao início da linha e o texto seguinte sobrescreve (barras de progresso);
 * - `ESC[K` (0 até o fim, 1 até o início, 2 a linha toda), `ESC[nG`, `ESC[nC`, `ESC[nD`, `\b`:
 *   aplicados sobre a linha atual (o avanço do cursor cria no máximo `MAX_PAD` espaços por
 *   linha); movimentos verticais e limpeza de tela são ignorados;
 * - OSC (`ESC]...BEL` ou `ESC]...ESC\`) e as demais sequências de escape: removidas;
 * - outros caracteres de controle (exceto `\n` e `\t`) somem.
 * O resultado é texto puro: vai para a tela como texto React e para o "Copiar registro".
 */

const ESC = "\u001b";
const CSI_8BIT = "\u009b";
const OSC_8BIT = "\u009d";
/** Algum caractere de controle além de `\n` e `\t`: sem ele, o texto já está limpo. */
const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/;

/** Parâmetros + intermediários + final de uma CSI, a partir do índice dado. */
const CSI_BODY = /([0-?]*)([ -/]*)([@-~])/y;
/** Escape de dois ou mais bytes que não é CSI nem OSC (ex.: `ESC(B`, `ESC7`, `ESC=`). */
const ESC_OTHER = /[ -/]*[0-~]/y;

/**
 * Espaços que um movimento de cursor pode criar numa linha (`ESC[nC`, `ESC[nG` além do fim). Uma
 * sequência hostil como `ESC[99999999999C` não pode virar uma linha gigante e travar a aba.
 */
export const MAX_PAD = 512;
/** Teto do número lido de uma CSI: basta para qualquer terminal real. */
const MAX_PARAM = 100_000;

class Line {
  cells: string[] = [];
  col = 0;
  /** Espaços de preenchimento que ainda podem ser criados nesta linha. */
  padLeft = MAX_PAD;

  /** Move o cursor, sem passar de `MAX_PAD` colunas além do fim nem do que resta de preenchimento. */
  moveTo(col: number): void {
    this.col = Math.max(0, Math.min(col, this.cells.length + this.padLeft));
  }

  write(ch: string): void {
    this.moveTo(this.col);
    const pad = this.col - this.cells.length;
    if (pad > 0) {
      this.padLeft -= pad;
      for (let i = 0; i < pad; i += 1) this.cells.push(" ");
    }
    this.cells[this.col] = ch;
    this.col += 1;
  }

  erase(mode: number): void {
    if (mode === 0) this.cells.length = Math.min(this.cells.length, this.col);
    else if (mode === 1) for (let i = 0; i <= this.col && i < this.cells.length; i += 1) this.cells[i] = " ";
    else if (mode === 2) this.cells = [];
  }

  take(): string {
    const text = this.cells.join("");
    this.cells = [];
    this.col = 0;
    this.padLeft = MAX_PAD;
    return text;
  }
}

function firstParam(params: string, fallback: number): number {
  const head = (params.split(";")[0] ?? "").slice(0, 12);
  const n = Number.parseInt(head, 10);
  return Number.isNaN(n) ? fallback : Math.min(n, MAX_PARAM);
}

function applyCsi(line: Line, params: string, final: string): void {
  if (params.startsWith("?") || params.startsWith(">") || params.startsWith("<") || params.startsWith("=")) return;
  switch (final) {
    case "K":
      line.erase(firstParam(params, 0));
      return;
    case "G":
      line.moveTo(firstParam(params, 1) - 1);
      return;
    case "C":
      line.moveTo(line.col + Math.max(1, firstParam(params, 1)));
      return;
    case "D":
      line.col = Math.max(0, line.col - Math.max(1, firstParam(params, 1)));
      return;
    default:
      // SGR (m), movimentos verticais, limpeza de tela e o resto: sem efeito no texto.
      return;
  }
}

/** Fim de uma OSC: índice depois do BEL ou do ST (`ESC\`), ou o fim do texto. */
function oscEnd(raw: string, from: number): number {
  for (let i = from; i < raw.length; i += 1) {
    const ch = raw[i];
    if (ch === "\u0007" || ch === "\u009c") return i + 1;
    if (ch === ESC && raw[i + 1] === "\\") return i + 2;
  }
  return raw.length;
}

/** Texto de terminal sem sequências ANSI e com `\r`, `\b` e limpeza de linha já aplicados. */
export function cleanTerminalText(raw: string): string {
  if (!CONTROL.test(raw)) return raw;
  const lines: string[] = [];
  const line = new Line();
  let i = 0;
  while (i < raw.length) {
    const ch = raw[i] ?? "";
    if (ch === ESC || ch === CSI_8BIT || ch === OSC_8BIT) {
      const csi = ch === CSI_8BIT || raw[i + 1] === "[";
      const osc = ch === OSC_8BIT || raw[i + 1] === "]";
      const start = ch === ESC && (csi || osc) ? i + 2 : i + 1;
      if (osc) {
        i = oscEnd(raw, start);
        continue;
      }
      if (csi) {
        CSI_BODY.lastIndex = start;
        const m = CSI_BODY.exec(raw);
        if (m === null) {
          i = start;
          continue;
        }
        applyCsi(line, m[1] ?? "", m[3] ?? "");
        i = CSI_BODY.lastIndex;
        continue;
      }
      ESC_OTHER.lastIndex = start;
      i = ESC_OTHER.exec(raw) === null ? start : ESC_OTHER.lastIndex;
      continue;
    }
    if (ch === "\n") {
      lines.push(line.take());
    } else if (ch === "\r") {
      if (raw[i + 1] !== "\n") line.col = 0;
    } else if (ch === "\b") {
      line.col = Math.max(0, line.col - 1);
    } else if (ch === "\t" || !CONTROL.test(ch)) {
      const code = raw.codePointAt(i) ?? 0;
      const glyph = String.fromCodePoint(code);
      line.write(glyph);
      i += glyph.length;
      continue;
    }
    i += 1;
  }
  lines.push(line.take());
  return lines.join("\n");
}

/** Prefixo que o `docker compose logs` põe em cada linha (`web-1  | `), sem cor (`--no-color`). */
const COMPOSE_PREFIX = /^[A-Za-z0-9][\w.-]*\s+\| /;

/**
 * Registro do `docker compose logs` limpo linha a linha: o prefixo do serviço fica de fora da
 * limpeza, para que `\r` e `ESC[2K` da saída do app (barras de progresso) não apaguem o nome do
 * serviço. Linhas sem prefixo passam por `cleanTerminalText` inteiras.
 */
export function cleanComposeLog(raw: string): string {
  if (!CONTROL.test(raw)) return raw;
  return raw
    .split("\n")
    .map((line) => {
      const prefix = COMPOSE_PREFIX.exec(line)?.[0] ?? "";
      return prefix + cleanTerminalText(line.slice(prefix.length));
    })
    .join("\n");
}
