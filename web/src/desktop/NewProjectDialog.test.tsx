import { cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { makeProject } from "../test-utils";
import { appStore } from "../store";
import { GIT_MISSING_MESSAGE, NAME_INVALID_MESSAGE, ROOT_INVALID_MESSAGE } from "./api";
import { findForbiddenGlyphs } from "./glyphs";
import { renderShell, settle } from "./shell-test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const created = (name: string, path: string) => ({
  id: "novo1",
  name,
  path,
  created_at: "2026-10-19T00:00:00Z",
  is_git_repo: true,
  initialized: true,
  git_initialized: true,
});

async function openDialog(options: Parameters<typeof renderShell>[0] = {}) {
  const shell = await renderShell({ projects: [makeProject("p1", "Loja")], ...options });
  fireEvent.click(within(document.querySelector<HTMLElement>("[data-welcome]") ?? document.body).getByRole("button", { name: "Novo projeto" }));
  const dialog = screen.getByRole("dialog", { name: "Novo projeto" });
  return { ...shell, dialog };
}

function typeName(dialog: HTMLElement, value: string): void {
  fireEvent.change(within(dialog).getByLabelText("Nome do projeto"), { target: { value } });
}

describe("Novo projeto", () => {
  it("por nome: envia só {name}; o nome exibido é o digitado e o espaço novo abre", async () => {
    const { server, dialog } = await openDialog();
    expect(document.activeElement).toBe(within(dialog).getByLabelText("Nome do projeto"));
    expect(findForbiddenGlyphs(dialog)).toEqual([]);
    server.reply("POST", /^\/api\/projects$/, (body) => {
      const name = (body as { name: string }).name;
      server.model.projects = [...server.model.projects, { id: "novo1", name, path: "/home/u/NantsProjects/loja-do-ze", created_at: "" }];
      return { status: 201, body: created(name, "/home/u/NantsProjects/loja-do-ze") };
    });
    typeName(dialog, "  Loja do Zé  ");
    fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    await settle();
    const post = server.calls.find((c) => c.method === "POST" && c.url === "/api/projects");
    expect(post?.body).toEqual({ name: "Loja do Zé" });
    expect(screen.queryByRole("dialog", { name: "Novo projeto" })).toBeNull();
    expect(window.location.search).toBe("?space=novo1");
    expect(appStore.getState().config?.projects.find((p) => p.id === "novo1")?.name).toBe("Loja do Zé");
    expect(document.querySelector("[data-toast]")?.textContent).toContain("Loja do Zé");
  });

  it("409 project_dir_exists: oferece 'Usar a pasta existente' com aquele caminho", async () => {
    const { server, dialog } = await openDialog();
    server.reply("POST", /^\/api\/projects$/, (body) => {
      const b = body as { name: string; path?: string };
      if (b.path === undefined) return { status: 409, body: { error: "a pasta do projeto já existe", code: "project_dir_exists", path: "/home/u/NantsProjects/blog" } };
      return { status: 201, body: created(b.name, b.path) };
    });
    typeName(dialog, "Blog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    await settle();
    const callout = dialog.querySelector<HTMLElement>("[data-dir-exists]");
    if (callout === null) throw new Error("aviso ausente");
    expect(callout.textContent).toContain("/home/u/NantsProjects/blog");
    fireEvent.click(within(callout).getByRole("button", { name: "Usar a pasta existente" }));
    await settle();
    const posts = server.calls.filter((c) => c.method === "POST" && c.url === "/api/projects");
    expect(posts.at(-1)?.body).toEqual({ name: "Blog", path: "/home/u/NantsProjects/blog" });
    expect(window.location.search).toBe("?space=novo1");
  });

  it("erros fixos: nome inválido (400), Git ausente (412) e pasta de projetos inválida (422)", async () => {
    const { server, dialog } = await openDialog();
    const replies = [
      { status: 400, body: { error: "nome inválido: use letras ou números", code: "project_name_invalid" } },
      { status: 412, body: { error: "prereq_missing", missing: ["git"] } },
      { status: 422, body: { error: "pasta de projetos inválida: diretório de sistema não é permitido", code: "projects_root_system" } },
    ];
    server.reply("POST", /^\/api\/projects$/, () => replies.shift() ?? { status: 500, body: {} });
    typeName(dialog, "!!!");
    for (const expected of [NAME_INVALID_MESSAGE, GIT_MISSING_MESSAGE, ROOT_INVALID_MESSAGE]) {
      fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
      await settle();
      expect(within(dialog).getByRole("alert").textContent).toBe(expected);
    }
  });

  it("200 para projeto já existente é idempotente: entra no espaço sem duplicar no store", async () => {
    const { server, dialog } = await openDialog();
    server.reply("POST", /^\/api\/projects$/, () => ({ status: 200, body: { ...created("Loja", "/home/dev/Loja"), id: "p1" } }));
    typeName(dialog, "Loja");
    fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    await settle();
    expect(appStore.getState().config?.projects.filter((p) => p.id === "p1")).toHaveLength(1);
    expect(window.location.search).toBe("?space=p1");
  });

  it("pasta existente: 409 (pasta dentro de outro repositório) e 403 mostram a mensagem fixa e mantêm o diálogo", async () => {
    const { server, dialog } = await openDialog();
    server.reply("GET", /^\/api\/fs\/browse/, () => ({ status: 200, body: { path: "/home/u", parent: "/home", truncated: false, entries: [] } }));
    const replies = [
      { status: 409, body: { error: "a pasta está dentro de outro repositório Git" } },
      { status: 403, body: { error: "caminho fora do permitido" } },
    ];
    server.reply("POST", /^\/api\/projects$/, () => replies.shift() ?? { status: 500, body: {} });
    fireEvent.click(within(dialog).getByRole("button", { name: "Usar uma pasta existente" }));
    await settle();
    fireEvent.change(within(dialog).getByLabelText(/Pasta do projeto/), { target: { value: "/home/u/repo/sub" } });
    typeName(dialog, "Sub");
    const messages: string[] = [];
    for (let i = 0; i < 2; i += 1) {
      fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
      await settle();
      messages.push(within(dialog).getByRole("alert").textContent ?? "");
    }
    expect(messages.every((m) => m.length > 0)).toBe(true);
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
    expect(server.count("POST", /^\/api\/projects$/)).toBe(2);
  });

  it("nome vazio não envia", async () => {
    const { server, dialog } = await openDialog();
    fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    await settle();
    expect(server.count("POST", /^\/api\/projects$/)).toBe(0);
    expect(dialog.querySelector(".nd-field-error")?.textContent).toMatch(/^Informe/);
  });

  it("navegador de pastas: subpasta, pasta anterior, caminho atual, ocultos e erro de listagem", async () => {
    const { server, dialog } = await openDialog();
    const listing = (path: string) => ({
      path,
      parent: path === "/" ? null : path.split("/").slice(0, -1).join("/") || "/",
      truncated: false,
      entries: [{ name: "app", path: `${path === "/" ? "" : path}/app`, is_git_repo: false }],
    });
    server.reply("GET", /^\/api\/fs\/browse/, (_body, url) => {
      const params = new URL(url, "http://x").searchParams;
      if (params.get("path") === "/quebra") return { status: 500, body: { error: "falha interna", code: "internal" } };
      return { status: 200, body: listing(params.get("path") ?? "/home/u") };
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Usar uma pasta existente" }));
    await settle();
    const browser = dialog.querySelector<HTMLElement>("[data-folder-browser]");
    if (browser === null) throw new Error("navegador ausente");
    const browses = () => server.calls.filter((c) => c.url.startsWith("/api/fs/browse")).map((c) => c.url);
    expect(browses()).toEqual(["/api/fs/browse"]);

    fireEvent.click(within(browser).getByRole("button", { name: "Abrir pasta app" }));
    await settle();
    expect(browses().at(-1)).toBe("/api/fs/browse?path=%2Fhome%2Fu%2Fapp");

    fireEvent.click(within(browser).getByRole("button", { name: "Pasta anterior" }));
    await settle();
    expect(browses().at(-1)).toBe("/api/fs/browse?path=%2Fhome%2Fu");

    const crumbs = within(browser).getByRole("navigation", { name: "Caminho atual" });
    expect(within(crumbs).getAllByRole("button").map((b) => b.textContent)).toEqual(["/", "home", "u"]);
    fireEvent.click(within(crumbs).getByRole("button", { name: "home" }));
    await settle();
    expect(browses().at(-1)).toBe("/api/fs/browse?path=%2Fhome");

    fireEvent.click(within(browser).getByRole("checkbox", { name: "Mostrar ocultos" }));
    await settle();
    expect(browses().at(-1)).toBe("/api/fs/browse?path=%2Fhome&hidden=true");

    server.reply("GET", /^\/api\/fs\/browse/, () => ({ status: 500, body: { error: "corpo cru do servidor" } }));
    fireEvent.click(within(browser).getByRole("button", { name: "Abrir pasta app" }));
    await settle();
    const alert = within(browser).getByRole("alert");
    expect(alert.textContent).not.toContain("corpo cru");
    expect(alert.textContent?.length).toBeGreaterThan(0);
  });

  it("avançado: 'Usar uma pasta existente' abre o navegador de pastas e envia {name, path}", async () => {
    const { server, dialog } = await openDialog();
    server.reply("GET", /^\/api\/fs\/browse/, () => ({
      status: 200,
      body: {
        path: "/home/u",
        parent: "/home",
        truncated: false,
        entries: [
          { name: "app", path: "/home/u/app", is_git_repo: true },
          { name: "solta", path: "/home/u/solta", is_git_repo: false },
        ],
      },
    }));
    server.reply("POST", /^\/api\/projects$/, (body) => {
      const b = body as { name: string; path: string };
      return { status: 201, body: created(b.name, b.path) };
    });
    const toggle = within(dialog).getByRole("button", { name: "Usar uma pasta existente" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(toggle);
    await settle();
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const browser = dialog.querySelector<HTMLElement>("[data-folder-browser]");
    if (browser === null) throw new Error("navegador ausente");
    expect(within(browser).getByText("Repositório Git")).toBeTruthy();
    fireEvent.click(within(browser).getByRole("button", { name: "Abrir pasta app" }));
    await settle();
    fireEvent.change(within(dialog).getByLabelText(/Pasta do projeto/), { target: { value: "/home/u/solta" } });
    expect(dialog.querySelector("[data-no-git-note]")?.textContent).toBe("Esta pasta não usa Git; o Nants vai inicializar um repositório.");
    fireEvent.change(within(dialog).getByLabelText(/Pasta do projeto/), { target: { value: "/home/u/app" } });
    expect(dialog.querySelector("[data-no-git-note]")).toBeNull();
    fireEvent.click(within(browser).getByRole("button", { name: "Usar esta pasta" }));
    typeName(dialog, "App");
    fireEvent.click(within(dialog).getByRole("button", { name: "Criar projeto" }));
    await settle();
    const post = server.calls.filter((c) => c.method === "POST" && c.url === "/api/projects").at(-1);
    expect(post?.body).toEqual({ name: "App", path: "/home/u" });
    expect(findForbiddenGlyphs(document.body)).toEqual([]);
  });

  it("aberto pelo seletor de espaços", async () => {
    await renderShell({ projects: [makeProject("p1", "Loja")] });
    fireEvent.click(screen.getByRole("button", { name: /Trocar de projeto/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Novo projeto" }));
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
    fireEvent.keyDown(screen.getByRole("dialog", { name: "Novo projeto" }), { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Novo projeto" })).toBeNull();
  });
});
