/*
 * App e Execução nos dois idiomas (spec 102, T04): em en, nenhum texto visível ou acessível das
 * duas janelas fica em português (estados vazios, barra do navegador, avisos de rede e de porta,
 * confirmação, console e erros do servidor pelo `code`); o que o app e o Docker escrevem (registro,
 * detalhe técnico) aparece como veio; pt-BR igual a hoje; a troca vale na hora, sem recarregar.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { appErrorMessage, appExitedText } from "../../copy";
import { rangeLabel } from "../../desktop/network";
import { activateApp, notReady, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { APP_URL, appEmpty, emit, mountRun, running } from "./app-test-utils";
import { reachHintText, runStateLabel } from "./appModel";
import { reachEnvironment } from "./runSession";

const original = { ...reachEnvironment };

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

type Lang = "pt-BR" | "en";
const RUN_NAME: Readonly<Record<Lang, string>> = { "pt-BR": "Execução", en: "Run" };

async function openApp(name: string): Promise<void> {
  await act(async () => {
    activateApp(name);
  });
  await settle();
}

/** Monta no idioma pedido e abre as janelas pelo menu do N (com os nomes do idioma). */
async function mount(lang: Lang, options: Parameters<typeof mountRun>[0] = {}, windows: readonly ("app" | "run")[] = ["app"]) {
  setLanguage(lang, { persist: false });
  const view = await mountRun({ ...options, open: [] });
  for (const w of windows) await openApp(w === "app" ? "App" : RUN_NAME[lang]);
  return view;
}

const appWin = (): HTMLElement => screen.getByRole("region", { name: "App" });
const runWin = (lang: Lang = "en"): HTMLElement => screen.getByRole("region", { name: RUN_NAME[lang] });
const toolbar = (): HTMLElement => within(appWin()).getByRole("toolbar");

function emptyTitle(): string | null | undefined {
  return appEmpty()?.querySelector("h2")?.textContent;
}

/** Sem português no App, na Execução (se aberta) e no que estiver flutuando (menus, diálogos). */
function expectNoPortuguese(): void {
  expect(portugueseLeftovers(appWin())).toEqual([]);
  const run = screen.queryByRole("region", { name: "Run" });
  if (run !== null) expect(portugueseLeftovers(run)).toEqual([]);
  for (const el of [...screen.queryAllByRole("menu"), ...screen.queryAllByRole("dialog")]) expect(portugueseLeftovers(el)).toEqual([]);
}

async function startInRun(): Promise<void> {
  await act(async () => {
    fireEvent.click(within(runWin()).getByRole("button", { name: "Start" }));
  });
  await settle();
  const dialog = screen.getByRole("dialog", { name: "Start the app?" });
  expect(dialog.textContent).toContain("Nants will start the project's Docker environment (.nants/compose.yaml) on your computer, in the project folder:");
  expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeTruthy();
  expectNoPortuguese();
  await act(async () => {
    fireEvent.click(within(dialog).getByRole("button", { name: "Run" }));
  });
  await settle();
}

describe("App em en", () => {
  it("parado: estado vazio, barra do navegador, menu de tamanho e subtítulo sem português", async () => {
    await mount("en");
    expect(emptyTitle()).toBe("The app is not running");
    const empty = appEmpty() as HTMLElement;
    expect(within(empty).getByText("Start the app in the Run window. As soon as it answers, the page shows up here.")).toBeTruthy();
    expect(within(empty).getByRole("button", { name: "Start the app" })).toBeTruthy();
    expect(within(empty).getByRole("button", { name: "Open Run" })).toBeTruthy();
    const address = within(toolbar()).getByRole("textbox", { name: "App address" });
    expect(address.getAttribute("placeholder")).toBe("The app is not running");
    expect(within(toolbar()).getByRole("button", { name: "Back" })).toBeTruthy();
    expect(within(toolbar()).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(toolbar().getAttribute("aria-label")).toBe("App browser");
    const size = within(toolbar()).getByRole("button", { name: "Screen size: Free" });
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("s1 · ready to test");
    expectNoPortuguese();
    await act(async () => {
      fireEvent.click(size);
    });
    const menu = screen.getByRole("menu", { name: "Screen size" });
    expect(within(menu).getAllByRole("menuitemradio").map((r) => r.textContent)).toEqual(["Free", "Desktop 1440", "Tablet 768", "Phone 390"]);
    expectNoPortuguese();
  });

  it("Iniciar: confirmação, subindo, página no ar e Execução com portas, aviso traduzido pelo code e console", async () => {
    const view = await mount("en", {
      app: {
        holdStart: true,
        start: {
          status: 200,
          body: running({ notices: ["Porta 3000 ocupada; usando 18080"], notice_codes: [{ code: "app_port_remapped", params: { wanted: 3000, used: 18080 } }] }),
        },
        logs: "listening on 0.0.0.0:8080",
      },
    });
    await act(async () => {
      fireEvent.click(within(appWin()).getByRole("button", { name: "Start the app" }));
    });
    await settle();
    expect(emptyTitle()).toBe("The app is starting");
    expect(within(appEmpty() as HTMLElement).getByText("Confirm the start in the Run window. The page shows up here when the port answers.")).toBeTruthy();
    const dialog = screen.getByRole("dialog", { name: "Start the app?" });
    expectNoPortuguese();
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Run" }));
    });
    await settle();
    expect(within(appEmpty() as HTMLElement).getByText("The Docker environment is starting. The page shows up here when the port answers.")).toBeTruthy();
    expect(within(appEmpty() as HTMLElement).getByRole("progressbar", { name: "Starting the app" })).toBeTruthy();
    expect(runWin().querySelector("[data-app-state]")?.textContent).toBe("Starting");
    expectNoPortuguese();
    await act(async () => {
      view.app.releaseStart?.();
    });
    await settle();
    expect(appWin().querySelector("iframe")?.getAttribute("title")).toBe("Running app");
    expect(within(toolbar()).getByRole("link", { name: "Open in new tab" }).getAttribute("href")).toBe(APP_URL);
    const run = runWin();
    expect(run.querySelector("[data-app-state]")?.textContent).toBe("Live");
    expect(within(run).getByRole("region", { name: "App run" }).textContent).toContain("docker compose up · port 18080");
    const ports = within(run).getByRole("list", { name: "Published ports" });
    expect(within(ports).getByRole("button", { name: "Open in App" })).toBeTruthy();
    expect(ports.textContent).toContain("Prefer the system browser?");
    expect(within(ports).getByRole("link", { name: "Open in new tab" })).toBeTruthy();
    expect(within(run).getByRole("status", { name: "Port notice" }).textContent).toBe("Port 3000 busy; using 18080");
    expect(within(run).getByRole("region", { name: "Console" }).textContent).toContain("Log");
    expect(within(run).getByRole("button", { name: "Copy log" })).toBeTruthy();
    // O registro do app aparece como veio.
    expect(within(run).getByRole("log").textContent).toContain("listening on 0.0.0.0:8080");
    expect(run.querySelector("[data-exec-hint]")?.textContent).toBe("When the port answers, the App window shows the page on its own.");
    expect(within(run).getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(screen.getByText("App is live")).toBeTruthy();
    expect(screen.getByText("The page was opened in the App window.")).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("execution"))?.textContent).toBe("s1 · docker compose");
    expectNoPortuguese();
  });

  it("erros do servidor pelo code (porta ocupada, faixa esgotada) e pelo reason, com o detalhe técnico como veio", async () => {
    await mount(
      "en",
      { app: { start: { status: 422, body: { error: "Uma porta publicada...", reason: "docker_port_in_use", detail: "bind: address already in use", code: "app_port_in_use" } } } },
      ["app", "run"],
    );
    await startInRun();
    const run = runWin();
    expect(within(run).getByRole("alert").textContent).toBe("An app port is already in use on this computer.");
    expect(within(appEmpty() as HTMLElement).getByText("An app port is already in use on this computer.")).toBeTruthy();
    const details = run.querySelector("details") as HTMLElement;
    expect(details.querySelector("summary")?.textContent).toBe("Technical details");
    expect(within(details).getByLabelText("Failure details").textContent).toBe("bind: address already in use");
    expectNoPortuguese();
    cleanup();

    await mount(
      "en",
      {
        app: {
          start: {
            status: 422,
            body: { error: "faixa esgotada", reason: "docker_port_in_use", detail: "x", code: "view_ports_exhausted", params: { start: 3001, end: 3010 } },
          },
        },
      },
      ["app", "run"],
    );
    await startInRun();
    expect(within(runWin()).getByRole("alert").textContent).toBe("All preview ports (3001 to 3010) are in use. Stop another app in Run and try again.");
    expectNoPortuguese();
    cleanup();

    await mount("en", { app: { start: { status: 422, body: { error: "x", reason: "docker_failed" } } } }, ["app", "run"]);
    await startInRun();
    expect(within(runWin()).getByRole("alert").textContent).toBe("Could not start the project's Docker environment. Check that Docker is running.");
    expectNoPortuguese();
  });

  it("encerrado, outra especificação, porta sem endereço e embed bloqueado ou sem veredito", async () => {
    await mount("en", { app: { status: running() } }, ["app", "run"]);
    emit("app_exited", { code: 1, reason: "exited" });
    await settle();
    expect(emptyTitle()).toBe("The app stopped");
    expect(within(appEmpty() as HTMLElement).getByText("Ended (code 1): the program finished.")).toBeTruthy();
    expect(within(appEmpty() as HTMLElement).getByRole("button", { name: "Run again" })).toBeTruthy();
    expect(runWin().querySelector("[data-app-state]")?.textContent).toBe("Exited");
    expect(within(runWin()).getByText("Ended (code 1): the program finished.")).toBeTruthy();
    expectNoPortuguese();
    cleanup();

    await mount("en", { app: { status: running({ spec_id: "s0" }) } });
    expect(emptyTitle()).toBe("Another spec is live");
    expect(within(appEmpty() as HTMLElement).getByText("Another spec of this project is running. Stop it to start this one.")).toBeTruthy();
    expect(within(appEmpty() as HTMLElement).getByRole("button", { name: "Stop" })).toBeTruthy();
    expectNoPortuguese();
    cleanup();

    await mount("en", { app: { status: running({ ports: [], embeddable: null }) } });
    expect(emptyTitle()).toBe("The app is live without an address");
    expect(within(appEmpty() as HTMLElement).getByText("The app did not publish any port with an address. See the ports and the log in Run.")).toBeTruthy();
    expectNoPortuguese();
    cleanup();

    await mount("en", { app: { status: running({ embeddable: false, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: false }] }) } });
    expect(emptyTitle()).toBe("The page cannot be shown here");
    expect(
      within(appEmpty() as HTMLElement).getByText("The app does not allow being shown inside another page (X-Frame-Options or frame-ancestors). Use Open in new tab."),
    ).toBeTruthy();
    expect(within(appEmpty() as HTMLElement).getByRole("link", { name: "Open in new tab" })).toBeTruthy();
    expectNoPortuguese();
    cleanup();

    await mount("en", { app: { status: running({ embeddable: null, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: null }] }) } });
    expect(appWin().querySelector("[data-embed-note]")?.textContent).toBe(
      "The app has not answered at the published address yet (it may be starting). Nants checks again and reloads the page when it answers.",
    );
    expectNoPortuguese();
  });

  it("sem especificação concluída e bloqueado pela prontidão", async () => {
    await mount("en", { status: "IN_PROGRESS", ui: {}, app: { logs: "" } }, ["app", "run"]);
    expect(emptyTitle()).toBe("Nothing to run yet");
    expect(within(appEmpty() as HTMLElement).getByText("The app can run once a spec is completed.")).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("waiting for completion");
    expect(document.getElementById(windowSubtitleId("execution"))?.textContent).toBe("waiting for completion");
    expect(runWin().textContent).toContain("No completed spec");
    expect(runWin().textContent).toContain("Available once a spec is completed.");
    expect(runWin().querySelector("[data-app-state]")?.textContent).toBe("Stopped");
    expect(within(runWin()).getByRole("log").textContent).toBe("No output yet.");
    expectNoPortuguese();
    cleanup();

    await mount("en", { extra: { readiness: notReady() } }, ["app", "run"]);
    expect(emptyTitle()).toBe("The machine needs setup");
    const empty = appEmpty() as HTMLElement;
    expect(empty.querySelector("p")?.textContent).toMatch(/^To start the app, you still need: .+\.$/);
    expect(within(empty).getByRole("button", { name: /^Set up: / })).toBeTruthy();
    expect(runWin().querySelector("[data-readiness-notice]")?.textContent).toContain("To run the app, you still need:");
    expectNoPortuguese();
  });

  it("ajuda da porta na rede (faixa e porta), com Recarregar e Já liberei", async () => {
    reachEnvironment.pageHostname = () => "192.168.0.68";
    await mount("en", { app: { status: running({ nants_port: 3000, lan_view_ports: { start: 3001, end: 3010 }, wsl: false }) } }, ["app", "run"]);
    const hint = appWin().querySelector("[data-reach-hint]") as HTMLElement;
    expect(hint.textContent).toContain(
      "Blank page on this device? The Nants preview ports (3001 to 3010) may not be allowed on the network of the computer where it runs. Allowing this range once is enough. After allowing them, use I already allowed it: Nants cannot check this on its own.",
    );
    expect(within(hint).getByRole("button", { name: "Reload" })).toBeTruthy();
    expect(within(hint).getByRole("button", { name: "I already allowed it" })).toBeTruthy();
    expect(runWin().querySelector("[data-port-hint]")?.textContent).toBe(
      "Blank page on this device? The preview ports (3001 to 3010) may not be allowed on the network.",
    );
    expectNoPortuguese();
  });

  it("funções do modelo no idioma ativo", () => {
    setLanguage("en", { persist: false });
    expect(appExitedText(1, "exited")).toBe("Ended (code 1): the program finished.");
    expect(appExitedText(null, "shutdown")).toBe("Ended: Nants was shut down.");
    expect(appExitedText(2, null)).toBe("Ended (code 2).");
    expect(appExitedText(null, null)).toBe("Ended.");
    expect(appExitedText(0, "stopped")).toBe("Stopped.");
    expect(appErrorMessage(409, "busy")).toBe("There is a run in progress in this project.");
    expect(appErrorMessage(400, undefined)).toBe("The run was not confirmed.");
    expect(appErrorMessage(500, undefined)).toBe("Could not complete the operation. Try again.");
    expect(runStateLabel({ kind: "other" })).toBe("Another spec is live");
    expect(rangeLabel({ start: 3001, end: 3010 })).toBe("3001 to 3010");
    expect(reachHintText("4173")).toContain("App port 4173 may not be allowed");
    setLanguage("pt-BR", { persist: false });
    expect(appExitedText(1, "exited")).toBe("Encerrado (código 1): o programa terminou.");
    expect(appExitedText(null, "shutdown")).toBe("Encerrado: o Nants foi encerrado.");
    expect(appExitedText(2, null)).toBe("Encerrado (código 2).");
    expect(appExitedText(0, "stopped")).toBe("Parado.");
    expect(rangeLabel({ start: 3001, end: 3010 })).toBe("3001 a 3010");
    expect(runStateLabel({ kind: "running", ports: [], embeddable: null })).toBe("No ar");
  });
});

describe("App e Execução em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora (estado, rótulos, subtítulos e o erro pelo code)", async () => {
    await mount("pt-BR", { app: { status: running() } }, ["app", "run"]);
    let run = runWin("pt-BR");
    expect(run.querySelector("[data-app-state]")?.textContent).toBe("No ar");
    expect(within(run).getByRole("button", { name: "Abrir no App" })).toBeTruthy();
    expect(within(run).getByRole("button", { name: "Copiar registro" })).toBeTruthy();
    expect(within(toolbar()).getByRole("button", { name: "Tamanho da tela: Livre" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("s1 · pronta para testar");
    expect(portugueseLeftovers(run).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    run = runWin("en");
    expect(run.querySelector("[data-app-state]")?.textContent).toBe("Live");
    expect(within(run).getByRole("button", { name: "Open in App" })).toBeTruthy();
    expect(within(run).getByRole("button", { name: "Copy log" })).toBeTruthy();
    expect(within(toolbar()).getByRole("button", { name: "Screen size: Free" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("s1 · ready to test");
    expectNoPortuguese();
  });

  it("erro pelo code: pt-BR pelo dicionário e troca ao vivo", async () => {
    await mount(
      "pt-BR",
      { app: { start: { status: 422, body: { error: "x", reason: "docker_port_in_use", detail: "d", code: "app_port_in_use" } } } },
      ["app", "run"],
    );
    await act(async () => {
      fireEvent.click(within(runWin("pt-BR")).getByRole("button", { name: "Iniciar" }));
    });
    await settle();
    await act(async () => {
      fireEvent.click(within(screen.getByRole("dialog", { name: "Iniciar o app?" })).getByRole("button", { name: "Executar" }));
    });
    await settle();
    expect(within(runWin("pt-BR")).getByRole("alert").textContent).toBe("Uma porta do app já está em uso neste computador.");
    expect(within(appEmpty() as HTMLElement).getByText("Uma porta do app já está em uso neste computador.")).toBeTruthy();
    await act(async () => {
      setLanguage("en", { persist: false });
    });
    expect(within(runWin()).getByRole("alert").textContent).toBe("An app port is already in use on this computer.");
    expect(within(appEmpty() as HTMLElement).getByText("An app port is already in use on this computer.")).toBeTruthy();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
    expectNoPortuguese();
  });
});
