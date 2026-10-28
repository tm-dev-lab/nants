/*
 * Visualizador dos Arquivos (spec 093, T08): estados, máscara, copiar, releitura com
 * If-None-Match, realce sob demanda (tetos, falha) e o tema do CodeMirror que segue o do desktop.
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useEffect, type ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { AppearanceContext } from "../../desktop/theme/appearanceContext";
import { DEFAULT_APPEARANCE } from "../../desktop/theme/appearance";
import { setLanguage } from "../../i18n";
import { HIGHLIGHT_MAX_BYTES, HIGHLIGHT_MAX_LINES } from "./filesModel";
import { FileViewer } from "./FileViewer";

vi.mock("./CodeView", () => ({
  default: (props: { content: string; filename: string; wrap: boolean; theme: string; onError: () => void }) => {
    useEffect(() => {
      if (props.content.includes("BOOM")) props.onError();
    }, [props]);
    return (
      <div data-testid="cm-mock" data-filename={props.filename} data-wrap={String(props.wrap)} data-theme={props.theme}>
        {props.content}
      </div>
    );
  },
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

interface Reply {
  status?: number;
  body?: unknown;
  etag?: string;
}

let calls: { url: URL; headers: Record<string, string> }[] = [];

function stubFetch(handler: (url: URL, headers: Record<string, string>) => Reply): void {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: string | URL | Request, init?: RequestInit) => {
      const url = new URL(typeof input === "string" ? input : input.toString(), "http://localhost");
      const headers = (init?.headers ?? {}) as Record<string, string>;
      calls.push({ url, headers });
      const reply = handler(url, headers);
      const h: Record<string, string> = { "Content-Type": "application/json" };
      if (reply.etag !== undefined) h.ETag = reply.etag;
      const status = reply.status ?? 200;
      return Promise.resolve(new Response(status === 304 ? null : JSON.stringify(reply.body ?? {}), { status, headers: h }));
    }),
  );
}

const text = (content: string, masked = false): Reply => ({ body: { kind: "text", size: content.length, content, masked } });

function themed(node: ReactNode, theme: "dark" | "light" = "dark") {
  return render(<AppearanceContext.Provider value={{ appearance: DEFAULT_APPEARANCE, theme, setAppearance: () => undefined, setLanguage: () => undefined }}>{node}</AppearanceContext.Provider>);
}

describe("FileViewer: estados", () => {
  it("binário, grande demais e erro estático", async () => {
    stubFetch((url) => {
      const path = url.searchParams.get("path");
      if (path === "logo.png") return { body: { kind: "binary", size: 2048 } };
      if (path === "big.sql") return { body: { kind: "too_large", size: 3 * 1024 * 1024 } };
      return { status: 404, body: { error: "segredo" } };
    });
    const first = themed(<FileViewer projectId="p1" source="base" path="logo.png" refreshKey={0} />);
    await screen.findByText(/Arquivo binário \(2 KB\)/);
    first.unmount();
    const second = themed(<FileViewer projectId="p1" source="base" path="big.sql" refreshKey={0} />);
    await screen.findByText(/grande demais para exibir \(3 MB\)/);
    second.unmount();
    themed(<FileViewer projectId="p1" source="base" path="nada.txt" refreshKey={0} />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("recurso não encontrado");
    expect(alert.textContent).not.toContain("segredo");
  });

  it("mostra o carregando antes da resposta", () => {
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>(() => {})));
    themed(<FileViewer projectId="p1" source="base" path="README.md" refreshKey={0} />);
    expect(screen.getByText("Carregando arquivo.")).toBeTruthy();
  });

  it("arquivo mascarado: faixa com Mostrar valores (reveal=1), sem guardar; reabrir volta mascarado", async () => {
    stubFetch((url) => (url.searchParams.get("reveal") === "1" ? text("KEY=segredo\n", true) : text("KEY=oculto\n", true)));
    const view = themed(<FileViewer projectId="p1" source="base" path=".env" refreshKey={0} />);
    expect(await screen.findByText(/KEY=oculto/)).toBeTruthy();
    expect(screen.getByText(/Valores ocultos/)).toBeTruthy();
    expect(findForbiddenGlyphs(view.container)).toEqual([]);
    fireEvent.click(screen.getByRole("button", { name: "Mostrar valores" }));
    expect(await screen.findByText(/KEY=segredo/)).toBeTruthy();
    expect(calls.some((c) => c.url.searchParams.get("reveal") === "1")).toBe(true);
    view.unmount();
    themed(<FileViewer projectId="p1" source="base" path=".env" refreshKey={0} />);
    await screen.findByText(/Valores ocultos/);
  });

  it("Copiar envia o conteúdo à área de transferência", async () => {
    stubFetch(() => text("# Titulo\n"));
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    themed(<FileViewer projectId="p1" source="base" path="README.txt" refreshKey={0} />);
    await screen.findByText("# Titulo");
    fireEvent.click(screen.getByRole("button", { name: "Copiar" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("# Titulo\n"));
    await screen.findByText("Copiado");
  });

  it("spec 131 T06: em en, os títulos da tarefa saem traduzidos na tela; o Copiar leva o arquivo gravado", async () => {
    const raw = "## Objetivo\nShip it.\n## Critérios de Aceitação\n- [ ] ok\n";
    stubFetch(() => text(raw));
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    setLanguage("en", { persist: false });
    try {
      const view = themed(<FileViewer projectId="p1" source="base" path=".sdd/specs/001-ledger/tasks/T01.md" refreshKey={0} />);
      const editor = await screen.findByTestId("cm-mock");
      expect(editor.textContent).toContain("## Objective");
      expect(editor.textContent).toContain("## Acceptance Criteria");
      expect(editor.textContent).not.toContain("Objetivo");
      fireEvent.click(screen.getByRole("button", { name: "Copy" }));
      await waitFor(() => expect(writeText).toHaveBeenCalledWith(raw));
      view.unmount();
      // Fora do spec.md e das tarefas, o texto aparece como está.
      themed(<FileViewer projectId="p1" source="base" path="notes/T01.md" refreshKey={0} />);
      expect((await screen.findByTestId("cm-mock")).textContent).toContain("## Objetivo");
    } finally {
      setLanguage("pt-BR", { persist: false });
    }
  });

  it("nova atualização relê com If-None-Match; 304 mantém o conteúdo", async () => {
    stubFetch((_url, headers) => (headers["If-None-Match"] === '"e1"' ? { status: 304 } : { ...text("# Titulo\n"), etag: '"e1"' }));
    const view = themed(<FileViewer projectId="p1" source="base" path="README.txt" refreshKey={0} />);
    await screen.findByText("# Titulo");
    await act(async () => {
      view.rerender(
        <AppearanceContext.Provider value={{ appearance: DEFAULT_APPEARANCE, theme: "dark", setAppearance: () => undefined, setLanguage: () => undefined }}>
          <FileViewer projectId="p1" source="base" path="README.txt" refreshKey={1} />
        </AppearanceContext.Provider>,
      );
    });
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1]?.headers["If-None-Match"]).toBe('"e1"');
    expect(screen.getByText("# Titulo")).toBeTruthy();
  });
});

describe("FileViewer: realce sob demanda", () => {
  it("troca o texto puro pelo editor, com o nome do arquivo e o tema do desktop", async () => {
    stubFetch(() => text("fn main() {}\n"));
    themed(<FileViewer projectId="p1" source="base" path="src/main.rs" refreshKey={0} />, "light");
    const editor = await screen.findByTestId("cm-mock");
    expect(editor.getAttribute("data-filename")).toBe("src/main.rs");
    expect(editor.getAttribute("data-theme")).toBe("light");
    expect(screen.queryByTestId("file-code")).toBeNull();
  });

  it("acima do teto de bytes ou de linhas mantém o texto puro", async () => {
    stubFetch(() => text("a".repeat(HIGHLIGHT_MAX_BYTES + 1)));
    const view = themed(<FileViewer projectId="p1" source="base" path="big.txt" refreshKey={0} />);
    await screen.findByTestId("file-code");
    expect(screen.queryByTestId("cm-mock")).toBeNull();
    view.unmount();
    stubFetch(() => text("x\n".repeat(HIGHLIGHT_MAX_LINES + 1)));
    themed(<FileViewer projectId="p1" source="base" path="many.txt" refreshKey={0} />);
    await screen.findByTestId("file-code");
    expect(screen.queryByTestId("cm-mock")).toBeNull();
  });

  it("falha do editor ao montar volta ao texto puro", async () => {
    stubFetch(() => text("BOOM\nlinha 2\n"));
    themed(<FileViewer projectId="p1" source="base" path="a.txt" refreshKey={0} />);
    const code = await screen.findByTestId("file-code");
    await waitFor(() => expect(screen.queryByTestId("cm-mock")).toBeNull());
    expect(code.textContent).toContain("linha 2");
  });
});
