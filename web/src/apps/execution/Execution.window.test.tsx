/*
 * Janela Execução montada pelo shell (spec 095, T03; casos herdados do App da 093/T09 e 094/T05):
 * - cartão (spec, "docker compose up · porta N", pílula Parada/Subindo/No ar/Saiu, Iniciar ou
 *   Parar) e a confirmação humana com o comando exato e a pasta;
 * - portas como links, avisos de porta, erros em português, detalhes técnicos recolhidos, 412
 *   `prereq_missing` e a prontidão `execute`;
 * - console no resto da janela: registro como texto, consulta a cada 2 s só com a Execução visível
 *   e o app no ar, Copiar registro;
 * - a dica de que a página aparece sozinha no App; o aviso "App no ar" só faz fade sob
 *   `prefers-reduced-motion` (C-15); o lançador e os registros de janela conhecem a Execução.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { notReady, settle } from "../../desktop/shell-test-utils";
import { cssRules } from "../../desktop/test-utils";
import { WindowLayoutContext } from "../../desktop/useWindowVisibility";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { parseLayout } from "../../desktop/windows";
import { resetStore } from "../../test-utils";
import {
  APP_URL,
  HOST,
  LOGS_URL,
  S,
  START_URL,
  STOP_URL,
  appEmpty,
  appWindow,
  calls,
  clickDock,
  confirmDialog,
  emit,
  execWindow,
  frame,
  mountRun,
  running,
  startFromExecution,
} from "../app/app-test-utils";
import { LOGS_POLL_MS } from "../app/appModel";
import { ExecutionPanel } from "./ExecutionApp";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const pill = (win: HTMLElement = execWindow()): HTMLElement => {
  const el = win.querySelector<HTMLElement>("[data-app-state]");
  if (el === null) throw new Error("pílula ausente");
  return el;
};

describe("Execução: registros de janela", () => {
  it("dock, lançador, título, ícone e subtítulo; arranjo com ou sem a Execução é válido", async () => {
    await mountRun({ open: ["Execução"] });
    const win = execWindow();
    expect(win.getAttribute("data-window")).toBe("execution");
    expect(document.getElementById(windowSubtitleId("execution"))?.textContent).toBe("s1 · docker compose");
    expect(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Execução" })).toBeTruthy();
    expect(parseLayout({ windows: { execution: { x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: true } } })?.windows.execution?.open).toBe(true);
    expect(parseLayout({ windows: { app: { x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: true } } })?.windows.execution).toBeUndefined();
  });

  it("o lançador lista a Execução com Alt+7", async () => {
    await mountRun({ open: [] });
    await act(async () => {
      fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    });
    await settle();
    const option = screen.getAllByRole("option").find((el) => el.textContent?.includes("Execução") === true);
    expect(option).toBeTruthy();
    expect(option?.textContent).toContain("Alt+7");
  });
});

describe("Execução: controle", () => {
  it("cartão parado: spec, comando, pílula Parada, Iniciar (primária) e a dica do App", async () => {
    await mountRun({ open: ["Execução"] });
    const win = execWindow();
    const card = within(win).getByRole("region", { name: "Execução do app" });
    expect(within(card).getByText(S)).toBeTruthy();
    expect(within(card).getByText("docker compose up")).toBeTruthy();
    expect(pill().textContent).toBe("Parada");
    expect(within(card).getByRole("button", { name: "Iniciar" }).getAttribute("data-variant")).toBe("primary");
    expect(within(win).getByText("Quando a porta responder, a janela App mostra a página sozinha.")).toBeTruthy();
    expect(win.querySelectorAll('[data-variant="primary"]')).toHaveLength(1);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("exige confirmação com o comando exato e a pasta; Cancelar e Esc não enviam nada", async () => {
    const view = await mountRun({ open: ["Execução"] });
    const win = execWindow();
    fireEvent.click(within(win).getByRole("button", { name: "Iniciar" }));
    const dialog = screen.getByRole("dialog", { name: "Iniciar o app?" });
    expect(dialog.textContent).toContain("docker compose up -d --build --wait");
    expect(dialog.textContent).toContain("ambiente Docker");
    expect(dialog.textContent).toContain("/home/dev/Loja");
    expect(document.activeElement?.textContent).toBe("Cancelar");
    expect(findForbiddenGlyphs(dialog)).toEqual([]);
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(within(win).getByRole("button", { name: "Iniciar" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancelar" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(calls(view, START_URL)).toBe(0);
  });

  it("Executar envia confirm e spec_id uma vez: no ar, porta no cartão com Abrir no App e a nova aba discreta; registro e Parar", async () => {
    const view = await mountRun({ open: ["App", "Execução"] });
    await startFromExecution();
    const win = execWindow();
    expect(calls(view, START_URL)).toBe(1);
    expect(view.server.calls.find((c) => START_URL.test(c.url))?.body).toEqual({ confirm: true, spec_id: S });
    expect(pill().textContent).toBe("No ar");
    expect(within(win).getByText("docker compose up · porta 18080")).toBeTruthy();
    const ports = within(win).getByRole("list", { name: "Portas publicadas" });
    // 095/T07 (decisão 6): o endereço é texto; a ação é "Abrir no App"; a nova aba fica na linha de baixo.
    expect(ports.querySelector(".nd-exec-port")?.textContent).toBe(`web · ${HOST}:18080`);
    expect(within(ports).getByRole("button", { name: "Abrir no App" })).toBeTruthy();
    const link = within(ports).getByRole("link", { name: "Abrir em nova aba" });
    expect(link.closest("p")?.textContent).toBe("Prefere o navegador do sistema? Abrir em nova aba");
    expect(link.getAttribute("href")).toBe(APP_URL);
    expect(link.getAttribute("target")).toBe("_blank");
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    await waitFor(() => expect(within(win).getByRole("log", { name: "Registro de execução" }).textContent).toBe("listening"));
    const stop = within(win).getByRole("button", { name: "Parar" });
    expect(stop.getAttribute("data-variant")).toBe("danger");
    expect(within(win).queryByRole("button", { name: "Iniciar" })).toBeNull();
    // A página aparece sozinha no App.
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("095/T07: uma linha por porta; Abrir no App abre e foca o App naquela porta", async () => {
    const API_URL = `http://${HOST}:18081`;
    const two = running({
      ports: [
        { service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: true },
        { service: "api", target: 8081, published: 18081, url: API_URL, embeddable: true },
      ],
    });
    await mountRun({ app: { status: two }, open: ["Execução"] });
    const ports = within(execWindow()).getByRole("list", { name: "Portas publicadas" });
    const rows = [...ports.querySelectorAll<HTMLElement>("[data-exec-port]")];
    expect(rows.map((r) => r.getAttribute("data-exec-port"))).toEqual([APP_URL, API_URL]);
    for (const row of rows) {
      expect(within(row).getAllByRole("button", { name: "Abrir no App" })).toHaveLength(1);
      expect(within(row).getByRole("link", { name: "Abrir em nova aba" }).getAttribute("href")).toBe(row.getAttribute("data-exec-port"));
    }
    // O App ainda não está aberto: "Abrir no App" da segunda porta abre o App nela, na frente.
    expect(screen.queryByRole("region", { name: "App" })).toBeNull();
    const second = rows[1];
    if (second === undefined) throw new Error("segunda porta ausente");
    await act(async () => {
      fireEvent.click(within(second).getByRole("button", { name: "Abrir no App" }));
    });
    await settle();
    expect(appWindow().getAttribute("data-active")).toBe("true");
    expect(frame().getAttribute("src")).toBe(API_URL);
    expect((appWindow().querySelector("[data-app-address]") as HTMLInputElement | null)?.value).toBe(API_URL);
    // De volta à primeira porta, com o App já aberto (só foca).
    const first = rows[0];
    if (first === undefined) throw new Error("primeira porta ausente");
    await act(async () => {
      fireEvent.click(within(first).getByRole("button", { name: "Abrir no App" }));
    });
    await settle();
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(appWindow().getAttribute("data-active")).toBe("true");
  });

  it("095/T07: as guardas continuam; endereço que não é http da página não vira linha, botão nem link", async () => {
    const odd = running({ ports: [{ service: "web", target: 8080, published: 18080, url: "javascript:alert(1)", embeddable: false }] });
    await mountRun({ app: { status: odd }, open: ["Execução"] });
    expect(within(execWindow()).queryByRole("list", { name: "Portas publicadas" })).toBeNull();
    expect(execWindow().querySelector("[data-open-browser], [data-open-in-app]")).toBeNull();
    // E o link da linha passa pelo `safeHref` (só http/https), além da guarda do cliente.
    const source = readFileSync(resolve(process.cwd(), "src/apps/execution/ExecutionApp.tsx"), "utf8");
    expect(source).toMatch(/const href = safeHref\(port\.url\);/);
    expect(source).toMatch(/href=\{href\} target="_blank" rel="noopener noreferrer"/);
  });

  it("estado inicial no ar (outra aba): mostra no ar sem iniciar", async () => {
    const view = await mountRun({ app: { status: running() }, open: ["Execução"] });
    expect(pill().textContent).toBe("No ar");
    expect(calls(view, START_URL)).toBe(0);
  });

  it("Parar envia stop; app_exited mostra o motivo e volta a oferecer a execução (no App também)", async () => {
    const view = await mountRun({ app: { status: running() }, open: ["App", "Execução"] });
    const win = execWindow();
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Parar" }));
    });
    await settle();
    expect(calls(view, STOP_URL)).toBe(1);
    emit("app_exited", { code: 143, reason: "stopped" });
    expect(within(win).getByText("Parado.")).toBeTruthy();
    expect(pill().textContent).toBe("Saiu");
    expect(within(win).queryByRole("button", { name: "Parar" })).toBeNull();
    expect(within(win).getByRole("button", { name: "Iniciar" })).toBeTruthy();
    expect(appWindow().querySelector("iframe")).toBeNull();
    expect(within(appWindow()).getByRole("button", { name: "Executar novamente" })).toBeTruthy();
  });

  it("app_exited por saída do programa informa o código", async () => {
    await mountRun({ app: { status: running() }, open: ["Execução"] });
    emit("app_exited", { code: 1, reason: "exited" });
    expect(within(execWindow()).getByText("Encerrado (código 1): o programa terminou.")).toBeTruthy();
  });

  it("outra especificação no ar: pílula própria e Parar; o App sem iframe", async () => {
    await mountRun({ app: { status: running({ spec_id: "s0" }) }, open: ["App", "Execução"] });
    expect(pill().textContent).toBe("Outra especificação no ar");
    expect(within(execWindow()).getByRole("button", { name: "Parar" })).toBeTruthy();
    expect(appWindow().querySelector("iframe")).toBeNull();
  });

  it("app_started e a releitura mostra o app encerrado: adota o encerramento (T10)", async () => {
    const view = await mountRun({ open: ["App", "Execução"] });
    view.app.status = { state: "exited", spec_id: S, url: null, started_at: null, exit_code: 1 };
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    const win = execWindow();
    expect(within(win).queryByRole("button", { name: "Parar" })).toBeNull();
    expect(within(win).getByText("Encerrado (código 1).")).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Iniciar" })).toBeTruthy();
    expect(within(appWindow()).getByRole("button", { name: "Executar novamente" })).toBeTruthy();
  });

  it("aviso de porta trocada junto aos links", async () => {
    await mountRun({ app: { start: { status: 200, body: running({ notices: ["Porta 3000 ocupada; usando 18080"] }) } }, open: ["Execução"] });
    await startFromExecution();
    expect(within(execWindow()).getByRole("status", { name: "Aviso de porta" }).textContent).toBe("Porta 3000 ocupada; usando 18080");
  });

  it.each([
    [409, "busy", "Há uma execução em andamento neste projeto."],
    [409, "already_running", "O sistema já está no ar."],
    [409, "spec_not_completed", "A especificação ainda não foi concluída."],
    [422, "docker_failed", "Não foi possível subir o ambiente Docker do projeto. Verifique se o Docker está rodando."],
    [409, "environment_not_docker", "Este projeto não tem ambiente Docker. Peça ao Arquiteto para replanejar a especificação."],
    [400, undefined, "A execução não foi confirmada."],
  ])("erro %i %s mostra o motivo em português", async (code, reason, text) => {
    await mountRun({ app: { start: { status: code, body: { error: "x", ...(reason !== undefined ? { reason } : {}) } } }, open: ["App", "Execução"] });
    await startFromExecution();
    const win = execWindow();
    expect(within(win).getByRole("alert").textContent).toBe(text);
    expect(within(win).getByRole("button", { name: "Iniciar" })).toBeTruthy();
    // O App volta a "parado", com a mesma frase (sem detalhe técnico).
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
    expect(within(appWindow()).getByText(text)).toBeTruthy();
  });

  it("falha do Docker com detalhe técnico recolhível (só na Execução)", async () => {
    await mountRun({ app: { start: { status: 422, body: { error: "x", reason: "docker_failed", detail: "container exited (1)\nnpm ERR! boom" } } }, open: ["App", "Execução"] });
    await startFromExecution();
    const win = execWindow();
    const details = win.querySelector("details");
    expect(details?.hasAttribute("open")).toBe(false);
    expect(within(win).getByLabelText("Detalhes da falha").textContent).toContain("npm ERR! boom");
    expect(appWindow().querySelector("details")).toBeNull();
    expect(appWindow().textContent).not.toContain("npm ERR!");
  });

  it("412 prereq_missing: a lista do que falta (rótulos da prontidão relida) com Configurar", async () => {
    const view = await mountRun({ app: { start: { status: 412, body: { error: "prereq_missing", missing: ["docker_daemon", "git"] } } }, open: ["Execução"] });
    view.server.model.readiness = {
      ...view.server.model.readiness,
      execute: { ok: false, missing: [{ id: "docker_daemon", label: "Docker daemon", settings_tab: "machine" }] },
    };
    const before = view.server.count("GET", /\/api\/readiness$/) + view.server.count("POST", /\/api\/readiness\/refresh$/);
    await startFromExecution();
    const alert = within(execWindow()).getByRole("alert");
    expect(alert.textContent).toContain("Falta configurar: Docker daemon, Git.");
    expect(within(alert).getByRole("button", { name: "Configurar: Docker daemon" })).toBeTruthy();
    expect(view.server.count("GET", /\/api\/readiness$/) + view.server.count("POST", /\/api\/readiness\/refresh$/)).toBeGreaterThan(before);
  });

  it("prontidão execute incompleta: Iniciar desabilitado com o que falta", async () => {
    await mountRun({ extra: { readiness: notReady() }, open: ["Execução"] });
    const win = execWindow();
    expect((within(win).getByRole("button", { name: "Iniciar" }) as HTMLButtonElement).disabled).toBe(true);
    expect(within(win).getByText("Para executar o app, falta:")).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Configurar: Docker daemon" })).toBeTruthy();
  });

  it("sem especificação concluída: Iniciar desabilitado com o motivo ligado; subtítulo de espera", async () => {
    await mountRun({ status: "IN_PROGRESS", ui: {}, open: ["Execução"] });
    const win = execWindow();
    const start = within(win).getByRole("button", { name: "Iniciar" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    const reason = document.getElementById(start.getAttribute("aria-describedby") ?? "");
    expect(reason?.textContent).toBe("Disponível quando uma especificação for concluída.");
    expect(document.getElementById(windowSubtitleId("execution"))?.textContent).toBe("aguardando a conclusão");
  });

  it("fechar a Execução com a confirmação aberta a cancela: o App volta a 'parado'", async () => {
    const view = await mountRun({ open: ["App"] });
    await act(async () => {
      fireEvent.click(within(appWindow()).getByRole("button", { name: "Iniciar o app" }));
    });
    await settle();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("starting");
    await act(async () => {
      fireEvent.click(within(execWindow()).getByRole("button", { name: "Fechar" }));
    });
    await settle();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
    expect(calls(view, START_URL)).toBe(0);
  });

  it("o GET inicial (parado) que chega depois do POST de partida não derruba o No ar", async () => {
    const view = await mountRun({ app: { holdStatus: true }, open: ["Execução"] });
    expect(view.app.held).toHaveLength(1);
    await startFromExecution();
    expect(pill().textContent).toBe("No ar");
    await act(async () => {
      view.app.held[0]?.();
    });
    await settle();
    expect(pill().textContent).toBe("No ar");
    expect(within(execWindow()).getByRole("button", { name: "Parar" })).toBeTruthy();
  });
});

describe("Execução: console", () => {
  it("registro é texto, consulta a cada 2 s com o app no ar; minimizar para a consulta e restaurar mantém o texto", async () => {
    const view = await mountRun({ app: { status: running(), logs: "<b>oi</b>" }, open: ["Execução"] });
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    // Religa o intervalo com o relógio falso (minimiza e restaura).
    await clickDock("Execução");
    await clickDock("Execução");
    const win = execWindow();
    const log = within(win).getByRole("log", { name: "Registro de execução" });
    expect(log.textContent).toBe("<b>oi</b>");
    expect(log.querySelector("b")).toBeNull();
    expect(log.getAttribute("aria-live")).toBe("off");
    const before = calls(view, LOGS_URL);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS * 2 + 50);
    });
    expect(calls(view, LOGS_URL)).toBe(before + 2);
    await clickDock("Execução");
    expect(win.hidden).toBe(true);
    const collapsed = calls(view, LOGS_URL);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS * 3);
    });
    expect(calls(view, LOGS_URL)).toBe(collapsed);
    await clickDock("Execução");
    expect(calls(view, LOGS_URL)).toBe(collapsed + 1);
    expect(within(win).getByRole("log").textContent).toBe("<b>oi</b>");
  });

  it("sem saída: 'Sem saída ainda.' e Copiar desabilitado; copiar usa a Clipboard API", async () => {
    const view = await mountRun({ app: { status: running(), logs: "" }, open: ["Execução"] });
    const win = execWindow();
    expect(within(win).getByRole("log").textContent).toBe("Sem saída ainda.");
    expect((within(win).getByRole("button", { name: "Copiar registro" }) as HTMLButtonElement).disabled).toBe(true);
    view.app.logs = "web-1 | no ar";
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await clickDock("Execução");
    await clickDock("Execução");
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Copiar registro" }));
    });
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("web-1 | no ar"));
    expect(within(win).getByRole("button", { name: "Copiado" })).toBeTruthy();
  });

  it("registro com ANSI (Vite real): a tela e o Copiar registro mostram texto limpo (spec 113)", async () => {
    const E = "\u001b";
    const raw = [
      `web-1  | ${E}[32m${E}[1mVITE${E}[22m v6.2.2${E}[39m  ${E}[2mready in ${E}[0m${E}[1m412${E}[22m${E}[2m${E}[0m ms${E}[22m`,
      `web-1  | ${E}[38;5;208mwarn${E}[0m`,
      `web-1  | building 10%\r${E}[2Kbuilding 100%`,
    ].join("\n");
    const clean = "web-1  | VITE v6.2.2  ready in 412 ms\nweb-1  | warn\nweb-1  | building 100%";
    await mountRun({ app: { status: running(), logs: raw }, open: ["Execução"] });
    const win = execWindow();
    const log = within(win).getByRole("log");
    expect(log.textContent).toBe(clean);
    expect(log.textContent).not.toMatch(/[\u001b\u009b]/);
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Copiar registro" }));
    });
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(clean));
  });

  it("detalhe da falha com ANSI chega limpo (spec 113)", async () => {
    const detail = "container exited (1)\n\u001b[31mnpm ERR!\u001b[39m boom";
    await mountRun({ app: { start: { status: 422, body: { error: "x", reason: "docker_failed", detail } } }, open: ["Execução"] });
    await startFromExecution();
    expect(within(execWindow()).getByLabelText("Detalhes da falha").textContent).toBe("container exited (1)\nnpm ERR! boom");
  });

  it("encerrado: para a consulta", async () => {
    const view = await mountRun({ app: { status: running() }, open: [] });
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    await clickDock("Execução");
    emit("app_exited", { code: 0, reason: "exited" });
    await settle();
    const n = calls(view, LOGS_URL);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS * 3);
    });
    expect(calls(view, LOGS_URL)).toBe(n);
  });

  it("o console nunca cobre nem remonta a página: o App não tem registro nem abas; o iframe fica o mesmo; o foco não é puxado quando a página sobe", async () => {
    const view = await mountRun({ app: { start: { status: 200, body: running({ ports: [], embeddable: null }) } }, open: ["App", "Execução"] });
    await startFromExecution();
    const copy = await waitFor(() => {
      const button = within(execWindow()).getByRole("button", { name: "Copiar registro" });
      expect(button.hasAttribute("disabled")).toBe(false);
      return button;
    });
    act(() => copy.focus());
    view.app.status = running();
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    const iframe = frame();
    expect(document.activeElement).toBe(copy);
    expect(within(appWindow()).queryByRole("log")).toBeNull();
    expect(within(appWindow()).queryByRole("tablist")).toBeNull();
    view.app.logs = "mais uma linha";
    await clickDock("Execução");
    await clickDock("Execução");
    expect(frame()).toBe(iframe);
    expect(iframe.getAttribute("src")).toBe(APP_URL);
  });
});

describe("Execução: aviso 'App no ar' (C-15)", () => {
  it("a entrada sobe e aparece; sob prefers-reduced-motion, só fade (sem transform)", () => {
    const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/shell.css"), "utf8");
    const rules = cssRules(css);
    const reduce = rules.filter((r) => r.at?.includes("prefers-reduced-motion: reduce") === true && r.selectors.includes('.nd-toast[data-titled="true"]'));
    expect(reduce).toHaveLength(1);
    const name = /animation:\s*([\w-]+)/.exec(reduce[0]?.body ?? "")?.[1];
    expect(name).toBe("nd-toast-fade");
    const fade = css.slice(css.indexOf("@keyframes nd-toast-fade"));
    const fadeBody = fade.slice(0, fade.indexOf("}\n}") + 3);
    expect(fadeBody).toContain("opacity");
    expect(fadeBody).not.toContain("transform");
    const motion = rules.filter((r) => r.at?.includes("prefers-reduced-motion: no-preference") === true && r.selectors.includes('.nd-toast[data-titled="true"]'));
    expect(motion[0]?.body).toContain("nd-toast-rise");
    // Fora das duas consultas, o aviso não anima.
    const plain = rules.filter((r) => r.at === null && r.selectors.includes('.nd-toast[data-titled="true"]'));
    for (const rule of plain) expect(rule.body).not.toMatch(/animation|transition/);
  });
});

describe("Execução: respostas atrasadas", () => {
  it("resposta do estado de outro projeto chegada depois da troca é descartada", async () => {
    resetStore();
    const held: Array<() => void> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith("/logs")) return Promise.resolve(new Response("", { status: 200 }));
        return new Promise<Response>((resolve) => held.push(() => resolve(new Response(JSON.stringify(running()), { status: 200 }))));
      }),
    );
    const layout = { windows: { execution: { x: 0, y: 0, w: 600, h: 400, z: 1, mode: "normal" as const, open: true } } };
    const panel = (projectId: string) => (
      <WindowLayoutContext.Provider value={layout}>
        <ExecutionPanel key={projectId} projectId={projectId} specId={S} projectPath="/x" size="wide" />
      </WindowLayoutContext.Provider>
    );
    const rendered = render(panel("p1"));
    await settle();
    rendered.rerender(panel("p2"));
    await settle();
    await act(async () => {
      held[0]?.();
    });
    await settle();
    const state = (): string | null => rendered.container.querySelector("[data-app-state]")?.getAttribute("data-app-state") ?? null;
    expect(state()).toBe("stopped");
    await act(async () => {
      held[1]?.();
    });
    await settle();
    expect(state()).toBe("running");
  });
});

describe("Execução: confirmação também pelo App", () => {
  it("'Iniciar o app' com a Execução aberta: foca a Execução e abre a confirmação; Executar sobe o app", async () => {
    const view = await mountRun({ open: ["App", "Execução"] });
    await act(async () => {
      fireEvent.click(within(appWindow()).getByRole("button", { name: "Iniciar o app" }));
    });
    await settle();
    await confirmDialog();
    expect(calls(view, START_URL)).toBe(1);
    expect(pill().textContent).toBe("No ar");
  });
});
