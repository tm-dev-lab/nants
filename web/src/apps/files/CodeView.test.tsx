/*
 * CodeMirror dos Arquivos (spec 093, T08): somente
 * leitura, num shadow root (CSP `style-src 'self'`), com o tema claro ou escuro dos tokens; sem
 * `adoptedStyleSheets`, cai para o texto puro.
 */
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import CodeView, { shadowStylesSupported } from "./CodeView";

const proto = ShadowRoot.prototype as unknown as { adoptedStyleSheets?: unknown };
const hadAdopted = "adoptedStyleSheets" in proto;
const originalSheet = window.CSSStyleSheet;

/** O jsdom pode não ter folhas construídas: simula o suficiente do navegador moderno. */
function simulateModernBrowser(): void {
  Object.defineProperty(ShadowRoot.prototype, "adoptedStyleSheets", {
    configurable: true,
    get() {
      return (this as { __sheets?: unknown[] }).__sheets ?? [];
    },
    set(value: unknown[]) {
      (this as { __sheets?: unknown[] }).__sheets = value;
    },
  });
  class FakeSheet {
    cssRules: unknown[] = [];
    insertRule(rule: string, index = 0): number {
      this.cssRules.splice(index, 0, rule);
      return index;
    }
    deleteRule(index: number): void {
      this.cssRules.splice(index, 1);
    }
  }
  window.CSSStyleSheet = FakeSheet as unknown as typeof CSSStyleSheet;
}

beforeEach(() => {
  if (!hadAdopted) simulateModernBrowser();
});

afterEach(() => {
  cleanup();
  if (!hadAdopted) {
    Reflect.deleteProperty(ShadowRoot.prototype, "adoptedStyleSheets");
    window.CSSStyleSheet = originalSheet;
  }
});

describe("CodeView dos Arquivos", () => {
  it("monta o editor num shadow root, somente leitura, sem vazar estilo para o head", async () => {
    const onError = vi.fn();
    const { getByTestId } = render(<CodeView content={"fn main() {\n  let x = 1;\n}\n"} filename="src/main.rs" wrap={false} theme="dark" onError={onError} />);
    const host = getByTestId("file-code-highlighted");
    await waitFor(() => expect(host.shadowRoot?.querySelector(".cm-editor")).not.toBeNull());
    const content = host.shadowRoot?.querySelector<HTMLElement>(".cm-content");
    expect(content?.textContent).toContain("let x = 1;");
    expect(content?.getAttribute("contenteditable")).toBe("false");
    expect(host.querySelector(".cm-editor")).toBeNull();
    expect(document.head.querySelector("style[data-cm]")).toBeNull();
    expect(onError).not.toHaveBeenCalled();
  });

  it("tema claro e escuro: o editor é remontado com o tema pedido", async () => {
    const { getByTestId, rerender } = render(<CodeView content="abc" filename="a.ts" wrap={false} theme="dark" onError={() => {}} />);
    const host = getByTestId("file-code-highlighted");
    await waitFor(() => expect(host.shadowRoot?.querySelector(".cm-editor")).not.toBeNull());
    expect(host.getAttribute("data-code-theme")).toBe("dark");
    rerender(<CodeView content="abc" filename="a.ts" wrap={false} theme="light" onError={() => {}} />);
    await waitFor(() => expect(host.shadowRoot?.querySelector(".cm-content")?.textContent).toContain("abc"));
    expect(host.getAttribute("data-code-theme")).toBe("light");
    expect(host.shadowRoot?.querySelectorAll(".cm-editor")).toHaveLength(1);
  });

  it("nome sem linguagem conhecida ainda mostra o texto; quebrar linhas monta com lineWrapping", async () => {
    const { getByTestId } = render(<CodeView content="abc" filename="arquivo.desconhecido-xyz" wrap theme="light" onError={() => {}} />);
    const host = getByTestId("file-code-highlighted");
    await waitFor(() => expect(host.shadowRoot?.querySelector(".cm-content")?.textContent).toContain("abc"));
    expect(host.shadowRoot?.querySelector(".cm-lineWrapping")).not.toBeNull();
  });

  it("sem adoptedStyleSheets não monta o editor e avisa para usar o texto puro", async () => {
    Reflect.deleteProperty(ShadowRoot.prototype, "adoptedStyleSheets");
    expect(shadowStylesSupported()).toBe(false);
    const onError = vi.fn();
    const { getByTestId } = render(<CodeView content="abc" filename="a.rs" wrap={false} theme="dark" onError={onError} />);
    await waitFor(() => expect(onError).toHaveBeenCalled());
    expect(getByTestId("file-code-highlighted").shadowRoot).toBeNull();
  });
});
