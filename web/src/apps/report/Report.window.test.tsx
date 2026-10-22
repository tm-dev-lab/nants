/*
 * Janela Relatório montada pelo shell (spec 093, T07): cabeçalho "Projeto concluído" com os fatos e
 * "Ver relatório", Markdown restrito com "Como executar" em destaque,
 * evidências, fatos sem relatório, relatório sendo escrito e o estado vazio.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecStatus } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { integratedCount, reportSummary } from "./summary";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const RESULT = {
  spec_id: "s1",
  title: "Loja online",
  status: "COMPLETED",
  tasks: [
    { id: "T01", title: "Base", status: "DONE" },
    { id: "T02", title: "Carrinho", status: "DONE" },
  ],
  commits: [{ hash: "8d0d36f", subject: "feat(s1): T02 - Carrinho", task_id: "T02", date: "2026-10-14T13:05:00Z" }],
  files: [{ path: "src/app.js", change: "A" }],
  files_truncated: false,
  readme: { text: "# Loja", truncated: false },
  project_path: "/home/u/NantsProjects/loja",
  test_command: "npm test",
};
const REPORT_MD = ["# Loja online", "", "Resumo do que foi entregue.", "", "## Como executar", "", "1. Rode `docker compose up`.", "", "## Decisões", "", "- Usou SQLite."].join("\n");

function state(status: SpecStatus, reportPending = false): ProjectState {
  return makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec("s1", "Loja online", status, [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "DONE")])], report_pending: reportPending });
}

/** Começa em execução e conclui depois de instalar as rotas (como na vida real: a spec termina). */
async function mount(s: ProjectState, report: () => { status: number; body?: unknown }, extra: Partial<RenderShellOptions> = {}) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state("IN_PROGRESS") }, ...extra });
  view.server.reply("GET", /\/specs\/s1\/result$/, () => ({ status: 200, body: RESULT }));
  view.server.reply("GET", /\/specs\/s1\/report$/, report);
  view.server.model.states.p1 = s;
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return view;
}

function reportWindow(): HTMLElement {
  return screen.getByRole("region", { name: "Relatório" });
}

describe("Relatório: conteúdo", () => {
  it("relatório pronto: 'Projeto concluído' com os fatos, 'Como executar' em destaque e evidências", async () => {
    await mount(state("COMPLETED"), () => ({ status: 200, body: { markdown: REPORT_MD, generated_at: "2026-10-14T13:05:00Z", source: "architect" } }));
    const win = reportWindow();
    const banner = within(win).getByRole("region", { name: "Projeto concluído" });
    await waitFor(() => expect(banner.querySelector("[data-completed-facts]")?.textContent).toContain("2 tarefas integradas"));
    expect(banner.textContent).toContain("testes: npm test");
    await waitFor(() => expect(within(win).getByRole("region", { name: "Como executar" }).textContent).toContain("docker compose up"));
    expect(within(win).getByRole("article", { name: "Conteúdo do relatório" }).textContent).toContain("Usou SQLite.");
    expect(within(win).getByText("Evidências")).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("s1 · concluída");
    // "Ver relatório" leva o foco ao relatório, dentro da janela.
    const view = within(banner).getByRole("button", { name: "Ver relatório" });
    // 095/T07, A-05: secundária (não primária) dentro da própria janela; a ação continua.
    expect(view.hasAttribute("data-variant")).toBe(false);
    expect(win.querySelectorAll('[data-variant="primary"]')).toHaveLength(0);
    fireEvent.click(view);
    expect(document.activeElement?.hasAttribute("data-report-body")).toBe(true);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("095/T06: sem caixa contornada; as evidências recolhidas abrem com tarefas, arquivos e commits", async () => {
    await mount(state("COMPLETED"), () => ({ status: 200, body: { markdown: REPORT_MD, generated_at: "2026-10-14T13:05:00Z", source: "architect" } }));
    const win = reportWindow();
    const banner = within(win).getByRole("region", { name: "Projeto concluído" });
    expect(banner.classList.contains("nd-card")).toBe(false);
    expect(win.querySelectorAll(".nd-card")).toHaveLength(0);
    const evidence = await waitFor(() => {
      const el = win.querySelector<HTMLDetailsElement>("details[data-evidence]");
      if (el === null) throw new Error("evidências ausentes");
      return el;
    });
    expect(evidence.open).toBe(false);
    const summary = evidence.querySelector("summary");
    if (summary === null) throw new Error("resumo ausente");
    fireEvent.click(summary);
    evidence.open = true;
    expect(within(evidence).getByRole("list", { name: "Tarefas" }).textContent).toContain("Carrinho");
    expect(within(evidence).getByRole("list", { name: "Arquivos" }).textContent).toContain("src/app.js");
    expect(evidence.textContent).toContain("8d0d36f");
  });

  it("sem relatório (404): os fatos do resultado (como executar pelo README)", async () => {
    await mount(state("COMPLETED"), () => ({ status: 404, body: { error: "sem relatório" } }));
    const win = reportWindow();
    await waitFor(() => expect(win.textContent).toContain("Pronto: 2 tarefas integradas"));
    expect(win.textContent).toContain("Trecho do início do README");
    expect(win.textContent).toContain("Testes: npm test");
  });

  it("'Abrir o App' abre a janela App", async () => {
    await mount(state("COMPLETED"), () => ({ status: 404 }));
    await act(async () => {
      fireEvent.click(within(reportWindow()).getByRole("button", { name: "Abrir o App" }));
    });
    await settle();
    const app = screen.getByRole("region", { name: "App" });
    expect(app.closest("[data-mode]")?.getAttribute("data-mode") ?? app.getAttribute("data-mode")).toBe("normal");
  });

  it("relatório sendo escrito: sem a faixa, com o aviso; subtítulo acompanha", async () => {
    await mount(state("COMPLETED", true), () => ({ status: 404 }));
    // Escrevendo o relatório, a fase é "finalizando" (layout de execução): abre pelo dock.
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Relatório" }));
    });
    await settle();
    const win = reportWindow();
    await waitFor(() => expect(win.textContent).toContain("Pronto: 2 tarefas integradas"));
    expect(within(win).queryByRole("region", { name: "Projeto concluído" })).toBeNull();
    expect(win.textContent).toContain("O Arquiteto está escrevendo o relatório");
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("o Arquiteto está escrevendo");
  });
});

describe("Relatório: vazio e resumo", () => {
  it("spec ainda em execução: estado vazio com 'Abrir o Quadro'", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state("IN_PROGRESS") } });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Relatório" }));
    });
    await settle();
    const win = reportWindow();
    expect(win.textContent).toContain("O relatório fica pronto quando a execução terminar.");
    expect(within(win).getByRole("button", { name: "Abrir o Quadro" })).toBeTruthy();
  });

  it("resumo puro e contagem de integradas", () => {
    expect(reportSummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    expect(integratedCount([{ status: "DONE" }, { status: "TODO" }, { status: "DONE" }])).toBe(2);
  });
});
