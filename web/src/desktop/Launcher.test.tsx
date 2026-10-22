import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import { findForbiddenGlyphs } from "./glyphs";
import { filterLauncher, moveActive, normalizeQuery, type LauncherItem } from "./launcher";
import { renderShell, settle } from "./shell-test-utils";
import { useSpace } from "./shellContext";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog de Receitas");

const item = (id: string, group: LauncherItem["group"], label: string, extra: Partial<LauncherItem> = {}): LauncherItem => ({
  id,
  group,
  label,
  icon: "search",
  run: () => undefined,
  ...extra,
});

describe("launcher (modelo)", () => {
  it("busca sem acento nem caixa, todas as palavras, na ordem dos grupos", () => {
    const items = [
      item("p", "projects", "Configurações antigas"),
      item("w", "windows", "Configurações"),
      item("a", "actions", "Abrir as Configurações"),
      item("x", "windows", "Quadro"),
    ];
    expect(normalizeQuery("  Configurações ")).toBe("configuracoes");
    expect(filterLauncher(items, "CONFIGURACOES").map((i) => i.id)).toEqual(["a", "w", "p"]);
    expect(filterLauncher(items, "abrir config").map((i) => i.id)).toEqual(["a"]);
    expect(filterLauncher(items, "").map((i) => i.id)).toEqual(["a", "w", "x", "p"]);
  });

  it("itens 'always' nunca são filtrados", () => {
    const items = [item("ask", "actions", "Pedir ao Arquiteto: x", { always: true }), item("w", "windows", "Plano")];
    expect(filterLauncher(items, "zzz").map((i) => i.id)).toEqual(["ask"]);
  });

  it("setas circulam", () => {
    expect(moveActive(-1, 1, 3)).toBe(0);
    expect(moveActive(-1, -1, 3)).toBe(2);
    expect(moveActive(2, 1, 3)).toBe(0);
    expect(moveActive(0, -1, 3)).toBe(2);
    expect(moveActive(0, 1, 0)).toBe(-1);
  });
});

function launcher(): HTMLElement {
  return screen.getByRole("dialog", { name: "Lançador" });
}

function input(): HTMLInputElement {
  return within(launcher()).getByRole("combobox") as HTMLInputElement;
}

function activeOption(): HTMLElement | null {
  const id = input().getAttribute("aria-activedescendant");
  return id === null ? null : document.getElementById(id);
}

describe("Lançador (Ctrl+K)", () => {
  it("Ctrl+K abre; Esc fecha antes de qualquer outro atalho", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    expect(launcher()).toBeTruthy();
    expect(document.activeElement).toBe(input());
    fireEvent.keyDown(input(), { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Lançador" })).toBeNull();
    // As janelas continuam (Esc nunca fecha janela).
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("janelas com o atalho do dock (Alt+N), projetos e ações; setas e Enter executam", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1, P2],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog de Receitas" }) },
    });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const list = within(launcher()).getByRole("listbox");
    const conversa = within(list).getByRole("option", { name: /^Conversa/ });
    expect(conversa.querySelector("kbd")?.textContent).toBe("Alt+1");
    expect(conversa.getAttribute("aria-keyshortcuts")).toBe("Alt+1");
    expect(within(list).getByRole("option", { name: /Plano/ }).querySelector("kbd")?.textContent).toBe("Alt+2");
    expect(within(list).getByRole("option", { name: /Blog de Receitas/ })).toBeTruthy();
    expect(within(list).getByRole("option", { name: /Novo projeto/ })).toBeTruthy();
    expect(findForbiddenGlyphs(launcher())).toEqual([]);

    fireEvent.change(input(), { target: { value: "receitas" } });
    expect(activeOption()?.textContent).toContain("Blog de Receitas");
    fireEvent.keyDown(input(), { key: "Enter" });
    await settle();
    expect(window.location.search).toBe("?space=p2");
  });

  it("setas movem o destaque e Enter abre a janela escolhida", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "custos" } });
    expect(activeOption()?.textContent).toContain("Custos");
    fireEvent.keyDown(input(), { key: "ArrowDown" });
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(activeOption()?.textContent).toContain("Custos");
    fireEvent.keyDown(input(), { key: "Enter" });
    await settle(3);
    expect(screen.queryByRole("dialog", { name: "Lançador" })).toBeNull();
    expect(screen.getByRole("region", { name: "Custos" })).toBeTruthy();
  });

  it("'Primeiros passos' e 'Novo projeto' pelo lançador", async () => {
    await renderShell({ projects: [P1] });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "primeiros" } });
    fireEvent.keyDown(input(), { key: "Enter" });
    await settle(3);
    expect(screen.getByRole("region", { name: "Primeiros passos" })).toBeTruthy();
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "novo projeto" } });
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(screen.getByRole("dialog", { name: "Novo projeto" })).toBeTruthy();
  });

  it("aprovar o plano quando há plano pronto; parar quando há execução", async () => {
    const plan = makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "a", "TODO")]);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [plan] }) },
    });
    server.reply("POST", /\/approve$/, () => ({ status: 200, body: { started: true } }));
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "aprovar" } });
    expect(activeOption()?.textContent).toContain("Aprovar o plano");
    fireEvent.keyDown(input(), { key: "Enter" });
    await settle();
    expect(server.count("POST", /\/specs\/s1\/approve$/)).toBe(1);
  });

  it("'Parar a execução' abre a confirmação da pílula", async () => {
    const spec = makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T01", "a", "IN_PROGRESS")]);
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) },
    });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "parar" } });
    fireEvent.keyDown(input(), { key: "Enter" });
    await settle(3);
    expect(screen.getByRole("alertdialog", { name: "Parar a execução?" })).toBeTruthy();
  });

  it("'Pedir ao Arquiteto' entrega o texto à Conversa pelo contexto do espaço", async () => {
    function Probe() {
      const space = useSpace();
      return <p data-probe="">{space?.architectRequest?.text ?? ""}</p>;
    }
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      apps: { conversation: { Content: Probe, scroll: "app" } },
    });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    fireEvent.change(input(), { target: { value: "gere os planetas" } });
    const ask = within(launcher()).getByRole("option", { name: /Pedir ao Arquiteto: gere os planetas/ });
    await act(async () => {
      fireEvent.click(ask);
    });
    await settle(3);
    expect(document.querySelector("[data-probe]")?.textContent).toBe("gere os planetas");
  });
});
