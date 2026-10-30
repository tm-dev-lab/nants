/*
 * Arquivos nos dois idiomas (spec 102, T04): em en, nenhum texto visível ou acessível da janela
 * fica em português (fonte, árvore, visualizador, notas, divisória e erros do servidor no idioma
 * ativo); os nomes e o conteúdo dos arquivos aparecem como vieram; pt-BR igual a hoje; a troca
 * vale na hora, sem recarregar.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { activateApp, renderShell, settle } from "../../desktop/shell-test-utils";
import type { SpaceData } from "../../desktop/useSpaceData";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { filesSummary, formatBytes } from "./filesModel";

// O realce tem testes próprios; aqui vale o texto puro.
vi.mock("./CodeView", () => {
  throw new Error("chunk indisponível");
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

type Lang = "pt-BR" | "en";
const NAME: Readonly<Record<Lang, string>> = { "pt-BR": "Arquivos", en: "Files" };

const BASE = {
  source: "base",
  truncated: false,
  entries: [
    { path: "README.md", kind: "file" },
    { path: "src/main.rs", kind: "file" },
    { path: "logo.png", kind: "file" },
    { path: "dump.sql", kind: "file" },
    { path: "config.env", kind: "file" },
    { path: "gone.txt", kind: "file" },
    { path: "link", kind: "symlink" },
    { path: "target", kind: "ignored_dir" },
  ],
};
const TASK = { source: "task", task_id: "T04", truncated: false, changed: ["src/main.rs"], entries: BASE.entries.slice(0, 2) };

interface Options {
  taskActive?: boolean;
  truncated?: boolean;
  listFails?: boolean;
}

async function mount(lang: Lang, { taskActive = false, truncated = false, listFails = false }: Options = {}) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Shop")], states: { p1: makeProjectState({ id: "p1", name: "Shop" }) } });
  view.server.reply("GET", /\/api\/projects\/p1\/files\?/, (_b, url) => {
    const params = new URL(url, "http://x").searchParams;
    if (params.get("source") === "task") return taskActive ? { status: 200, body: TASK } : { status: 409, body: { reason: "no_active_task" } };
    if (params.get("dir") === "target") return { status: 500, body: {} };
    if (listFails) return { status: 500, body: {} };
    return { status: 200, body: { ...BASE, truncated } };
  });
  view.server.reply("GET", /\/api\/projects\/p1\/file\?/, (_b, url) => {
    const path = new URL(url, "http://x").searchParams.get("path");
    if (path === "README.md") return { status: 200, body: { kind: "text", size: 1536, content: "# Title\nHello\n", masked: false } };
    if (path === "src/main.rs") return { status: 200, body: { kind: "text", size: 14, content: "fn main() {}\n", masked: false } };
    if (path === "logo.png") return { status: 200, body: { kind: "binary", size: 2048 } };
    if (path === "dump.sql") return { status: 200, body: { kind: "too_large", size: 3 * 1024 * 1024 } };
    if (path === "config.env") return { status: 200, body: { kind: "text", size: 9, content: "KEY=****\n", masked: true } };
    return { status: 404, body: { error: "não encontrado", code: "not_found" } };
  });
  await act(async () => {
    activateApp(NAME[lang]);
  });
  await settle();
  return view;
}

const files = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: NAME[lang] });

async function open(name: string, lang: Lang = "en"): Promise<void> {
  fireEvent.click(await within(files(lang)).findByText(name));
  await settle();
}

describe("Arquivos em en", () => {
  it("fonte, árvore, estado sem arquivo, divisória e visualizador sem português", async () => {
    await mount("en");
    const win = files();
    const tree = await within(win).findByRole("tree", { name: "Project files" });
    expect(within(tree).getByText("ignored by git")).toBeTruthy();
    expect(within(tree).getByText("symbolic link")).toBeTruthy();
    expect(within(win).getByRole("group", { name: "Source" })).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Integrated code" }).getAttribute("aria-pressed")).toBe("true");
    expect((within(win).getByRole("button", { name: "Work in progress" }) as HTMLButtonElement).disabled).toBe(true);
    expect(within(win).getByTestId("files-source-reason").textContent).toBe("No task in progress");
    const refresh = within(win).getByRole("button", { name: "Refresh" });
    expect(refresh.getAttribute("title")).toBe("Refresh");
    expect(within(win).getByRole("button", { name: "Show hidden" })).toBeTruthy();
    expect(win.querySelector("[data-files-placeholder]")?.textContent).toBe("Choose a file in the tree");
    expect(document.getElementById(windowSubtitleId("files"))?.textContent).toBe("integrated code");
    expect(portugueseLeftovers(win)).toEqual([]);

    await open("README.md");
    const viewer = within(win).getByRole("region", { name: "File content" });
    expect(within(viewer).getByRole("button", { name: "Close file" }).getAttribute("title")).toBe("Close file");
    expect(within(viewer).getByRole("button", { name: "Wrap lines" })).toBeTruthy();
    expect(within(viewer).getByRole("button", { name: "Copy" })).toBeTruthy();
    // Número pelo formatador do idioma ativo.
    expect(viewer.querySelector(".nd-file-size")?.textContent).toBe("1.5 KB");
    // Conteúdo do arquivo como veio.
    expect(within(viewer).getByTestId("file-code").textContent).toContain("Hello");
    const handle = within(win).getByRole("separator", { name: "Adjust the tree width" });
    expect(handle.getAttribute("aria-valuetext")).toBe("Tree at 34% of the width");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("binário, grande demais, mascarado, erro do servidor e pasta com falha", async () => {
    await mount("en");
    const win = files();
    await within(win).findByRole("tree");
    await open("logo.png");
    expect(within(win).getByText("Binary file (2 KB): not shown.")).toBeTruthy();
    await open("dump.sql");
    expect(within(win).getByText("File too large to show (3 MB).")).toBeTruthy();
    await open("config.env");
    expect(within(win).getByText("Values hidden: file ignored by git.")).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Show values" })).toBeTruthy();
    expect(portugueseLeftovers(win)).toEqual([]);
    await open("gone.txt");
    const alert = within(win).getByRole("alert");
    expect(alert.textContent).toMatch(/^Could not open the file: /);
    expect(portugueseLeftovers(win)).toEqual([]);
    fireEvent.click(within(win).getByText("target"));
    await settle();
    await waitFor(() => expect(within(win).getAllByRole("alert").length).toBe(2));
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("lista parcial, falha da lista e fonte da tarefa (alterados)", async () => {
    await mount("en", { truncated: true });
    await within(files()).findByRole("tree");
    expect(within(files()).getByText("Partial list: the project has too many files and only some of them are shown.")).toBeTruthy();
    expect(portugueseLeftovers(files())).toEqual([]);
    cleanup();

    await mount("en", { listFails: true });
    await waitFor(() => expect(within(files()).getByRole("alert").textContent).toMatch(/^Could not list the files: /));
    expect(portugueseLeftovers(files())).toEqual([]);
    cleanup();

    await mount("en", { taskActive: true });
    const task = await within(files()).findByRole("button", { name: "Work in progress · T04" });
    fireEvent.click(task);
    await settle();
    expect(within(files()).getByRole("button", { name: "Changed only" })).toBeTruthy();
    fireEvent.click(within(files()).getByText("src"));
    await settle();
    expect(within(files()).getByRole("tree").textContent).toContain("changed");
    expect(portugueseLeftovers(files())).toEqual([]);
  });

  it("funções do modelo no idioma ativo", () => {
    setLanguage("en", { persist: false });
    expect([formatBytes(12), formatBytes(1536), formatBytes(1234567)]).toEqual(["12 B", "1.5 KB", "1.2 MB"]);
    const spec = makeSpec("s1", "Shop", "IN_PROGRESS", [makeTask("T02", "Cart", "IN_PROGRESS")]);
    const space = { activeSpec: spec, running: true } as unknown as SpaceData;
    expect(filesSummary({ space, readiness: null, projectCount: 1 }).subtitle).toBe("integrated code · T02 in progress");
    setLanguage("pt-BR", { persist: false });
    expect(formatBytes(1536)).toBe("1,5 KB");
    expect(filesSummary({ space, readiness: null, projectCount: 1 }).subtitle).toBe("código integrado · T02 em andamento");
  });
});

describe("Arquivos em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora", async () => {
    await mount("pt-BR");
    let win = files("pt-BR");
    await within(win).findByRole("tree", { name: "Arquivos do projeto" });
    expect(within(win).getByRole("button", { name: "Código integrado" })).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Mostrar ocultos" })).toBeTruthy();
    expect(win.querySelector("[data-files-placeholder]")?.textContent).toBe("Escolha um arquivo na árvore");
    expect(document.getElementById(windowSubtitleId("files"))?.textContent).toBe("código integrado");
    await open("gone.txt", "pt-BR");
    expect(within(win).getByRole("alert").textContent).toMatch(/^Não foi possível abrir o arquivo: /);
    expect(portugueseLeftovers(win).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    win = files("en");
    expect(within(win).getByRole("tree", { name: "Project files" })).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Integrated code" })).toBeTruthy();
    expect(within(win).getByRole("alert").textContent).toMatch(/^Could not open the file: /);
    expect(document.getElementById(windowSubtitleId("files"))?.textContent).toBe("integrated code");
    expect(portugueseLeftovers(win)).toEqual([]);
  });
});
