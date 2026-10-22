/*
 * Relatório nos dois idiomas (spec 102, T04): em en, nenhum texto visível ou acessível da janela
 * fica em português (faixa "Projeto concluído", fatos, ações, vazio e subtítulo); o relatório
 * escrito pelo Arquiteto aparece como foi escrito, e o destaque da seção de execução vale nos dois
 * idiomas ("How to run" e "Como executar"); pt-BR igual a hoje; a troca vale na hora.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecStatus } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { reportSummary } from "./summary";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

type Lang = "pt-BR" | "en";
const NAME: Readonly<Record<Lang, string>> = { "pt-BR": "Relatório", en: "Report" };

const P1 = makeProject("p1", "Shop");
const RESULT = {
  spec_id: "s1",
  title: "Online shop",
  status: "COMPLETED",
  tasks: [
    { id: "T01", title: "Base", status: "DONE" },
    { id: "T02", title: "Cart", status: "DONE" },
  ],
  commits: [{ hash: "8d0d36f", subject: "feat(s1): T02 - Cart", task_id: "T02", date: "2026-10-14T13:05:00Z" }],
  files: [{ path: "src/app.js", change: "A" }],
  files_truncated: false,
  readme: { text: "# Shop", truncated: false },
  project_path: "/home/u/NantsProjects/shop",
  test_command: "npm test",
};
const REPORT_EN = ["# Online shop", "", "What was delivered.", "", "## How to run", "", "1. Run `docker compose up`.", "", "## Decisions", "", "- Used SQLite."].join("\n");
const REPORT_PT = ["# Loja online", "", "Resumo.", "", "## Como executar", "", "1. Rode `docker compose up`.", "", "## Decisões", "", "- Usou SQLite."].join("\n");

function state(status: SpecStatus, reportPending = false): ProjectState {
  return makeProjectState({
    id: "p1",
    name: "Shop",
    specs: [makeSpec("s1", "Online shop", status, [makeTask("T01", "Base", "DONE"), makeTask("T02", "Cart", "DONE")])],
    report_pending: reportPending,
  });
}

/** Começa em execução e conclui depois de instalar as rotas (a spec termina). */
async function mount(lang: Lang, s: ProjectState, report: () => { status: number; body?: unknown }) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state("IN_PROGRESS") } });
  view.server.reply("GET", /\/specs\/s1\/result$/, () => ({ status: 200, body: RESULT }));
  view.server.reply("GET", /\/specs\/s1\/report$/, report);
  view.server.model.states.p1 = s;
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return view;
}

const reportWindow = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: NAME[lang] });
const ready = (markdown: string) => () => ({ status: 200, body: { markdown, generated_at: "2026-10-14T13:05:00Z", source: "architect" } });

async function openByDock(lang: Lang): Promise<void> {
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: NAME[lang] }));
  });
  await settle();
}

describe("Relatório em en", () => {
  it("faixa, fatos, ações, 'How to run' em destaque e subtítulo sem português", async () => {
    await mount("en", state("COMPLETED"), ready(REPORT_EN));
    const win = reportWindow();
    const banner = within(win).getByRole("region", { name: "Project completed" });
    await waitFor(() => expect(banner.querySelector("[data-completed-facts]")?.textContent).toBe("2 tasks integrated · tests: npm test"));
    expect(within(banner).getByRole("button", { name: "View report" })).toBeTruthy();
    expect(within(banner).getByRole("button", { name: "Open the App" })).toBeTruthy();
    await waitFor(() => expect(within(win).getByRole("region", { name: "How to run" }).textContent).toContain("docker compose up"));
    // O relatório escrito aparece como foi escrito; a seção de execução sai do resto.
    const article = within(win).getByRole("article", { name: "Report content" });
    expect(article.textContent).toContain("Used SQLite.");
    expect(article.textContent).not.toContain("docker compose up");
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("s1 · completed");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("relatório em pt-BR com a interface em en: o destaque vale e o texto não é traduzido", async () => {
    await mount("en", state("COMPLETED"), ready(REPORT_PT));
    const win = reportWindow();
    await waitFor(() => expect(within(win).getByRole("region", { name: "How to run" }).textContent).toContain("Rode"));
    expect(within(win).getByRole("article", { name: "Report content" }).textContent).toContain("Usou SQLite.");
  });

  it("sem relatório: fatos e carregando; escrevendo: subtítulo; vazio antes da conclusão", async () => {
    await mount("en", state("COMPLETED", true), () => ({ status: 404 }));
    await openByDock("en");
    const win = reportWindow();
    await waitFor(() => expect(win.textContent).toContain("Done: 2 tasks integrated"));
    expect(within(win).queryByRole("region", { name: "Project completed" })).toBeNull();
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("the Architect is writing");
    expect(portugueseLeftovers(win)).toEqual([]);
    cleanup();

    setLanguage("en", { persist: false });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state("IN_PROGRESS") } });
    await openByDock("en");
    const empty = reportWindow();
    expect(empty.textContent).toContain("The report is ready once the run finishes.");
    expect(within(empty).getByRole("button", { name: "Open the Board" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("waiting for completion");
    expect(portugueseLeftovers(empty)).toEqual([]);
  });

  it("resumo puro no idioma ativo", () => {
    const space = { activeSpec: makeSpec("s1", "Shop", "COMPLETED", []), state: { report_pending: false } } as never;
    setLanguage("en", { persist: false });
    expect(reportSummary({ space, readiness: null, projectCount: 1 }).subtitle).toBe("s1 · completed");
    setLanguage("pt-BR", { persist: false });
    expect(reportSummary({ space, readiness: null, projectCount: 1 }).subtitle).toBe("s1 · concluída");
  });
});

describe("Relatório em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora", async () => {
    await mount("pt-BR", state("COMPLETED"), ready(REPORT_PT));
    let win = reportWindow("pt-BR");
    let banner = within(win).getByRole("region", { name: "Projeto concluído" });
    await waitFor(() => expect(banner.querySelector("[data-completed-facts]")?.textContent).toBe("2 tarefas integradas · testes: npm test"));
    expect(within(banner).getByRole("button", { name: "Ver relatório" })).toBeTruthy();
    await waitFor(() => expect(within(win).getByRole("region", { name: "Como executar" })).toBeTruthy());
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("s1 · concluída");

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    win = reportWindow("en");
    banner = within(win).getByRole("region", { name: "Project completed" });
    expect(banner.querySelector("[data-completed-facts]")?.textContent).toBe("2 tasks integrated · tests: npm test");
    expect(within(banner).getByRole("button", { name: "View report" })).toBeTruthy();
    expect(within(win).getByRole("region", { name: "How to run" }).textContent).toContain("Rode");
    expect(document.getElementById(windowSubtitleId("report"))?.textContent).toBe("s1 · completed");
    expect(portugueseLeftovers(banner)).toEqual([]);
  });
});
