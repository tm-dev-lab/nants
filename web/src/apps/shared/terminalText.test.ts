/*
 * Texto de terminal limpo (spec 113, T03): amostras reais do Vite, cores 256 e 24 bits, cursor,
 * limpeza de linha, OSC e controles soltos.
 */
import { describe, expect, it } from "vitest";
import { MAX_PAD, cleanComposeLog, cleanTerminalText } from "./terminalText";

const E = "\u001b";

describe("cleanTerminalText", () => {
  it("texto sem controle passa igual", () => {
    expect(cleanTerminalText("web-1  | no ar\n\tok\n")).toBe("web-1  | no ar\n\tok\n");
    expect(cleanTerminalText("")).toBe("");
    expect(cleanTerminalText("Ø café ➜ 🙂")).toBe("Ø café ➜ 🙂");
  });

  it("amostra do Vite (Café Doce): cores, negrito e esmaecido somem", () => {
    const raw = `${E}[32m${E}[1mVITE${E}[22m v6.2.2${E}[39m  ${E}[2mready in ${E}[0m${E}[1m412${E}[22m${E}[2m${E}[0m ms${E}[22m`;
    expect(cleanTerminalText(raw)).toBe("VITE v6.2.2  ready in 412 ms");
    const local = `  ${E}[32m➜${E}[39m  ${E}[1mLocal${E}[22m:   ${E}[36mhttp://localhost:${E}[1m5173${E}[22m/${E}[39m`;
    expect(cleanTerminalText(local)).toBe("  ➜  Local:   http://localhost:5173/");
  });

  it("cores 256 e 24 bits, com e sem parâmetros", () => {
    expect(cleanTerminalText(`${E}[38;5;208mwarn${E}[0m ${E}[48;2;255;100;0mfundo${E}[m`)).toBe("warn fundo");
    expect(cleanTerminalText(`\u009b31mvermelho\u009b0m`)).toBe("vermelho");
  });

  it("\\r e limpeza de linha: a barra de progresso deixa só o estado final", () => {
    expect(cleanTerminalText(`building 10%\r${E}[2Kbuilding 55%\r${E}[2K${E}[1Gbuilding 100%\nfim`)).toBe("building 100%\nfim");
    expect(cleanTerminalText(`${E}[2K${E}[1Gpronto`)).toBe("pronto");
    expect(cleanTerminalText(`abcdef\r${E}[Kxy`)).toBe("xy");
    expect(cleanTerminalText(`abcdef${E}[3D${E}[K!`)).toBe("abc!");
    expect(cleanTerminalText("hello\rhi")).toBe("hillo");
    expect(cleanTerminalText("linha\r\noutra\r\n")).toBe("linha\noutra\n");
  });

  it("movimentos de cursor, modos privados, OSC e outros escapes não deixam lixo", () => {
    expect(cleanTerminalText(`${E}[?25l${E}[1A${E}[2J${E}[Hok${E}[?25h`)).toBe("ok");
    expect(cleanTerminalText(`${E}]0;titulo${E}\\a${E}]8;;http://x\u0007link${E}]8;;\u0007b`)).toBe("alinkb");
    expect(cleanTerminalText(`${E}(B${E}7x${E}8${E}=y`)).toBe("xy");
    expect(cleanTerminalText(`a${E}[5Cb`)).toBe("a     b");
    expect(cleanTerminalText("ab\bc")).toBe("ac");
    expect(cleanTerminalText(`fim${E}`)).toBe("fim");
    expect(cleanTerminalText(`x${E}[`)).toBe("x");
    expect(cleanTerminalText("a\u0007b\u0000c\u007fd")).toBe("abcd");
  });

  it("cursor hostil (ESC[99999999999C, ESC[99999999999G) termina rápido com linha limitada", () => {
    const started = performance.now();
    const forward = cleanTerminalText(`a${E}[99999999999Cx`);
    const absolute = cleanTerminalText(`a${E}[99999999999Gx`);
    const repeated = cleanTerminalText(`${E}[99999999999Cx`.repeat(2000));
    expect(performance.now() - started).toBeLessThan(1000);
    expect(forward).toBe(`a${" ".repeat(MAX_PAD)}x`);
    expect(absolute).toBe(`a${" ".repeat(MAX_PAD)}x`);
    // O preenchimento é um orçamento por linha: repetir não multiplica o tamanho.
    expect(repeated.length).toBeLessThanOrEqual(MAX_PAD + 2000);
    expect(cleanTerminalText(`a${E}[99999999999Cx\nb${E}[3Cy`)).toBe(`a${" ".repeat(MAX_PAD)}x\nb   y`);
  });

  it("nenhum caractere de controle sobra numa saída mista", () => {
    const raw = `${E}[32m${E}[1mVITE${E}[22m\r\n${E}[38;5;1mx${E}]0;t\u0007${E}[2K\ry\n`;
    const out = cleanTerminalText(raw);
    expect(out).toBe("VITE\ny\n");
    expect(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/.test(out)).toBe(false);
  });
});

describe("cleanComposeLog", () => {
  it("o prefixo do serviço sobrevive ao \\r e à limpeza de linha da saída do app", () => {
    const raw = `web-1  | building 10%\r${E}[2Kbuilding 100%\napi_2  | ${E}[32mok${E}[39m\r\nsem prefixo ${E}[1mx${E}[22m\n`;
    expect(cleanComposeLog(raw)).toBe("web-1  | building 100%\napi_2  | ok\nsem prefixo x\n");
  });
  it("texto sem controle passa igual", () => {
    expect(cleanComposeLog("web-1  | no ar\n")).toBe("web-1  | no ar\n");
  });
});
