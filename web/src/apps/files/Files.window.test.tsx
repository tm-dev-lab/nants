/*
 * Janela Arquivos montada pelo shell (spec 093, T08): árvore, teclado, pastas ignoradas sob
 * demanda, visualizador, fonte da tarefa, alterados, diff, o arquivo aberto e a
 * divisão em `selection`, a divisória por teclado, o `data-size` estreito e o poll que pausa com
 * a janela invisível.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";

// O realce tem testes próprios (CodeView.test e FileViewer.highlight.test); aqui vale o texto puro.
vi.mock("./CodeView", () => {
  throw new Error("chunk indisponível");
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");

const BASE = {
  source: "base",
  truncated: false,
  entries: [
    { path: "README.md", kind: "file" },
    { path: "src/main.rs", kind: "file" },
    { path: "src/lib.rs", kind: "file" },
    { path: ".env", kind: "file" },
    { path: "link", kind: "symlink" },
    { path: "target", kind: "ignored_dir" },
  ],
};
const TASK = { source: "task", task_id: "T04", truncated: false, changed: ["src/main.rs"], entries: BASE.entries.slice(0, 3) };
const DIFF = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,2 +1,2 @@\n fn main() {\n-    old();\n+    new();\n }\n";

interface Options {
  taskActive?: boolean;
  truncated?: boolean;
  ui?: Record<string, unknown>;
  extra?: RenderShellOptions;
}

async function openFiles({ taskActive = false, truncated = false, ui, extra = {} }: Options = {}) {
  // Planejamento (Conversa + Plano): a janela Arquivos só abre pelo dock, depois das rotas falsas.
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    ...(ui !== undefined ? { ui: { p1: { rev: 1, data: ui } } } : {}),
    ...extra,
  });
  const state = { taskActive };
  view.server.reply("GET", /\/api\/projects\/p1\/files\?/, (_b, url) => {
    const params = new URL(url, "http://x").searchParams;
    if (params.get("source") === "task") return state.taskActive ? { status: 200, body: TASK } : { status: 409, body: { reason: "no_active_task" } };
    if (params.get("dir") === "target") {
      return { status: 200, body: { source: "base", dir: "target", truncated: false, entries: [{ path: "target/debug", kind: "dir" }, { path: "target/CACHEDIR.TAG", kind: "file" }] } };
    }
    return { status: 200, body: { ...BASE, truncated } };
  });
  view.server.reply("GET", /\/api\/projects\/p1\/file\?/, (_b, url) => {
    const params = new URL(url, "http://x").searchParams;
    if (params.get("diff") === "1") return { status: 200, body: { kind: "diff", size: DIFF.length, content: DIFF } };
    const path = params.get("path");
    if (path === "README.md") return { status: 200, body: { kind: "text", size: 12, content: "# Titulo\n<b>x</b>\n", masked: false } };
    if (path === "src/main.rs") return { status: 200, body: { kind: "text", size: 14, content: "fn main() {}\n", masked: false } };
    return { status: 404, body: { error: "não encontrado" } };
  });
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Arquivos" }));
  });
  await settle();
  return { ...view, state };
}

function files(): HTMLElement {
  return screen.getByRole("region", { name: "Arquivos" });
}

const fileCalls = (server: Awaited<ReturnType<typeof openFiles>>["server"], pattern = /\/files\?source=base$/) =>
  server.calls.filter((c) => c.method === "GET" && pattern.test(c.url)).length;

describe("Arquivos: árvore", () => {
  it("pastas primeiro, dotfiles ocultos, ignorado pelo git recolhido, symlink sem abrir e sem caracteres no lugar de ícones", async () => {
    const { server } = await openFiles();
    const win = files();
    const tree = await within(win).findByRole("tree", { name: "Arquivos do projeto" });
    expect(within(tree).getAllByRole("treeitem").map((el) => el.getAttribute("data-path"))).toEqual(["src", "target", "link", "README.md"]);
    expect(within(win).getByText("ignorado pelo git")).toBeTruthy();
    fireEvent.click(within(win).getByRole("button", { name: "Mostrar ocultos" }));
    expect(within(tree).getAllByRole("treeitem").map((el) => el.getAttribute("data-path"))).toContain(".env");
    const before = server.calls.length;
    fireEvent.click(within(win).getByText("link"));
    await settle();
    expect(server.calls.filter((c) => c.url.includes("/file?")).length).toBe(server.calls.slice(0, before).filter((c) => c.url.includes("/file?")).length);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("pasta ignorada só carrega ao expandir (dir=)", async () => {
    const { server } = await openFiles();
    const win = files();
    await within(win).findByRole("tree");
    expect(server.calls.some((c) => c.url.includes("dir=target"))).toBe(false);
    fireEvent.click(within(win).getByText("target"));
    await within(win).findByText("debug");
    expect(server.calls.filter((c) => c.url.includes("dir=target"))).toHaveLength(1);
    expect(within(win).getByText("CACHEDIR.TAG")).toBeTruthy();
  });

  it("teclado: setas, Enter expande e abre, Esquerda recolhe", async () => {
    await openFiles();
    const win = files();
    const tree = await within(win).findByRole("tree");
    const first = within(tree).getAllByRole("treeitem")[0] as HTMLElement;
    first.focus();
    fireEvent.keyDown(tree, { key: "ArrowRight" });
    expect(first.getAttribute("aria-expanded")).toBe("true");
    fireEvent.keyDown(tree, { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-path")).toBe("src/lib.rs");
    fireEvent.keyDown(tree, { key: "ArrowLeft" });
    expect(document.activeElement?.getAttribute("data-path")).toBe("src");
    fireEvent.keyDown(tree, { key: "ArrowLeft" });
    expect(first.getAttribute("aria-expanded")).toBe("false");
    fireEvent.keyDown(tree, { key: "End" });
    expect(document.activeElement?.getAttribute("data-path")).toBe("README.md");
    fireEvent.keyDown(tree, { key: "Enter" });
    expect((await within(win).findByTestId("file-code")).textContent).toContain("# Titulo");
  });

  it("lista truncada avisa; erro da lista é estático", async () => {
    await openFiles({ truncated: true });
    expect(await within(files()).findByText(/Lista parcial/)).toBeTruthy();
    cleanup();
    const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    view.server.reply("GET", /\/files\?/, () => ({ status: 409, body: { error: "segredo do servidor" } }));
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Arquivos" }));
    });
    await settle();
    const alert = within(files()).getByRole("alert");
    expect(alert.textContent).toContain("Não foi possível listar os arquivos");
    expect(alert.textContent).not.toContain("segredo do servidor");
  });
});

describe("Arquivos: visualizador", () => {
  it("texto com número de linha, tamanho e caminho, sem interpretar HTML; quebra opcional", async () => {
    await openFiles();
    const win = files();
    fireEvent.click(await within(win).findByText("README.md"));
    const code = await within(win).findByTestId("file-code");
    expect(code.textContent).toContain("<b>x</b>");
    expect(code.querySelector("b")).toBeNull();
    expect(within(win).getByTestId("file-path").textContent).toBe("README.md");
    // O caminho longo trunca na tela; o título traz o caminho inteiro.
    expect(within(win).getByTestId("file-path").getAttribute("title")).toBe("README.md");
    expect(within(code).getByText("2")).toBeTruthy();
    expect(within(win).getByText("12 B")).toBeTruthy();
    const wrap = within(win).getByRole("button", { name: "Quebrar linhas" });
    fireEvent.click(wrap);
    expect(wrap.getAttribute("aria-pressed")).toBe("true");
    expect(code.getAttribute("data-wrap")).toBe("true");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Fechar arquivo volta à árvore inteira, sem divisória, e devolve o foco ao item", async () => {
    await openFiles();
    const win = files();
    fireEvent.click(await within(win).findByText("README.md"));
    await within(win).findByTestId("file-code");
    expect(within(win).getByRole("separator", { name: "Ajustar a largura da árvore" })).toBeTruthy();
    fireEvent.click(within(win).getByRole("button", { name: "Fechar arquivo" }));
    await settle();
    expect(within(win).queryByRole("separator")).toBeNull();
    expect(document.activeElement?.getAttribute("data-path")).toBe("README.md");
  });

  it("A-07 (094/T08): sem arquivo aberto, o lado do visualizador orienta ('Escolha um arquivo na árvore')", async () => {
    await openFiles();
    const win = files();
    await within(win).findByRole("tree");
    const placeholder = win.querySelector("[data-files-placeholder]") as HTMLElement;
    expect(placeholder.textContent).toBe("Escolha um arquivo na árvore");
    expect(placeholder.querySelector("[data-empty-state]")).not.toBeNull();
    fireEvent.click(await within(win).findByText("README.md"));
    await within(win).findByTestId("file-code");
    expect(win.querySelector("[data-files-placeholder]")).toBeNull();
  });
});

describe("Arquivos: fonte da tarefa", () => {
  it("sem tarefa ativa (409) o 'Trabalho em andamento' fica desabilitado, com o motivo", async () => {
    await openFiles();
    const win = files();
    await within(win).findByRole("tree");
    expect((within(win).getByRole("button", { name: "Trabalho em andamento" }) as HTMLButtonElement).disabled).toBe(true);
    expect(within(win).getByTestId("files-source-reason").textContent).toBe("Sem tarefa em andamento");
  });

  it("095/T06: a fonte e 'Atualizar' ficam no slot da barra de título; o motivo descreve o botão desabilitado", async () => {
    await openFiles();
    const win = files();
    await within(win).findByRole("tree");
    const group = within(win).getByRole("group", { name: "Fonte" });
    expect(group.closest("[data-window-tools]")).not.toBeNull();
    expect(group.closest("[data-window-content]")).toBeNull();
    const refresh = within(win).getByRole("button", { name: "Atualizar" });
    expect(refresh.closest("[data-window-tools]")).not.toBeNull();
    const task = within(win).getByRole("button", { name: "Trabalho em andamento" });
    expect(task.getAttribute("aria-describedby")).toBe(within(win).getByTestId("files-source-reason").id);
    // Os alternadores da árvore ficam na árvore, neutros (fantasma).
    const hidden = within(win).getByRole("button", { name: "Mostrar ocultos" });
    expect(hidden.closest("[data-window-content]")).not.toBeNull();
    expect(hidden.getAttribute("data-variant")).toBe("ghost");
  });

  it("com tarefa: alterado marcado, 'Só alterados', Diff com linhas por tipo e 409 volta para a base com nota", async () => {
    const { state, server } = await openFiles({ taskActive: true });
    const win = files();
    const task = await within(win).findByRole("button", { name: "Trabalho em andamento · T04" });
    fireEvent.click(task);
    await settle();
    expect(task.getAttribute("aria-pressed")).toBe("true");
    const tree = within(win).getByRole("tree");
    fireEvent.click(within(win).getByRole("button", { name: "Só alterados" }));
    await waitFor(() => expect(within(tree).getAllByRole("treeitem").map((el) => el.getAttribute("data-path"))).toEqual(["src", "src/main.rs"]));
    expect(within(tree).getByText("alterado")).toBeTruthy();
    fireEvent.click(within(tree).getByText("main.rs"));
    await within(win).findByTestId("file-code");
    fireEvent.click(within(win).getByRole("button", { name: "Diff" }));
    const diff = await within(win).findByTestId("file-diff");
    expect(server.calls.some((c) => c.url.includes("diff=1"))).toBe(true);
    expect([...diff.querySelectorAll("[data-diff]")].map((l) => l.getAttribute("data-diff"))).toEqual(["head", "head", "hunk", "ctx", "del", "add", "ctx"]);
    // A tarefa terminou: o próximo "Atualizar" responde 409 e a janela volta para a base.
    state.taskActive = false;
    fireEvent.click(within(win).getByRole("button", { name: "Atualizar" }));
    await settle();
    expect(within(win).getByRole("button", { name: "Código integrado" }).getAttribute("aria-pressed")).toBe("true");
    expect(within(win).getByText("A tarefa terminou: mostrando o código integrado.")).toBeTruthy();
  });
});

describe("Arquivos: estado salvo em selection", () => {
  it("abrir um arquivo grava filePath e fileSource; ao montar de novo, o arquivo volta aberto", async () => {
    const { server } = await openFiles();
    fireEvent.click(await within(files()).findByText("README.md"));
    await settle();
    expect(server.model.ui.p1?.data.selection).toMatchObject({ filePath: "README.md", fileSource: "base" });
    cleanup();
    await openFiles({ ui: { selection: { filePath: "README.md", fileSource: "base" } } });
    expect((await within(files()).findByTestId("file-path")).textContent).toBe("README.md");
  });

  it("arquivo que não existe mais é esquecido sem erro", async () => {
    const { server } = await openFiles({ ui: { selection: { filePath: "sumiu.txt", fileSource: "base" } } });
    await within(files()).findByRole("tree");
    expect(within(files()).queryByTestId("file-path")).toBeNull();
    expect(within(files()).queryByRole("alert")).toBeNull();
    expect(server.model.ui.p1?.data.selection).toMatchObject({ filePath: null });
  });

  it("divisória: ARIA, teclado (setas, Home, End), grava filesSplit; a fração salva é restaurada", async () => {
    const { server } = await openFiles();
    const win = files();
    fireEvent.click(await within(win).findByText("README.md"));
    await within(win).findByTestId("file-code");
    const handle = within(win).getByRole("separator", { name: "Ajustar a largura da árvore" });
    expect(handle.getAttribute("aria-orientation")).toBe("vertical");
    expect(handle.getAttribute("aria-valuenow")).toBe("34");
    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(handle.getAttribute("aria-valuenow")).toBe("38");
    fireEvent.keyDown(handle, { key: "Home" });
    expect(handle.getAttribute("aria-valuenow")).toBe("15");
    fireEvent.keyDown(handle, { key: "End" });
    expect(handle.getAttribute("aria-valuenow")).toBe("85");
    expect((win.querySelector("[data-files-tree]") as HTMLElement).style.flexBasis).toBe("85%");
    await settle();
    expect(server.model.ui.p1?.data.selection).toMatchObject({ filesSplit: 0.85 });
    cleanup();
    await openFiles({ ui: { selection: { filePath: "README.md", fileSource: "base", filesSplit: 0.5 } } });
    const restored = await within(files()).findByRole("separator");
    expect(restored.getAttribute("aria-valuenow")).toBe("50");
  });

  it("divisória por ponteiro: arrastar muda a largura e só grava ao soltar", async () => {
    const { server } = await openFiles();
    const win = files();
    fireEvent.click(await within(win).findByText("README.md"));
    await within(win).findByTestId("file-code");
    const body = win.querySelector("[data-arrangement]") as HTMLElement;
    body.getBoundingClientRect = () => ({ left: 0, top: 0, width: 1000, height: 400, right: 1000, bottom: 400, x: 0, y: 0, toJSON: () => ({}) });
    const handle = within(win).getByRole("separator");
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 340 });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: 600 });
    expect(handle.getAttribute("aria-valuenow")).toBe("60");
    await settle();
    expect(server.model.ui.p1?.data.selection).not.toMatchObject({ filesSplit: 0.6 });
    fireEvent.pointerUp(handle, { pointerId: 1, clientX: 600 });
    await settle();
    expect(server.model.ui.p1?.data.selection).toMatchObject({ filesSplit: 0.6 });
  });
});

describe("Arquivos: responsividade e desempenho", () => {
  it("estreita: árvore OU arquivo, com Voltar; sem divisória", async () => {
    const observers = fakeResizeObservers();
    await openFiles({ extra: { createObserver: observers.factory } });
    await act(async () => observers.resize(380));
    const win = files();
    expect(win.getAttribute("data-size")).toBe("narrow");
    fireEvent.click(await within(win).findByText("README.md"));
    await within(win).findByTestId("file-code");
    expect(within(win).queryByRole("tree")).toBeNull();
    expect(within(win).queryByRole("separator")).toBeNull();
    fireEvent.click(within(win).getByRole("button", { name: "Voltar" }));
    expect(within(win).getByRole("tree")).toBeTruthy();
  });

  it("janela minimizada não relê nada com eventos; ao restaurar, atualiza uma vez", async () => {
    const { server } = await openFiles();
    await within(files()).findByRole("tree");
    const dockButton = within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Arquivos" });
    // Em foco: o clique no dock minimiza.
    await act(async () => {
      fireEvent.click(dockButton);
    });
    await settle();
    expect((document.querySelector('[data-window="files"]') as HTMLElement).hidden).toBe(true);
    const before = fileCalls(server);
    act(() => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } }));
    });
    await new Promise((resolve) => setTimeout(resolve, 1700));
    await settle();
    expect(fileCalls(server)).toBe(before);
    await act(async () => {
      fireEvent.click(dockButton);
    });
    await settle();
    expect(fileCalls(server)).toBe(before + 1);
  });

  it("minimizada no arranjo salvo: nenhuma requisição inicial (sonda, lista, arquivo salvo); ao restaurar, carrega uma vez", async () => {
    const minimized = {
      layouts: {
        planning: {
          current: {
            windows: {
              conversation: { x: 0, y: 0, w: 500, h: 400, z: 1, mode: "normal", open: true },
              files: { x: 40, y: 40, w: 700, h: 500, z: 2, mode: "minimized", open: true, restoreMode: "normal" },
            },
          },
          mine: null,
        },
      },
      selection: { filePath: "README.md", fileSource: "base" },
    };
    const view = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      ui: { p1: { rev: 1, data: minimized } },
    });
    view.server.reply("GET", /\/api\/projects\/p1\/files\?/, () => ({ status: 200, body: BASE }));
    view.server.reply("GET", /\/api\/projects\/p1\/file\?/, () => ({ status: 200, body: { kind: "text", size: 12, content: "# Titulo\n", masked: false } }));
    await settle();
    const win = document.querySelector('[data-window="files"]') as HTMLElement;
    expect(win.hidden).toBe(true);
    const fileApi = (): number => view.server.calls.filter((c) => /\/api\/projects\/p1\/files?\?/.test(c.url)).length;
    expect(fileApi()).toBe(0);
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Arquivos" }));
    });
    await settle();
    expect(win.hidden).toBe(false);
    expect(view.server.count("GET", /\/files\?source=task$/)).toBe(1);
    expect(view.server.count("GET", /\/files\?source=base$/)).toBe(1);
    expect(await within(files()).findByTestId("file-code")).toBeTruthy();
    expect(view.server.count("GET", /\/file\?/)).toBe(1);
  });

  it("'Atualizar' reavalia a lista e a sonda da fonte da tarefa", async () => {
    const { server } = await openFiles();
    await within(files()).findByRole("tree");
    const [lists, probes] = [fileCalls(server), fileCalls(server, /\/files\?source=task/)];
    await act(async () => {
      fireEvent.click(within(files()).getByRole("button", { name: "Atualizar" }));
    });
    await settle();
    expect(fileCalls(server)).toBe(lists + 1);
    expect(fileCalls(server, /\/files\?source=task/)).toBe(probes + 1);
  });

  it("a atualização não rouba o foco que saiu da árvore (End na última linha não deixa foco armado)", async () => {
    const { server } = await openFiles();
    const tree = await within(files()).findByRole("tree");
    (within(tree).getAllByRole("treeitem").at(-1) as HTMLElement).focus();
    fireEvent.keyDown(tree, { key: "End" });
    const outside = screen.getByRole("button", { name: "Personalizar" });
    outside.focus();
    const before = fileCalls(server);
    act(() => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } }));
    });
    await waitFor(() => expect(fileCalls(server)).toBe(before + 1), { timeout: 4000 });
    expect(document.activeElement).toBe(outside);
  });

  it("evento com a janela visível atualiza a lista depois do debounce", async () => {
    const { server } = await openFiles();
    await within(files()).findByRole("tree");
    const before = fileCalls(server);
    act(() => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: (s.revisions.p1 ?? 0) + 1 } }));
    });
    await waitFor(() => expect(fileCalls(server)).toBe(before + 1), { timeout: 4000 });
  });
});
