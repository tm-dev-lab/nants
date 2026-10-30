/*
 * Spec 130 T05: a janela App segue o que o programa faz (mockup aprovado na T01), com os dados das
 * T02 a T04 num servidor falso (nenhuma rede nem Docker reais):
 * - rodando sem página: selo, porta, comando, registro ao vivo e Parar; `app_mode_changed` leva à
 *   página;
 * - terminou: o comando, o código, a saída, os arquivos (Abrir nos Arquivos e Copiar), o que foi
 *   gravado fora da pasta, Rodar de novo e Abrir no Console; fechar a janela para o app;
 * - o ambiente não subiu: o fim do registro com a hora, "Pedir ao Arquiteto para corrigir" (leva à
 *   Conversa) e Tentar de novo;
 * - não roda no sandbox: a explicação do Arquiteto, o Como usar, as provas e o Console;
 * - o Console: confirmação com o comando exato, a saída ao vivo, o código e a duração, o Parar, o
 *   histórico, os arquivos e os erros pelo `code`; sem `run_command`, o Console abre sozinho; no
 *   celular, as abas Saída, Como usar e Provas;
 * - a Execução: a pílula pelo modo e "Abrir o Console".
 */
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { publishConsoleEvent, isConsoleEvent } from "../../api/consoleEvents";
import { parseApiEvent } from "../../api/events";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { settle } from "../../desktop/shell-test-utils";
import { WindowLayoutContext } from "../../desktop/useWindowVisibility";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { resetStore } from "../../test-utils";
import { AppRunner } from "./AppApp";
import { clearGuideCache } from "./useAppGuide";
import { ExecutionPanel } from "../execution/ExecutionApp";
import { CONSOLE_EMPTY, S, STOP_URL, appWindow, calls, clickApp, emit, execWindow, guide, mountRun, running } from "./app-test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

const FINISHED = {
  state: "exited",
  mode: "finished",
  spec_id: S,
  url: null,
  started_at: "2026-10-27T12:00:00Z",
  exit_code: 0,
  command: "ledger report --month 2026-09 --sample",
  ports: [],
  notices: [],
  finished: {
    exit_code: 0,
    output: "Imported 3 sample files\nWrote reports/2026-09.md\n",
    files: [
      { path: "reports/2026-09.md", change: "added" },
      { path: "ledger.db", change: "changed" },
    ],
    files_total: 2,
    files_truncated: false,
    outside_work: ["/root/.cache/ledger"],
    outside_work_total: 1,
    outside_work_truncated: false,
  },
};

const RUNNING_NO_PAGE = running({ mode: "running", command: "python -m worker", embeddable: null });

const FAILURE = {
  spec_id: S,
  code: "app_docker_up_failed",
  detail: "#5 [2/3] RUN apt-get install -y libpq-dev\nE: Unable to locate package libpq-dev\nfailed to solve: process exited with code 100",
  at: "2026-10-27T12:04:00Z",
  action: "ask_architect_fix",
};

const STOPPED_WITH_FAILURE = { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null, failure: FAILURE };

function view(kind: string): HTMLElement {
  const el = appWindow().querySelector<HTMLElement>(`[data-runview="${kind}"]`);
  if (el === null) throw new Error(`forma ${kind} ausente`);
  return el;
}

function consoleEvent(type: string, payload: Record<string, unknown>): void {
  const event = parseApiEvent({ event_type: type, project_id: "p1", payload });
  if (event === null || !isConsoleEvent(event)) throw new Error("evento do Console inválido");
  act(() => publishConsoleEvent(event));
}

async function click(el: HTMLElement): Promise<void> {
  await act(async () => {
    fireEvent.click(el);
  });
  await settle();
}

describe("rodando sem página", () => {
  it("selo, porta, comando, registro ao vivo e Parar; sem iframe", async () => {
    const mounted = await mountRun({ app: { status: RUNNING_NO_PAGE, logs: "12:04:01 worker started\n12:04:03 order 1041 received" } });
    const v = view("running");
    expect(within(v).getByRole("status").textContent).toBe("Rodando sem página");
    expect(v.querySelector("[data-runview-port]")?.textContent).toContain(`${window.location.hostname}:18080`);
    expect(v.querySelector("[data-runview-command]")?.textContent).toContain("python -m worker");
    expect(within(v).getByRole("log").textContent).toContain("order 1041 received");
    expect(v.querySelector("[data-runview-live]")?.textContent).toBe("ao vivo");
    expect(appWindow().querySelector("iframe")).toBeNull();
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
    await click(within(v).getByRole("button", { name: "Parar" }));
    expect(calls(mounted, STOP_URL)).toBe(1);
  });

  it("app_mode_changed para page: relê o estado e mostra a página", async () => {
    const mounted = await mountRun({ app: { status: RUNNING_NO_PAGE } });
    expect(view("running")).toBeTruthy();
    mounted.app.status = running({ mode: "page" });
    emit("app_mode_changed", { spec_id: S, mode: "page", previous: "running", exit_code: null });
    await settle();
    expect(appWindow().querySelector("[data-runview]")).toBeNull();
    expect(appWindow().querySelector("iframe")).not.toBeNull();
  });

  it("app_mode_changed para finished: mostra o terminou com a saída relida", async () => {
    const mounted = await mountRun({ app: { status: RUNNING_NO_PAGE } });
    mounted.app.status = FINISHED;
    emit("app_mode_changed", { spec_id: S, mode: "finished", previous: "running", exit_code: 0 });
    emit("app_exited", { code: 0, reason: "exited", mode: "finished" });
    await settle();
    expect(within(view("finished")).getByRole("log").textContent).toContain("Wrote reports/2026-09.md");
  });
});

describe("terminou", () => {
  it("comando, código, saída, arquivos e o que ficou fora da pasta", async () => {
    await mountRun({ app: { status: FINISHED } });
    const v = view("finished");
    expect(within(v).getByRole("status").textContent).toBe("Terminou · código 0");
    expect(v.querySelector("[data-runview-command]")?.textContent).toBe("$ ledger report --month 2026-09 --sample");
    expect(within(v).getByRole("log", { name: "Saída do programa" }).textContent).toContain("Imported 3 sample files");
    const files = v.querySelector<HTMLElement>("[data-changed-files]");
    if (files === null) throw new Error("arquivos ausentes");
    expect([...files.querySelectorAll("[data-changed-file]")].map((el) => el.getAttribute("data-changed-file"))).toEqual(["reports/2026-09.md", "ledger.db"]);
    expect(within(files).getAllByText("novo")).toHaveLength(1);
    expect(within(files).getAllByText("mudou")).toHaveLength(1);
    expect(v.querySelector("[data-outside-work]")?.textContent).toContain("/root/.cache/ledger");
    expect(v.textContent).toContain("O programa rodou e terminou sem erro");
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
  });

  it("código diferente de zero: selo e frase próprios", async () => {
    await mountRun({ app: { status: { ...FINISHED, exit_code: 2, finished: { ...FINISHED.finished, exit_code: 2 } } } });
    const v = view("finished");
    expect(within(v).getByRole("status").textContent).toBe("Terminou · código 2");
    expect(v.textContent).toContain("O programa terminou com o código 2.");
  });

  it("Abrir leva o arquivo aos Arquivos; Copiar copia o caminho", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    const mounted = await mountRun({ app: { status: FINISHED } });
    const v = view("finished");
    await click(within(v).getByRole("button", { name: "Copiar o caminho reports/2026-09.md" }));
    expect(writeText).toHaveBeenCalledWith("reports/2026-09.md");
    await click(within(v).getByRole("button", { name: "Abrir reports/2026-09.md nos Arquivos" }));
    const files = screen.getByRole("region", { name: "Arquivos" });
    expect(files).toBeTruthy();
    await settle();
    expect(mounted.server.calls.length).toBeGreaterThan(0);
    expect(mounted.fetchMock.mock.calls.map((c) => String(c[0])).some((u) => u.includes("/file?") && u.includes("reports%2F2026-09.md"))).toBe(true);
  });

  it("Rodar de novo abre a confirmação na Execução; Abrir no Console mostra o Console", async () => {
    await mountRun({ app: { status: FINISHED } });
    await click(within(view("finished")).getByRole("button", { name: "Rodar de novo" }));
    expect(screen.getByRole("dialog", { name: "Iniciar o app?" })).toBeTruthy();
    await click(within(screen.getByRole("dialog", { name: "Iniciar o app?" })).getByRole("button", { name: "Cancelar" }));
    await click(within(view("finished")).getByRole("button", { name: "Abrir no Console" }));
    expect(appWindow().querySelector("[data-console]")).not.toBeNull();
    await click(within(appWindow()).getByRole("button", { name: "Voltar" }));
    expect(view("finished")).toBeTruthy();
  });

  it("fechar a janela de um terminou para o app (o container encerrado sai)", async () => {
    const mounted = await mountRun({ app: { status: FINISHED } });
    expect(calls(mounted, STOP_URL)).toBe(0);
    await click(within(appWindow()).getByRole("button", { name: "Fechar" }));
    expect(calls(mounted, STOP_URL)).toBe(1);
  });

  it("fechar a janela com o app parado não chama o parar", async () => {
    const mounted = await mountRun();
    await click(within(appWindow()).getByRole("button", { name: "Fechar" }));
    expect(calls(mounted, STOP_URL)).toBe(0);
  });
});

describe("o ambiente não subiu", () => {
  it("título, texto, hora, fim do registro, Pedir ao Arquiteto (leva à Conversa) e Tentar de novo", async () => {
    const mounted = await mountRun({ app: { status: STOPPED_WITH_FAILURE } });
    const v = view("failure");
    expect(within(v).getByRole("heading", { level: 2 }).textContent).toBe("O ambiente Docker do projeto não subiu");
    expect(within(v).getByRole("status").textContent).toBe("O ambiente não subiu");
    expect(v.querySelector("[data-failure-at]")?.textContent).toMatch(/^Falha às /);
    expect(v.querySelector("[data-failure-log]")?.textContent).toContain("Unable to locate package libpq-dev");
    expect(v.textContent).toContain("a mudança passa pela sua aprovação");
    await click(within(v).getByRole("button", { name: "Pedir ao Arquiteto para corrigir" }));
    const fix = mounted.server.calls.find((c) => c.url.endsWith("/app/fix"));
    expect(fix?.method).toBe("POST");
    expect(fix?.body).toEqual({ confirm: true, spec_id: S });
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
    await click(within(view("failure")).getByRole("button", { name: "Tentar de novo" }));
    expect(screen.getByRole("dialog", { name: "Iniciar o app?" })).toBeTruthy();
  });

  it("o pedido ao Arquiteto que falha mostra o motivo junto do botão", async () => {
    await mountRun({ app: { status: STOPPED_WITH_FAILURE, fix: { status: 409, body: { error: "x", code: "app_fix_no_failure", action: "retry" } } } });
    await click(within(view("failure")).getByRole("button", { name: "Pedir ao Arquiteto para corrigir" }));
    const problem = appWindow().querySelector("[data-fix-problem]");
    expect(problem?.textContent).not.toBe("");
  });

  it("a partida que falha relê o estado e mostra a falha guardada", async () => {
    const mounted = await mountRun({
      app: { start: { status: 422, body: { error: "x", code: "app_docker_up_failed", reason: "docker_failed", detail: "boom", action: "ask_architect_fix" } } },
      open: ["App", "Execução"],
    });
    mounted.app.status = STOPPED_WITH_FAILURE;
    await click(within(execWindow()).getByRole("button", { name: "Iniciar" }));
    await click(within(screen.getByRole("dialog", { name: "Iniciar o app?" })).getByRole("button", { name: "Executar" }));
    expect(view("failure")).toBeTruthy();
    // A Execução também oferece o pedido ao Arquiteto.
    expect(within(execWindow()).getByRole("button", { name: "Pedir ao Arquiteto para corrigir" })).toBeTruthy();
  });
});

describe("não roda no sandbox", () => {
  it("a explicação do Arquiteto, como rodar no computador, as provas e o Console", async () => {
    await mountRun({ app: { guide: { status: 200, body: guide({ sandbox: { runs_here: false, note: "Abre uma janela nativa de desktop (Tk)." } }) } } });
    const v = view("sandbox");
    expect(within(v).getByRole("status").textContent).toBe("Não roda aqui");
    expect(v.querySelector("[data-sandbox-note]")?.textContent).toBe("Abre uma janela nativa de desktop (Tk).");
    expect(v.querySelector("[data-how-to-use]")?.textContent).toContain("Como rodar no seu computador");
    expect(v.querySelector("[data-how-to-use]")?.textContent).toContain("ledger report --month 2026-09");
    const proofs = v.querySelector<HTMLElement>("[data-proofs]");
    expect(proofs?.textContent).toContain("7 de 7 tarefas concluídas");
    expect(proofs?.textContent).toContain("Verificação final aprovada");
    await click(within(v).getByRole("button", { name: "Abrir o Console do projeto" }));
    expect(appWindow().querySelector("[data-console]")).not.toBeNull();
  });
});

describe("Console do projeto", () => {
  it("abre pelo estado parado, com Como usar, provas, histórico e o limite de tempo", async () => {
    const history = [{ exec_id: "h1", spec_id: S, command: "ledger import samples/card.csv", redacted: false, origin: { device: { id: "d1", name: "iPhone" }, loopback: false }, at: "2026-10-27T12:00:00Z", status: "exited", exit_code: 0 }];
    await mountRun({ app: { console: { ...CONSOLE_EMPTY, history } } });
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    const c = appWindow().querySelector<HTMLElement>("[data-console]");
    if (c === null) throw new Error("Console ausente");
    expect(within(c).getByRole("heading", { level: 2 }).textContent).toBe("Console");
    expect(c.textContent).toContain("s1 · container do ambiente aprovado");
    expect(c.querySelector("[data-how-to-use]")?.textContent).toContain("ledger report --month 2026-09");
    expect(c.querySelector("[data-proofs]")?.textContent).toContain("7 de 7 tarefas concluídas");
    expect(c.textContent).toContain("até 5 minutos");
    const hist = c.querySelector<HTMLElement>("[data-console-history]");
    if (hist === null) throw new Error("histórico ausente");
    expect(hist.textContent).toContain("iPhone");
    await click(within(hist).getByRole("button", { name: "Usar o comando ledger import samples/card.csv" }));
    expect((within(c).getByRole("textbox", { name: "Comando" }) as HTMLInputElement).value).toBe("ledger import samples/card.csv");
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
  });

  it("cada comando passa pela confirmação com o texto exato; Cancelar não envia", async () => {
    const mounted = await mountRun();
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    const input = within(appWindow()).getByRole("textbox", { name: "Comando" });
    expect((within(appWindow()).getByRole("button", { name: "Rodar" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(input, { target: { value: "ledger report --month 2026-09" } });
    await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
    const dialog = screen.getByRole("dialog", { name: "Rodar este comando?" });
    expect(dialog.querySelector("[data-console-command]")?.textContent).toBe("ledger report --month 2026-09");
    await click(within(dialog).getByRole("button", { name: "Cancelar" }));
    expect(mounted.server.calls.filter((c) => c.method === "POST" && c.url.endsWith("/app/exec"))).toHaveLength(0);
    await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
    await click(within(screen.getByRole("dialog", { name: "Rodar este comando?" })).getByRole("button", { name: "Rodar" }));
    const posts = mounted.server.calls.filter((c) => c.method === "POST" && c.url.endsWith("/app/exec"));
    expect(posts).toHaveLength(1);
    expect(posts[0]?.body).toEqual({ confirm: true, spec_id: S, command: "ledger report --month 2026-09" });
  });

  it("saída ao vivo, código e duração, Parar e os arquivos do comando", async () => {
    const mounted = await mountRun();
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    fireEvent.change(within(appWindow()).getByRole("textbox", { name: "Comando" }), { target: { value: "ls" } });
    await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
    await click(within(screen.getByRole("dialog", { name: "Rodar este comando?" })).getByRole("button", { name: "Rodar" }));
    consoleEvent("app_exec_started", { exec_id: "e1", spec_id: S, command: "ls", started_at: "2026-10-27T12:00:00Z", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_output", { exec_id: "e1", seq: 1, text: "Wrote reports/2026-09.md\n", truncated: false });
    consoleEvent("app_exec_output", { exec_id: "e1", seq: 1, text: "Wrote reports/2026-09.md\n", truncated: false });
    await settle();
    const out = within(appWindow()).getByRole("log", { name: "Saída do Console" });
    expect(out.textContent).toContain("$ ls");
    expect(out.textContent?.match(/Wrote reports/g)).toHaveLength(1);
    expect(out.querySelector("[data-console-result]")?.textContent).toBe("rodando");
    await click(within(appWindow()).getByRole("button", { name: "Parar o comando" }));
    expect(mounted.server.calls.some((c) => c.method === "POST" && c.url.endsWith("/app/exec/stop"))).toBe(true);
    const last = { exec_id: "e1", spec_id: S, command: "ls", status: "exited", exit_code: 0, duration_ms: 900, output: "Wrote reports/2026-09.md\n", files: [{ path: "reports/2026-09.md", change: "added" }], files_total: 1 };
    mounted.app.console = { ...CONSOLE_EMPTY, last };
    consoleEvent("app_exec_finished", { exec_id: "e1", status: "exited", exit_code: 0, duration_ms: 900, files: [{ path: "reports/2026-09.md", change: "added" }], files_total: 1, notice: null });
    await settle();
    expect(out.querySelector("[data-console-result]")?.textContent).toBe("código 0 · 0,9 s");
    expect(appWindow().querySelector('[data-console] [data-changed-file="reports/2026-09.md"]')).not.toBeNull();
    expect(within(appWindow()).getByRole("button", { name: "Rodar" })).toBeTruthy();
  });

  it("tempo estourado traz o aviso do dicionário; erro do POST pelo code", async () => {
    await mountRun({ app: { exec: { status: 409, body: { error: "x", code: "app_exec_busy", action: "stop_command" } } } });
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    fireEvent.change(within(appWindow()).getByRole("textbox", { name: "Comando" }), { target: { value: "python -i" } });
    await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
    await click(within(screen.getByRole("dialog", { name: "Rodar este comando?" })).getByRole("button", { name: "Rodar" }));
    expect(appWindow().querySelector("[data-console-problem]")?.textContent).toBe("já há um comando rodando no Console deste projeto; espere o fim dele ou pare-o");
    consoleEvent("app_exec_started", { exec_id: "e9", spec_id: S, command: "python -i", started_at: "2026-10-27T12:00:00Z", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_finished", { exec_id: "e9", status: "timed_out", exit_code: null, duration_ms: 300000, notice: { code: "app_exec_timeout", params: { seconds: 300 }, action: "edit_command" } });
    await settle();
    const out = within(appWindow()).getByRole("log", { name: "Saída do Console" });
    expect(out.querySelector("[data-console-result]")?.textContent).toBe("passou do tempo · 5 min 0 s");
    expect(out.querySelector("[data-console-notice]")?.textContent).toContain("O Console não tem entrada");
  });

  it("código 127: diz qual comando não existe na imagem e sugere pedir a ferramenta ao Arquiteto", async () => {
    await mountRun();
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    const run = async (execId: string, command: string, output: string, code: number): Promise<HTMLElement> => {
      fireEvent.change(within(appWindow()).getByRole("textbox", { name: "Comando" }), { target: { value: command } });
      await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
      await click(within(screen.getByRole("dialog", { name: "Rodar este comando?" })).getByRole("button", { name: "Rodar" }));
      consoleEvent("app_exec_started", { exec_id: execId, spec_id: S, command, started_at: "2026-10-27T12:00:00Z", timeout_secs: 300, origin: {} });
      if (output !== "") consoleEvent("app_exec_output", { exec_id: execId, seq: 1, text: output, truncated: false });
      consoleEvent("app_exec_finished", { exec_id: execId, status: "exited", exit_code: code, duration_ms: 100, files: [], files_total: 0, notice: null });
      await settle();
      const entry = appWindow().querySelector<HTMLElement>(`[data-console-entry="${execId}"]`);
      if (entry === null) throw new Error(`entrada ${execId} ausente`);
      return entry;
    };
    // A ferramenta ausente numa cadeia: o nome vem da saída do shell.
    const missing = await run("n1", "cd api && curl -s http://localhost:8080/health", "sh: 1: curl: not found\n", 127);
    expect(missing.querySelector("[data-console-result]")?.textContent).toBe("código 127 · 0,1 s");
    expect(missing.querySelector('[data-console-notice="command_not_found"]')?.textContent).toBe(
      "O comando curl não existe na imagem do projeto (falta a ferramenta ou o nome foi digitado errado). Se o projeto precisa dele, peça ao Arquiteto para incluir a ferramenta no ambiente.",
    );
    // Erro de digitação sem saída: a primeira palavra do comando.
    const typo = await run("n2", "nmp test", "", 127);
    expect(typo.querySelector('[data-console-notice="command_not_found"]')?.textContent).toContain("O comando nmp não existe na imagem do projeto");
    // Outro código não traz a mensagem.
    const other = await run("n3", "false", "", 1);
    expect(other.querySelector("[data-console-notice]")).toBeNull();
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
  });

  it("sem run_command (próximo passo open_console), o Console abre sozinho nas duas janelas, sem o erro", async () => {
    await mountRun({
      app: { start: { status: 422, body: { error: "x", code: "app_no_run_command", reason: "no_run_command", action: "open_console" } } },
      open: ["App", "Execução"],
    });
    await click(within(execWindow()).getByRole("button", { name: "Iniciar" }));
    await click(within(screen.getByRole("dialog", { name: "Iniciar o app?" })).getByRole("button", { name: "Executar" }));
    expect(appWindow().querySelector("[data-console]")).not.toBeNull();
    // Spec 131 T05: a Execução também abre no Console, sem "Iniciar" nem o erro vermelho.
    expect(execWindow().querySelector("[data-console]")).not.toBeNull();
    for (const w of [appWindow(), execWindow()]) {
      expect(within(w).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
      expect(w.textContent).not.toContain("não declara como executar");
    }
  });

  // Spec 131 T05 (U2 do Test 9): a spec sem run_command, com a decisão "nada a executar".
  const NO_RUN_GUIDE = guide({ run: { declared: false, nothing_to_run: "Biblioteca: o uso é pelos testes e pelo README." } });

  it("sem run_command: App e Execução abrem no Console com o Como usar e as provas, sem Iniciar nem erro", async () => {
    const mounted = await mountRun({ app: { guide: { status: 200, body: NO_RUN_GUIDE } }, open: ["App", "Execução"] });
    for (const w of [appWindow(), execWindow()]) {
      const c = w.querySelector<HTMLElement>("[data-console]");
      if (c === null) throw new Error("Console ausente");
      expect(c.querySelector("[data-console-norun]")?.textContent).toContain("não declara um comando para iniciar o produto");
      expect(c.querySelector("[data-console-decision]")?.textContent).toBe("Biblioteca: o uso é pelos testes e pelo README.");
      expect(c.textContent).toContain("ledger report --month 2026-09");
      expect(c.querySelector("[data-proofs]")?.textContent).toContain("7 de 7");
      expect(within(w).getByRole("textbox", { name: "Comando" })).toBeTruthy();
      expect(within(w).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
      expect(w.querySelector("[data-console-start], [data-console-back]")).toBeNull();
      expect(within(w).queryByRole("alert")).toBeNull();
      expect(findForbiddenGlyphs(w)).toEqual([]);
    }
    expect(appWindow().querySelector("[data-app-view]")?.getAttribute("data-app-view")).toBe("console");
    expect(calls(mounted, /\/app\/start$/)).toBe(0);
  });

  it("sem run_command e sem a decisão: o Console abre igual, só com a linha do dicionário (en)", async () => {
    await mountRun({ app: { guide: { status: 200, body: guide({ run: { declared: false, nothing_to_run: null } }) } }, open: ["App", "Execução"] });
    const exec = execWindow();
    await act(async () => {
      setLanguage("en", { persist: false });
    });
    const c = screen.getByRole("region", { name: "App" }).querySelector<HTMLElement>("[data-console]");
    if (c === null) throw new Error("Console ausente");
    expect(c.querySelector("[data-console-norun]")?.textContent).toBe("This specification does not declare a command to start the product: it is used through Console commands.");
    expect(c.querySelector("[data-console-decision]")).toBeNull();
    expect(within(exec).queryByRole("button", { name: /^Start/ })).toBeNull();
    expect(exec.querySelector("[data-console-norun]")?.textContent).toContain("does not declare a command to start");
  });

  it("enquanto o guia carrega, o App parado fica neutro (sem Iniciar nem Console); decide quando ele responde", async () => {
    const STOPPED_BODY = { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null };
    const mountHeld = async (): Promise<{ container: HTMLElement; release: (status: number, body: unknown) => Promise<void> }> => {
      resetStore();
      clearGuideCache();
      let answer: ((r: Response) => void) | null = null;
      vi.stubGlobal(
        "fetch",
        vi.fn((input: string | URL | Request) => {
          const url = String(input);
          if (url.includes("/guide/")) return new Promise<Response>((resolve) => (answer = resolve));
          const body = url.endsWith("/app/exec") ? CONSOLE_EMPTY : STOPPED_BODY;
          return Promise.resolve(new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } }));
        }),
      );
      const layout = { windows: { app: { x: 0, y: 0, w: 900, h: 700, z: 1, mode: "normal" as const, open: true } } };
      const rendered = render(
        <WindowLayoutContext.Provider value={layout}>
          <AppRunner projectId="p1" specId={S} size="wide" viewport="free" setSelection={() => undefined} />
        </WindowLayoutContext.Provider>,
      );
      await settle();
      const release = async (status: number, body: unknown): Promise<void> => {
        await act(async () => {
          answer?.(new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }));
        });
        await settle();
      };
      return { container: rendered.container, release };
    };
    const neutral = (c: HTMLElement): void => {
      expect(c.querySelector('[data-runview="loading"]')?.textContent).toBe("Carregando o Como usar e as provas.");
      expect(within(c).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
      expect(c.querySelector("[data-console]")).toBeNull();
    };

    // Sem run_command: do neutro direto para o Console, sem "Iniciar" no meio.
    let m = await mountHeld();
    neutral(m.container);
    await m.release(200, NO_RUN_GUIDE);
    expect(m.container.querySelector("[data-console]")).not.toBeNull();
    expect(m.container.querySelector('[data-runview="loading"]')).toBeNull();
    cleanup();

    // Com run_command: do neutro para o "Iniciar o app".
    m = await mountHeld();
    neutral(m.container);
    await m.release(200, guide());
    expect(within(m.container).getByRole("button", { name: "Iniciar o app" })).toBeTruthy();
    expect(m.container.querySelector("[data-console]")).toBeNull();
    cleanup();

    // O guia falhou: vale o comportamento de antes (parado com "Iniciar o app").
    m = await mountHeld();
    neutral(m.container);
    await m.release(500, { error: "x", code: "internal" });
    expect(within(m.container).getByRole("button", { name: "Iniciar o app" })).toBeTruthy();
    expect(m.container.querySelector('[data-runview="loading"]')).toBeNull();
  });

  it("Execução: enquanto o guia carrega, fica neutra (sem Iniciar nem Console); decide quando ele responde", async () => {
    const STOPPED_BODY = { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null };
    const mountHeld = async (): Promise<{ container: HTMLElement; release: (status: number, body: unknown) => Promise<void> }> => {
      resetStore();
      clearGuideCache();
      let answer: ((r: Response) => void) | null = null;
      vi.stubGlobal(
        "fetch",
        vi.fn((input: string | URL | Request) => {
          const url = String(input);
          if (url.includes("/guide/")) return new Promise<Response>((resolve) => (answer = resolve));
          if (url.endsWith("/logs")) return Promise.resolve(new Response("", { status: 200 }));
          const body = url.endsWith("/app/exec") ? CONSOLE_EMPTY : STOPPED_BODY;
          return Promise.resolve(new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } }));
        }),
      );
      const layout = { windows: { execution: { x: 0, y: 0, w: 390, h: 800, z: 1, mode: "normal" as const, open: true } } };
      const rendered = render(
        <WindowLayoutContext.Provider value={layout}>
          <ExecutionPanel projectId="p1" specId={S} projectPath="/x" size="narrow" />
        </WindowLayoutContext.Provider>,
      );
      await settle();
      const release = async (status: number, body: unknown): Promise<void> => {
        await act(async () => {
          answer?.(new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } }));
        });
        await settle();
      };
      return { container: rendered.container, release };
    };
    const neutral = (c: HTMLElement): void => {
      expect(c.querySelector('[data-runview="loading"]')?.textContent).toBe("Carregando o Como usar e as provas.");
      expect(within(c).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
      expect(c.querySelector("[data-console]")).toBeNull();
    };

    // Sem run_command: do neutro direto para o Console, sem "Iniciar" no meio.
    let m = await mountHeld();
    neutral(m.container);
    await m.release(200, NO_RUN_GUIDE);
    expect(m.container.querySelector("[data-console]")).not.toBeNull();
    expect(within(m.container).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
    cleanup();

    // Com run_command: do neutro para o "Iniciar".
    m = await mountHeld();
    neutral(m.container);
    await m.release(200, guide());
    expect(within(m.container).getByRole("button", { name: "Iniciar" })).toBeTruthy();
    expect(m.container.querySelector("[data-console]")).toBeNull();
    cleanup();

    // O guia falhou: vale o cartão de antes, com "Iniciar".
    m = await mountHeld();
    neutral(m.container);
    await m.release(500, { error: "x", code: "internal" });
    expect(within(m.container).getByRole("button", { name: "Iniciar" })).toBeTruthy();
    expect(m.container.querySelector('[data-runview="loading"]')).toBeNull();
  });

  it("não roda no sandbox e sem run_command: a explicação do sandbox vale nas duas janelas, não o Console direto", async () => {
    const note = "A janela nativa de desktop não abre no sandbox; o Console roda o núcleo.";
    await mountRun({
      app: { guide: { status: 200, body: guide({ sandbox: { runs_here: false, note }, run: { declared: false, nothing_to_run: null } }) } },
      open: ["App", "Execução"],
    });
    for (const w of [appWindow(), execWindow()]) {
      const v = w.querySelector<HTMLElement>('[data-runview="sandbox"]');
      if (v === null) throw new Error("explicação do sandbox ausente");
      expect(v.querySelector("[data-sandbox-note]")?.textContent).toBe(note);
      expect(w.querySelector("[data-console]")).toBeNull();
      expect(within(w).queryByRole("button", { name: /^Iniciar/ })).toBeNull();
    }
    // "Abrir o Console do projeto" na Execução abre o Console no App.
    await click(within(execWindow()).getByRole("button", { name: "Abrir o Console do projeto" }));
    expect(appWindow().querySelector("[data-console]")).not.toBeNull();
  });

  it("com run_command declarado: nada muda (Iniciar na Execução e no App)", async () => {
    await mountRun({ app: { guide: { status: 200, body: guide({ run: { declared: true, nothing_to_run: null } }) } }, open: ["App", "Execução"] });
    expect(appWindow().querySelector("[data-console]")).toBeNull();
    expect(within(execWindow()).getByRole("button", { name: "Iniciar" })).toBeTruthy();
  });

  it("no celular: abas Saída, Como usar e Provas, e o campo embaixo", async () => {
    resetStore();
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: string | URL | Request) => {
        const url = String(input);
        const body = url.includes("/guide/") ? guide() : url.endsWith("/app/exec") ? CONSOLE_EMPTY : { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null };
        return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
      }),
    );
    const layout = { windows: { app: { x: 0, y: 0, w: 390, h: 800, z: 1, mode: "normal" as const, open: true } } };
    const rendered = render(
      <WindowLayoutContext.Provider value={layout}>
        <AppRunner projectId="p1" specId={S} size="narrow" viewport="free" setSelection={() => undefined} />
      </WindowLayoutContext.Provider>,
    );
    await settle();
    await click(within(rendered.container).getByRole("button", { name: "Abrir o Console" }));
    const c = rendered.container.querySelector<HTMLElement>("[data-console]");
    if (c === null) throw new Error("Console ausente");
    expect(c.getAttribute("data-compact")).toBe("true");
    const tabs = within(c).getByRole("group", { name: "Seções do Console" });
    expect(within(tabs).getAllByRole("button").map((b) => b.textContent)).toEqual(["Saída", "Como usar", "Provas"]);
    expect(within(c).getByRole("log", { name: "Saída do Console" })).toBeTruthy();
    await click(within(tabs).getByRole("button", { name: "Provas" }));
    expect(c.querySelector("[data-proofs]")?.textContent).toContain("7 de 7");
    expect(within(c).queryByRole("log", { name: "Saída do Console" })).toBeNull();
    // O campo segue no fim, em qualquer aba.
    expect(within(c).getByRole("textbox", { name: "Comando" })).toBeTruthy();
  });
});

/** Abre o Console, digita e confirma um comando. */
async function runCommand(command: string): Promise<void> {
  if (appWindow().querySelector("[data-console]") === null) await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
  fireEvent.change(within(appWindow()).getByRole("textbox", { name: "Comando" }), { target: { value: command } });
  await click(within(appWindow()).getByRole("button", { name: "Rodar" }));
  await click(within(screen.getByRole("dialog", { name: "Rodar este comando?" })).getByRole("button", { name: "Rodar" }));
}

type Mounted = Awaited<ReturnType<typeof mountRun>>;

/** Segura as respostas de `GET .../app/exec` (com `hold.on`) ou do `POST .../app/exec` (com `hold.post`). */
function holdConsole(mounted: Mounted) {
  const base = mounted.fetchMock.getMockImplementation();
  const hold = { on: false, post: false, held: [] as Array<() => void> };
  mounted.fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    if (base === undefined) throw new Error("fetch ausente");
    if (url.endsWith("/app/exec") && ((method === "GET" && hold.on) || (method === "POST" && hold.post))) {
      const body = JSON.stringify(method === "GET" ? mounted.app.console : mounted.app.exec.body);
      const status = method === "GET" ? 200 : mounted.app.exec.status;
      return new Promise<Response>((resolve) => hold.held.push(() => resolve(new Response(body, { status, headers: { "Content-Type": "application/json" } }))));
    }
    return base(input, init);
  });
  return hold;
}

const fixButton = (): HTMLElement | null => within(appWindow()).queryByRole("button", { name: "Pedir ao Arquiteto para corrigir" });
const runButton = (): HTMLElement | null => within(appWindow()).queryByRole("button", { name: "Rodar" });
const stopCommand = (): HTMLElement | null => within(appWindow()).queryByRole("button", { name: "Parar o comando" });

describe("Console: falha do ambiente e corridas (revisão da T05)", () => {
  it("um comando que não roda mostra o log e o pedido ao Arquiteto; o seguinte que roda os esconde", async () => {
    const mounted = await mountRun();
    await runCommand("ls");
    const statusBefore = calls(mounted, /\/api\/projects\/p1\/app$/);
    consoleEvent("app_exec_started", { exec_id: "e1", spec_id: S, command: "ls", started_at: "2026-10-27T12:00:00Z", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_finished", {
      exec_id: "e1",
      status: "failed",
      exit_code: null,
      finished_at: "2026-10-27T12:00:02Z",
      notice: { code: "app_exec_docker_failed", params: {}, action: "ask_architect_fix" },
      detail: "Cannot connect to the Docker daemon",
    });
    await settle();
    expect(appWindow().querySelector('[data-console] [data-failure-log]')?.textContent).toContain("Cannot connect to the Docker daemon");
    expect(fixButton()).not.toBeNull();
    // O fim do comando relê o estado do app (a falha guardada no servidor mudou).
    expect(calls(mounted, /\/api\/projects\/p1\/app$/)).toBeGreaterThan(statusBefore);
    await click(fixButton() as HTMLElement);
    expect(mounted.server.calls.find((c) => c.url.endsWith("/app/fix"))?.body).toEqual({ confirm: true, spec_id: S });
    mounted.app.exec = { status: 202, body: { exec_id: "e2", spec_id: S, command: "ls", status: "running", started_at: "x", timeout_secs: 300, origin: {} } };
    await runCommand("ls");
    consoleEvent("app_exec_started", { exec_id: "e2", spec_id: S, command: "ls", started_at: "2026-10-27T12:01:00Z", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_finished", { exec_id: "e2", status: "exited", exit_code: 0, duration_ms: 100, notice: null });
    await settle();
    expect(fixButton()).toBeNull();
    expect(appWindow().querySelector("[data-console] [data-failure-log]")).toBeNull();
  });

  it("a falha antiga do servidor some quando um comando roda nesta aba", async () => {
    const old = { ...FAILURE, code: "app_exec_docker_failed" };
    await mountRun({ app: { status: { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null, failure: old } } });
    await click(within(appWindow()).getByRole("button", { name: "Abrir o Console" }));
    expect(fixButton()).not.toBeNull();
    await runCommand("ls");
    consoleEvent("app_exec_started", { exec_id: "e1", spec_id: S, command: "ls", started_at: "x", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_finished", { exec_id: "e1", status: "exited", exit_code: 0, duration_ms: 10, notice: null });
    await settle();
    expect(fixButton()).toBeNull();
  });

  it("o fim chega antes da resposta 202: o Console não fica preso em Parar", async () => {
    const mounted = await mountRun();
    const hold = holdConsole(mounted);
    hold.post = true;
    await runCommand("ls");
    consoleEvent("app_exec_started", { exec_id: "e1", spec_id: S, command: "ls", started_at: "x", timeout_secs: 300, origin: {} });
    consoleEvent("app_exec_finished", { exec_id: "e1", status: "exited", exit_code: 0, duration_ms: 10, notice: null });
    await settle();
    await act(async () => {
      hold.held.shift()?.();
    });
    await settle();
    expect(stopCommand()).toBeNull();
    expect(runButton()).not.toBeNull();
  });

  it("uma leitura em voo, saída antes do fim, não traz o comando de volta; a releitura pedida no fim roda depois", async () => {
    const mounted = await mountRun();
    await runCommand("ls");
    consoleEvent("app_exec_started", { exec_id: "e1", spec_id: S, command: "ls", started_at: "x", timeout_secs: 300, origin: {} });
    await settle();
    // O Parar relê o Console; essa leitura fica em voo e traz o comando ainda em curso.
    const hold = holdConsole(mounted);
    hold.on = true;
    mounted.app.console = { ...CONSOLE_EMPTY, active: { exec_id: "e1", spec_id: S, command: "ls", status: "running", started_at: "x", timeout_secs: 300, origin: {} } };
    await click(stopCommand() as HTMLElement);
    expect(hold.held).toHaveLength(1);
    // O fim chega com a leitura em voo; a releitura do fim fica pendente.
    consoleEvent("app_exec_finished", { exec_id: "e1", status: "stopped", exit_code: null, duration_ms: 10, notice: null });
    await settle();
    hold.on = false;
    mounted.app.console = { ...CONSOLE_EMPTY, last: { exec_id: "e1", spec_id: S, command: "ls", status: "stopped", exit_code: null, duration_ms: 10, output: "" } };
    const getsBefore = mounted.server.calls.filter((c) => c.method === "GET" && c.url.endsWith("/app/exec")).length;
    await act(async () => {
      hold.held.shift()?.();
    });
    await settle();
    expect(stopCommand()).toBeNull();
    expect(runButton()).not.toBeNull();
    expect(mounted.server.calls.filter((c) => c.method === "GET" && c.url.endsWith("/app/exec")).length).toBe(getsBefore + 1);
  });
});

describe("Execução pelo modo", () => {
  it("rodando sem página e terminou têm pílula própria; Abrir o Console mostra o Console no App", async () => {
    const mounted = await mountRun({ app: { status: RUNNING_NO_PAGE }, open: ["App", "Execução"] });
    const pill = (): Element | null => execWindow().querySelector("[data-app-state]");
    expect(pill()?.textContent).toBe("Rodando sem página");
    mounted.app.status = FINISHED;
    emit("app_mode_changed", { spec_id: S, mode: "finished", previous: "running", exit_code: 0 });
    await settle();
    expect(pill()?.textContent).toBe("Terminou · código 0");
    await click(within(execWindow()).getByRole("button", { name: "Abrir o Console" }));
    expect(appWindow().querySelector("[data-console]")).not.toBeNull();
  });
});

describe("em en", () => {
  it("terminou, falha e Console sem português", async () => {
    setLanguage("en", { persist: false });
    // O "Como usar" vem do relatório, no idioma em que ele foi escrito.
    const english = guide({ how_to_use: { source: "report", markdown: "## How to run\nRun `ledger report --month 2026-09`.", truncated: false } });
    await mountRun({ app: { status: FINISHED, guide: { status: 200, body: english } }, open: [] });
    await clickApp("App");
    const v = view("finished");
    expect(within(v).getByRole("status").textContent).toBe("Finished · code 0");
    expect(within(v).getByRole("button", { name: "Run again" })).toBeTruthy();
    expect(portugueseLeftovers(appWindow())).toEqual([]);
    await click(within(v).getByRole("button", { name: "Open in the Console" }));
    expect(appWindow().querySelector("[data-console]")?.textContent).toContain("approved environment container");
    expect(portugueseLeftovers(appWindow())).toEqual([]);
    await click(within(appWindow()).getByRole("button", { name: "Back" }));
    expect(view("finished")).toBeTruthy();
  });

  it("falha do ambiente em en", async () => {
    setLanguage("en", { persist: false });
    await mountRun({ app: { status: STOPPED_WITH_FAILURE }, open: [] });
    await clickApp("App");
    const v = view("failure");
    expect(within(v).getByRole("heading", { level: 2 }).textContent).toBe("The project's Docker environment did not start");
    expect(within(v).getByRole("button", { name: "Ask the Architect to fix it" })).toBeTruthy();
    expect(portugueseLeftovers(appWindow())).toEqual([]);
  });
});
