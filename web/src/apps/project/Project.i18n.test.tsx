/*
 * Janela Projeto nos dois idiomas (spec 102, T05): em en, nenhum texto visível ou acessível da
 * janela, do diálogo de remover e dos avisos da remoção fica em português; o caminho e o nome do
 * projeto aparecem como vieram; pt-BR igual a hoje; a troca de idioma vale na hora.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
  window.history.replaceState({}, "", "/");
});

type Lang = "pt-BR" | "en";
const PROJECT: Readonly<Record<Lang, string>> = { "pt-BR": "Projeto", en: "Project" };
const P1 = makeProject("p1", "Loja");
const P2 = makeProject("p2", "Blog");

async function open(lang: Lang, options: RenderShellOptions = {}) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1, P2],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
    ...options,
  });
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: PROJECT[lang] }));
  });
  await settle();
  return view;
}

const win = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: PROJECT[lang] });
const subtitle = (): string | null | undefined => document.getElementById(windowSubtitleId("project"))?.textContent;

function expectNoPortuguese(): void {
  expect(portugueseLeftovers(win())).toEqual([]);
  for (const el of [...screen.queryAllByRole("dialog"), ...screen.queryAllByRole("alertdialog")]) expect(portugueseLeftovers(el)).toEqual([]);
}

describe("Projeto em en", () => {
  it("seções, pasta, limites (validação, 409 e sucesso), modos e subtítulo", async () => {
    const { server } = await open("en");
    let reply: { status: number; body: unknown } = { status: 409, body: { error: "já existe uma execução em andamento neste projeto", code: "run_active" } };
    server.reply("PUT", /^\/api\/projects\/p1\/settings$/, () => reply);
    const w = win();
    expect([...w.querySelectorAll(".nd-project-section h3")].map((h) => h.textContent)).toEqual(["Project folder", "Limits", "Modes", "Remove from Nants"]);
    expect(w.querySelector("[data-project-path]")?.textContent).toBe("/home/dev/Loja");
    expect(within(w).getByRole("button", { name: "Copy the folder path" }).textContent).toBe("Copy");
    expect(w.textContent).toContain("Open the folder in your editor to see the code.");
    expect(w.textContent).toContain("With automatic approval, the agents go on without asking for your permission");
    expect(w.textContent).toContain("Only applies to planning and recovery.");
    expect(subtitle()).toBe("standard mode · 5 attempts");
    expectNoPortuguese();

    const form = within(w).getByRole("form", { name: "Project limits" });
    expect(form.textContent).toContain("Values from 1 to 10. Existing tasks keep their own limit.");
    fireEvent.change(within(form).getByLabelText("Attempts per task (new ones)"), { target: { value: "11" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save limits" }));
    });
    expect(within(form).getByRole("alert").textContent).toBe("Enter whole numbers from 1 to 10.");
    fireEvent.change(within(form).getByLabelText("Attempts per task (new ones)"), { target: { value: "1" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save limits" }));
    });
    await settle();
    expect(within(form).getByRole("alert").textContent).toBe("A run is in progress; try again when it ends.");
    expectNoPortuguese();
    reply = { status: 200, body: makeProjectState({ id: "p1", name: "Loja", max_attempts: 1, yolo: true }) };
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save limits" }));
    });
    await settle();
    expect(within(form).getByRole("status").textContent).toBe("Limits saved.");
    expect(within(form).getByLabelText("Test cycles per attempt")).toBeTruthy();
    expect(subtitle()).toBe("automatic approval · 1 attempt");
    expectNoPortuguese();
  });

  it("Remover do Nants: diálogo, erro do 409 traduzido e o aviso da remoção", async () => {
    const { server } = await open("en");
    let reply: { status: number; body: unknown } = { status: 409, body: { error: "já existe um turno de chat em andamento para este projeto", code: "chat_active" } };
    server.reply("DELETE", /^\/api\/projects\/p1$/, () => reply);
    fireEvent.click(within(win()).getByRole("button", { name: "Remove from Nants" }));
    const dialog = screen.getByRole("alertdialog", { name: "Remove the workspace from Nants?" });
    expect(dialog.textContent).toContain("Removes the workspace from Nants only. The files in /home/dev/Loja stay on disk.");
    expect(dialog.textContent).toContain("To use it again, add the folder again.");
    expect(within(dialog).getByRole("checkbox", { name: /Also delete the run data volumes \(Docker\)/ })).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeTruthy();
    expectNoPortuguese();
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Remove from Nants" }));
    });
    await settle();
    expect(within(dialog).getByRole("alert").textContent).toBe(
      "A conversation with the Architect is in progress in this project. Wait for the answer before removing.",
    );
    expectNoPortuguese();
    reply = { status: 200, body: { ok: true, removed: "p1" } };
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Remove from Nants" }));
    });
    await settle();
    expect(document.querySelector("[data-toast]")?.textContent).toContain('"Loja" left Nants. The folder stays on disk.');
  });
});

describe("Projeto em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora (seções, limites, subtítulo e diálogo)", async () => {
    await open("pt-BR");
    expect([...win("pt-BR").querySelectorAll(".nd-project-section h3")].map((h) => h.textContent)).toEqual([
      "Pasta do projeto",
      "Limites",
      "Modos",
      "Remover do Nants",
    ]);
    expect(subtitle()).toBe("modo padrão · 5 tentativas");
    fireEvent.click(within(win("pt-BR")).getByRole("button", { name: "Remover do Nants" }));
    expect(screen.getByRole("alertdialog", { name: "Remover workspace do Nants?" }).textContent).toContain("/home/dev/Loja permanecem no disco");
    expect(portugueseLeftovers(win("pt-BR")).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    expect([...win().querySelectorAll(".nd-project-section h3")].map((h) => h.textContent)).toEqual(["Project folder", "Limits", "Modes", "Remove from Nants"]);
    expect(subtitle()).toBe("standard mode · 5 attempts");
    expect(screen.getByRole("alertdialog", { name: "Remove the workspace from Nants?" }).textContent).toContain("/home/dev/Loja stay on disk");
    expectNoPortuguese();
  });
});
