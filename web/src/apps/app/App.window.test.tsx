/*
 * Janela App montada pelo shell (spec 093, T09; 094, T05; 095, T03): SÓ o navegador.
 * - ferramentas no slot da barra de título (voltar, recarregar, endereço, tamanho, nova aba) e
 *   nenhuma barra de ação acima do conteúdo (decisão 10);
 * - o iframe (sandbox, guardas de origem, `embeddable` false/null, releitura até o app responder),
 *   o menu de tamanhos, a barra compacta, o DOM estável ao focar, mover, maximizar e minimizar,
 *   `pointer-events: none` durante gestos e nenhuma requisição montada minimizada;
 * - os estados vazios da C-10, um teste por estado: parado, subindo, sem especificação concluída,
 *   bloqueado pela prontidão, outra especificação no ar, encerrado, embed bloqueado e porta sem
 *   endereço (título, frase, uma primária e "Abrir Execução"; o detalhe técnico só na Execução);
 * - "Iniciar o app" no centro: abre a Execução com a confirmação e o App mostra "subindo".
 * O controle da execução e o console estão em `apps/execution/Execution.window.test.tsx`; as
 * regras da fonte única, em `runSession.test.tsx`.
 */
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, notReady, settle } from "../../desktop/shell-test-utils";
import { fakeResizeObservers, installPointerCapturePolyfill, manualScheduler, pointerEvent } from "../../desktop/test-utils";
import { WindowLayoutContext } from "../../desktop/useWindowVisibility";
import { windowTitleBarId } from "../../desktop/focus";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { resetStore } from "../../test-utils";
import { AppBrowser, AppRunner } from "./AppApp";
import {
  APP_URL,
  HOST,
  S,
  STATUS_URL,
  STOPPED,
  START_URL,
  STOP_URL,
  appEmpty,
  appWindow,
  calls,
  clickApp,
  confirmDialog,
  emit,
  frame,
  mountRun,
  running,
} from "./app-test-utils";
import { EMBED_RETRY_MS, IFRAME_SANDBOX } from "./appModel";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

function toolbar(win: HTMLElement = appWindow()): HTMLElement {
  return within(win).getByRole("toolbar", { name: "Navegador do app" });
}

/** Primárias visíveis (fora de `hidden`/`inert`). */
function primaries(root: Element): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>('[data-variant="primary"]')].filter((el) => el.closest("[hidden], [inert]") === null);
}

/** O estado vazio tem título 17/600 (h2 da classe da escala), uma frase, ≤ 1 primária e "Abrir Execução"; sem detalhe técnico. */
function expectEmptyShape(kind: string, title: string): HTMLElement {
  const empty = appEmpty();
  expect(empty?.getAttribute("data-app-empty")).toBe(kind);
  if (empty === null) throw new Error("estado vazio ausente");
  const heading = within(empty).getByRole("heading", { level: 2 });
  expect(heading.textContent).toBe(title);
  expect(heading.className).toBe("nd-app-empty-title");
  expect(empty.querySelectorAll("p")).toHaveLength(1);
  expect(within(empty).getByRole("button", { name: "Abrir Execução" })).toBeTruthy();
  expect(primaries(appWindow()).length).toBeLessThanOrEqual(1);
  // O detalhe técnico fica só na Execução.
  const win = appWindow();
  expect(win.querySelector("details")).toBeNull();
  expect(win.querySelector("[data-run-command]")).toBeNull();
  expect(within(win).queryByRole("log")).toBeNull();
  expect(findForbiddenGlyphs(win)).toEqual([]);
  return empty;
}

describe("App: só navegador (decisão 10)", () => {
  it("ferramentas no slot da barra de título, na ordem do mockup; nenhuma barra de ação acima do conteúdo", async () => {
    await mountRun({ app: { status: running() } });
    const win = appWindow();
    const slot = document.getElementById(windowTitleBarId("app"))?.querySelector("[data-window-tools]");
    const bar = toolbar();
    expect(slot?.contains(bar)).toBe(true);
    const labels = [...bar.querySelectorAll("button, input, a")].map((el) => el.getAttribute("aria-label"));
    expect(labels).toEqual(["Voltar", "Recarregar", "Endereço do app", "Tamanho da tela: Livre", "Abrir em nova aba"]);
    // O conteúdo da janela é só o palco: nada de painel de execução, abas ou console.
    const content = win.querySelector<HTMLElement>("[data-window-content]");
    expect(content?.querySelector("[data-run-panel]")).toBeNull();
    expect(content?.querySelector("[role='tablist']")).toBeNull();
    expect(content?.querySelector("[role='toolbar']")).toBeNull();
    expect(within(win).queryByRole("button", { name: "Parar" })).toBeNull();
    expect(frame().closest("[data-app-stage]")?.parentElement?.getAttribute("data-app-browser")).toBe("");
  });

  it("subtítulo com a especificação concluída", async () => {
    await mountRun();
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("s1 · pronta para testar");
  });
});

describe("App: estados vazios (C-10)", () => {
  it("parado: 'O app não está rodando', Iniciar o app (primária) e Abrir Execução", async () => {
    await mountRun();
    const empty = expectEmptyShape("stopped", "O app não está rodando");
    expect(within(empty).getByText("Inicie o app na janela Execução. Assim que ele responder, a página aparece aqui.")).toBeTruthy();
    const start = within(empty).getByRole("button", { name: "Iniciar o app" });
    expect(start.getAttribute("data-variant")).toBe("primary");
    expect(within(empty).getByRole("button", { name: "Abrir Execução" }).getAttribute("data-variant")).toBeNull();
    // Sem página: o endereço vazio e os controles de navegação desabilitados.
    expect((within(toolbar()).getByRole("textbox", { name: "Endereço do app" }) as HTMLInputElement).value).toBe("");
    expect((within(toolbar()).getByRole("button", { name: "Voltar" }) as HTMLButtonElement).disabled).toBe(true);
    expect((within(toolbar()).getByRole("button", { name: "Recarregar" }) as HTMLButtonElement).disabled).toBe(true);
    expect(appWindow().querySelector("iframe")).toBeNull();
  });

  it("Iniciar o app (decisão 10): abre a Execução com a confirmação; o botão some e o App mostra 'subindo' (com progresso, sem console) até a página aparecer", async () => {
    const view = await mountRun({ app: { holdStart: true } });
    expect(screen.queryByRole("region", { name: "Execução" })).toBeNull();
    await act(async () => {
      fireEvent.click(within(appWindow()).getByRole("button", { name: "Iniciar o app" }));
    });
    await settle();
    // A Execução abriu, com a confirmação de sempre; nada foi enviado ainda.
    expect(screen.getByRole("region", { name: "Execução" })).toBeTruthy();
    expect(screen.getByRole("dialog", { name: "Iniciar o app?" })).toBeTruthy();
    expect(calls(view, START_URL)).toBe(0);
    let empty = expectEmptyShape("starting", "O app está subindo");
    expect(within(empty).queryByRole("button", { name: "Iniciar o app" })).toBeNull();
    expect(within(empty).getByText(/Confirme o início na janela Execução/)).toBeTruthy();
    await confirmDialog();
    expect(calls(view, START_URL)).toBe(1);
    empty = expectEmptyShape("starting", "O app está subindo");
    expect(within(empty).getByRole("progressbar", { name: "Subindo o app" })).toBeTruthy();
    expect(within(empty).getByText(/O ambiente Docker está subindo/)).toBeTruthy();
    // Sem outra ação, "Abrir Execução" é a primária.
    expect(within(empty).getByRole("button", { name: "Abrir Execução" }).getAttribute("data-variant")).toBe("primary");
    await act(async () => {
      view.app.releaseStart?.();
    });
    await settle();
    expect(appEmpty()).toBeNull();
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });

  it("Iniciar o app com a confirmação cancelada: o App volta a 'parado'", async () => {
    const view = await mountRun();
    await act(async () => {
      fireEvent.click(within(appWindow()).getByRole("button", { name: "Iniciar o app" }));
    });
    await settle();
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancelar" }));
    await settle();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
    expect(calls(view, START_URL)).toBe(0);
  });

  it("sem especificação concluída: Iniciar o app desabilitado com o motivo ligado; subtítulo de espera", async () => {
    await mountRun({ status: "IN_PROGRESS", ui: {} });
    const empty = expectEmptyShape("no_spec", "Nada para executar ainda");
    const start = within(empty).getByRole("button", { name: "Iniciar o app" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    const messages = within(appWindow()).getAllByText("O app pode ser executado quando uma especificação for concluída.");
    expect(messages).toHaveLength(1);
    expect(start.getAttribute("aria-describedby")).toBe(messages[0]?.id);
    expect(document.getElementById(windowSubtitleId("app"))?.textContent).toBe("aguardando a conclusão");
  });

  it("bloqueado pela prontidão: o que falta e Configurar (abre as Configurações)", async () => {
    await mountRun({ extra: { readiness: notReady() } });
    const empty = expectEmptyShape("blocked", "Falta configurar a máquina");
    expect(within(empty).getByText("Para iniciar o app, falta: Conexão com um provedor, Arquiteto, Docker daemon.")).toBeTruthy();
    // A primária leva à primeira pendência; a lista com um Configurar por item fica na Execução.
    const configure = within(empty).getByRole("button", { name: "Configurar: Conexão com um provedor" });
    expect(configure.getAttribute("data-variant")).toBe("primary");
    expect(within(empty).queryByRole("button", { name: "Iniciar o app" })).toBeNull();
    await act(async () => {
      fireEvent.click(configure);
    });
    await settle();
    expect(screen.getByRole("region", { name: "Configurações" })).toBeTruthy();
  });

  it("outra especificação no ar: Parar (perigo) e Abrir Execução, sem iframe", async () => {
    const view = await mountRun({ app: { status: running({ spec_id: "s0" }) } });
    const empty = expectEmptyShape("other", "Outra especificação está no ar");
    expect(appWindow().querySelector("iframe")).toBeNull();
    const stop = within(empty).getByRole("button", { name: "Parar" });
    expect(stop.getAttribute("data-variant")).toBe("danger");
    await act(async () => {
      fireEvent.click(stop);
    });
    await settle();
    expect(calls(view, STOP_URL)).toBe(1);
  });

  it("encerrado: o código e o motivo, Executar novamente (abre a Execução com a confirmação)", async () => {
    await mountRun({ app: { status: running() } });
    emit("app_exited", { code: 1, reason: "exited" });
    const empty = expectEmptyShape("exited", "O app parou");
    expect(within(empty).getByText("Encerrado (código 1): o programa terminou.")).toBeTruthy();
    expect(appWindow().querySelector("iframe")).toBeNull();
    await act(async () => {
      fireEvent.click(within(empty).getByRole("button", { name: "Executar novamente" }));
    });
    await settle();
    expect(screen.getByRole("region", { name: "Execução" })).toBeTruthy();
    expect(screen.getByRole("dialog", { name: "Iniciar o app?" })).toBeTruthy();
  });

  it("embed bloqueado (embeddable false): explica, Abrir em nova aba (primária) e Abrir Execução", async () => {
    await mountRun({ app: { status: running({ embeddable: false, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: false }] }) } });
    const win = appWindow();
    expect(win.querySelector("iframe")).toBeNull();
    const empty = expectEmptyShape("embed_blocked", "A página não pode ser exibida aqui");
    expect(empty.getAttribute("data-embed-blocked")).toBe("not_embeddable");
    expect(within(empty).getByText(/não permite ser exibido dentro de outra página/)).toBeTruthy();
    const open = within(empty).getByRole("link", { name: "Abrir em nova aba" });
    expect(open.getAttribute("href")).toBe(APP_URL);
    expect(open.getAttribute("rel")).toBe("noopener noreferrer");
    expect(open.getAttribute("data-variant")).toBe("primary");
  });

  it("porta sem endereço: no ar sem porta publicada; Abrir Execução é a primária", async () => {
    await mountRun({ app: { status: running({ ports: [], embeddable: null }) } });
    const empty = expectEmptyShape("no_address", "O app está no ar sem endereço");
    expect(within(empty).getByText(/não publicou nenhuma porta com endereço/)).toBeTruthy();
    const open = within(empty).getByRole("button", { name: "Abrir Execução" });
    expect(open.getAttribute("data-variant")).toBe("primary");
    await act(async () => {
      fireEvent.click(open);
    });
    await settle();
    expect(screen.getByRole("region", { name: "Execução" })).toBeTruthy();
  });
});

describe("App: estado do servidor", () => {
  it("no ar e Recarregar relê o app parado (app_exited perdido): adota o estado do servidor (T10)", async () => {
    const view = await mountRun({ app: { status: running() } });
    expect(frame().getAttribute("src")).toBe(APP_URL);
    view.app.status = STOPPED;
    fireEvent.click(within(toolbar()).getByRole("button", { name: "Recarregar" }));
    await settle();
    expect(calls(view, STATUS_URL)).toBe(2);
    expect(appWindow().querySelector("iframe")).toBeNull();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
  });

  it("app_started (outra aba): relê o estado para as portas e embute", async () => {
    const view = await mountRun();
    view.app.status = running();
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    expect(calls(view, STATUS_URL)).toBe(2);
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });
});

describe("App: minimizada ao montar", () => {
  it("nenhuma requisição (estado do app) enquanto minimizada; ao restaurar, consulta uma vez", async () => {
    const minimized = {
      layouts: {
        done: {
          current: {
            windows: {
              conversation: { x: 0, y: 0, w: 500, h: 400, z: 1, mode: "normal", open: true },
              app: { x: 40, y: 40, w: 700, h: 500, z: 2, mode: "minimized", open: true, restoreMode: "normal" },
            },
          },
          mine: null,
        },
      },
    };
    const view = await mountRun({ ui: minimized, open: [], app: { status: running() } });
    const win = document.querySelector<HTMLElement>('[data-window="app"]');
    expect(win?.hidden).toBe(true);
    expect(view.fetchMock.mock.calls.some(([input]) => /\/app(\/|$)/.test(String(input)))).toBe(false);
    await clickApp("App");
    expect(calls(view, STATUS_URL)).toBe(1);
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });
});

describe("App: navegador interno", () => {
  it("iframe com o sandbox exato apontando para o primeiro link; endereço somente leitura, Recarregar e nova aba", async () => {
    await mountRun({
      app: {
        status: running({
          ports: [
            { service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: true },
            { service: "api", target: 9000, published: 19000, url: `http://${HOST}:19000`, embeddable: true },
          ],
        }),
      },
    });
    const win = appWindow();
    const iframe = frame();
    expect(iframe.getAttribute("sandbox")).toBe(IFRAME_SANDBOX);
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts allow-forms allow-same-origin allow-popups allow-popups-to-escape-sandbox");
    expect(iframe.getAttribute("src")).toBe(APP_URL);
    const bar = toolbar();
    const address = within(bar).getByRole("textbox", { name: "Endereço do app" }) as HTMLInputElement;
    expect(address.readOnly).toBe(true);
    expect(address.value).toBe(APP_URL);
    const open = within(bar).getByRole("link", { name: "Abrir em nova aba" });
    expect(open.getAttribute("href")).toBe(APP_URL);
    expect(open.getAttribute("target")).toBe("_blank");
    expect(open.getAttribute("rel")).toBe("noopener noreferrer");
    expect(within(win).queryByRole("note")).toBeNull();
    // Recarregar troca o iframe (recarga explícita).
    fireEvent.click(within(bar).getByRole("button", { name: "Recarregar" }));
    expect(frame()).not.toBe(iframe);
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("Voltar: volta no histórico do iframe; sem acesso (outra origem), volta à página inicial do app", async () => {
    await mountRun({ app: { status: running() } });
    const iframe = frame();
    const history = iframe.contentWindow?.history;
    if (history === undefined) throw new Error("histórico ausente");
    const back = vi.spyOn(history, "back").mockImplementation(() => undefined);
    fireEvent.click(within(toolbar()).getByRole("button", { name: "Voltar" }));
    expect(back).toHaveBeenCalledTimes(1);
    expect(frame()).toBe(iframe);
    // Outra origem: o histórico lança; o iframe volta ao endereço inicial.
    back.mockImplementation(() => {
      throw new DOMException("cross-origin", "SecurityError");
    });
    fireEvent.click(within(toolbar()).getByRole("button", { name: "Voltar" }));
    expect(frame()).not.toBe(iframe);
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });

  it("embeddable null: tenta embutir, avisa que confere de novo e mantém Abrir em nova aba", async () => {
    await mountRun({ app: { status: running({ embeddable: null, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: null }] }) } });
    const win = appWindow();
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(within(win).getByRole("note").textContent).toContain("O Nants confere de novo");
    expect(within(toolbar()).getByRole("link", { name: "Abrir em nova aba" })).toBeTruthy();
  });

  it("mesma origem do Nants: não embute (guarda do cliente)", async () => {
    const own = `http://${HOST}:${window.location.port}`;
    await mountRun({ app: { status: running({ ports: [{ service: "web", target: 80, published: Number(window.location.port), url: own, embeddable: true }] }) } });
    const win = appWindow();
    expect(win.querySelector("iframe")).toBeNull();
    expect(win.querySelector("[data-embed-blocked]")?.getAttribute("data-embed-blocked")).toBe("same_origin");
  });

  it("outro host: não embute (fora de uma janela, as ferramentas ficam no lugar)", () => {
    render(<AppBrowser url="http://evil.example:18080" embeddable={true} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} />);
    expect(document.querySelector("iframe")).toBeNull();
    expect(document.querySelector("[data-embed-blocked]")?.getAttribute("data-embed-blocked")).toBe("other_host");
    for (const link of screen.getAllByRole("link", { name: "Abrir em nova aba" })) expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    expect(screen.getByRole("toolbar", { name: "Navegador do app" })).toBeTruthy();
  });

  it("https e esquemas estranhos não embutem; javascript: nem vira link", () => {
    const view = render(<AppBrowser url={`https://${HOST}:18080`} embeddable={true} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} />);
    expect(document.querySelector("[data-embed-blocked]")?.getAttribute("data-embed-blocked")).toBe("not_http");
    view.unmount();
    render(<AppBrowser url="javascript:alert(1)" embeddable={true} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} />);
    expect(document.querySelector("iframe")).toBeNull();
    expect(screen.queryByRole("link")).toBeNull();
  });

  it("menu de tamanho: Livre, Desktop 1440, Tablet 768 e Celular 390; escala por transform para caber; a escolha vai para selection", async () => {
    const observers = fakeResizeObservers();
    const view = await mountRun({ app: { status: running() }, extra: { createObserver: observers.factory } });
    await act(async () => observers.resize(720));
    const iframe = frame();
    expect(iframe.style.width).toBe("100%");
    expect(iframe.style.transform).toBe("");
    const pick = async (label: string): Promise<void> => {
      await act(async () => {
        fireEvent.click(within(toolbar()).getByRole("button", { name: /^Tamanho da tela/ }));
      });
      const menu = screen.getByRole("menu", { name: "Tamanho da tela" });
      expect(within(menu).getAllByRole("menuitemradio").map((r) => r.textContent)).toEqual(["Livre", "Desktop 1440", "Tablet 768", "Celular 390"]);
      await act(async () => {
        fireEvent.click(within(menu).getByRole("menuitemradio", { name: label }));
      });
      await settle();
    };
    expect(within(toolbar()).getByRole("button", { name: "Tamanho da tela: Livre" }).getAttribute("aria-haspopup")).toBe("menu");

    await pick("Desktop 1440");
    expect(frame()).toBe(iframe);
    expect(iframe.style.width).toBe("1440px");
    expect(iframe.style.transform).toBe("scale(0.5)");
    expect(iframe.style.height).toBe("200%");
    expect(within(toolbar()).getByRole("button", { name: "Tamanho da tela: Desktop 1440" })).toBeTruthy();
    await waitFor(() => {
      const put = view.server.calls.filter((c) => c.method === "PUT" && (c.body as { section?: string }).section === "selection").at(-1);
      expect((put?.body as { value: { appViewport: string } } | undefined)?.value.appViewport).toBe("desktop");
    });

    await pick("Tablet 768");
    expect(iframe.style.width).toBe("768px");
    expect(iframe.style.transform).toBe("scale(0.9375)");

    await pick("Celular 390");
    expect(iframe.style.width).toBe("390px");
    expect(iframe.style.transform).toBe("");
    expect(iframe.style.height).toBe("100%");
    expect(frame()).toBe(iframe);
    expect(iframe.getAttribute("src")).toBe(APP_URL);
  });

  it("o tamanho salvo volta ao montar", async () => {
    await mountRun({
      app: { status: running() },
      ui: {
        layouts: { done: { current: { windows: { conversation: { x: 0, y: 0, w: 500, h: 400, z: 1, mode: "normal", open: true } } }, mine: null } },
        selection: { appViewport: "phone" },
      },
    });
    expect(within(toolbar()).getByRole("button", { name: "Tamanho da tela: Celular 390" })).toBeTruthy();
    expect(frame().style.width).toBe("390px");
  });

  it("barra larga, depois do teto de releituras: um só Abrir em nova aba (o da barra)", () => {
    render(<AppBrowser url={APP_URL} embeddable={null} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} />);
    const note = screen.getByRole("note");
    expect(note.getAttribute("data-embed-note")).toBe("unconfirmed");
    expect(within(note).queryByRole("link")).toBeNull();
    const links = screen.getAllByRole("link", { name: "Abrir em nova aba" });
    expect(links).toHaveLength(1);
    expect(within(screen.getByRole("toolbar", { name: "Navegador do app" })).getByRole("link", { name: "Abrir em nova aba" })).toBe(links[0]);
  });
});

describe("App: iframe branco no celular (094/T05)", () => {
  it("sem resposta do app (embeddable null): relê o estado e, quando o app responde, recarrega o iframe uma vez", async () => {
    const pending = running({ embeddable: null, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: null }] });
    const view = await mountRun({ app: { status: pending }, open: [] });
    // Relógio controlado antes de a janela abrir (o primeiro intervalo já nasce falso).
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    await act(async () => {
      activateApp("App");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
    const first = frame();
    expect(appWindow().querySelector("[data-embed-note]")?.getAttribute("data-embed-note")).toBe("probing");
    const before = calls(view, STATUS_URL);
    expect(before).toBe(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EMBED_RETRY_MS + 10);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
    expect(calls(view, STATUS_URL)).toBe(before + 1);
    expect(frame()).toBe(first);
    // O servidor do app passou a responder.
    view.app.status = running();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EMBED_RETRY_MS + 10);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10);
    });
    expect(calls(view, STATUS_URL)).toBe(before + 2);
    expect(frame()).not.toBe(first);
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(appWindow().querySelector("[data-embed-note]")).toBeNull();
    // Com o veredito, a releitura para.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EMBED_RETRY_MS * 3);
    });
    expect(calls(view, STATUS_URL)).toBe(before + 2);
  });

  it("veredito indeterminado depois do teto: explica e oferece Abrir em nova aba na própria explicação", () => {
    render(<AppBrowser url={APP_URL} embeddable={null} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} compact />);
    const note = screen.getByRole("note");
    expect(note.getAttribute("data-embed-note")).toBe("unconfirmed");
    expect(note.textContent).toContain("Não foi possível confirmar");
    expect(note.textContent).toContain("127.0.0.1");
    const link = within(note).getByRole("link", { name: "Abrir em nova aba" });
    expect(link.getAttribute("href")).toBe(APP_URL);
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    expect(link.getAttribute("target")).toBe("_blank");
  });
});

describe("App: barra compacta no celular (094/T05)", () => {
  it("janela estreita: endereço numa linha; Voltar, Recarregar, nova aba e tamanhos num menu", async () => {
    const observers = fakeResizeObservers();
    const view = await mountRun({ app: { status: running() }, extra: { createObserver: observers.factory } });
    await act(async () => observers.resize(390));
    const bar = toolbar();
    expect(bar.getAttribute("data-compact")).toBe("true");
    expect(within(bar).getByRole("textbox", { name: "Endereço do app" })).toBeTruthy();
    expect(within(bar).queryByRole("button", { name: "Recarregar" })).toBeNull();
    expect(within(bar).queryByRole("link")).toBeNull();
    // Dois controles só: o endereço e o botão do menu.
    expect(Array.from(bar.children).map((el) => el.tagName)).toEqual(["INPUT", "BUTTON"]);
    const more = within(bar).getByRole("button", { name: "Ações do navegador" });
    expect(more.getAttribute("aria-haspopup")).toBe("menu");
    await act(async () => {
      fireEvent.click(more);
    });
    const menu = screen.getByRole("menu", { name: "Navegador do app" });
    expect(within(menu).getByRole("menuitem", { name: "Voltar" })).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: "Recarregar" })).toBeTruthy();
    const open = within(menu).getByRole("menuitem", { name: "Abrir em nova aba" });
    expect(open.getAttribute("href")).toBe(APP_URL);
    expect(open.getAttribute("target")).toBe("_blank");
    expect(open.getAttribute("rel")).toBe("noopener noreferrer");
    const radios = within(menu).getAllByRole("menuitemradio");
    expect(radios.map((r) => r.textContent)).toEqual(["Livre", "Desktop 1440", "Tablet 768", "Celular 390"]);
    expect(within(menu).getByRole("menuitemradio", { name: "Livre" }).getAttribute("aria-checked")).toBe("true");
    expect(findForbiddenGlyphs(menu)).toEqual([]);
    const iframe = frame();
    await act(async () => {
      fireEvent.click(within(menu).getByRole("menuitemradio", { name: "Celular 390" }));
    });
    await settle();
    expect(screen.queryByRole("menu")).toBeNull();
    expect(frame()).toBe(iframe);
    expect(iframe.style.width).toBe("390px");
    await waitFor(() => {
      const put = view.server.calls.filter((c) => c.method === "PUT" && (c.body as { section?: string }).section === "selection").at(-1);
      expect((put?.body as { value: { appViewport: string } } | undefined)?.value.appViewport).toBe("phone");
    });
    await act(async () => {
      fireEvent.click(within(bar).getByRole("button", { name: "Ações do navegador" }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu", { name: "Navegador do app" })).getByRole("menuitem", { name: "Recarregar" }));
    });
    expect(frame()).not.toBe(iframe);
    // Larga de novo: a barra completa volta.
    await act(async () => observers.resize(1000));
    expect(within(toolbar()).getByRole("button", { name: "Recarregar" })).toBeTruthy();
  });

  it("estreita e bloqueado: o estado vazio traz Abrir em nova aba (a barra não mostra o link)", () => {
    render(<AppBrowser url="http://evil.example:18080" embeddable={true} viewport="free" onViewport={() => undefined} reloadKey={0} onReload={() => undefined} compact />);
    const blocked = document.querySelector<HTMLElement>("[data-embed-blocked]");
    expect(blocked?.getAttribute("data-embed-blocked")).toBe("other_host");
    expect(within(blocked as HTMLElement).getByRole("link", { name: "Abrir em nova aba" }).getAttribute("href")).toBe("http://evil.example:18080");
    expect(within(screen.getByRole("toolbar", { name: "Navegador do app" })).queryByRole("link")).toBeNull();
  });
});

describe("App: ciclo da janela (DOM estável)", () => {
  it("focar outra janela, maximizar, restaurar, minimizar e restaurar: o mesmo iframe, sem mudar o src", async () => {
    const view = await mountRun({ app: { status: running() } });
    const iframe = frame();
    const setSrc = vi.fn();
    const proto = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, "src");
    Object.defineProperty(iframe, "src", {
      configurable: true,
      get: () => proto?.get?.call(iframe) as string,
      set: (value: string) => {
        setSrc(value);
        proto?.set?.call(iframe, value);
      },
    });
    const observer = new MutationObserver(() => undefined);
    observer.observe(iframe, { attributes: true, attributeFilter: ["src"] });
    const win = appWindow();
    await act(async () => {
      fireEvent.pointerDown(screen.getByRole("region", { name: "Conversa" }));
    });
    await act(async () => {
      fireEvent.pointerDown(win);
    });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Maximizar" }));
    });
    await settle();
    expect(win.getAttribute("data-mode")).toBe("maximized");
    expect(frame()).toBe(iframe);
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Restaurar" }));
    });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Minimizar" }));
    });
    await settle();
    expect(win.hidden).toBe(true);
    expect(iframe.isConnected).toBe(true);
    await clickApp("App");
    expect(win.hidden).toBe(false);
    expect(frame()).toBe(iframe);
    expect(iframe.getAttribute("src")).toBe(APP_URL);
    expect(setSrc).not.toHaveBeenCalled();
    expect(observer.takeRecords()).toEqual([]);
    observer.disconnect();
    // O estado do app foi lido uma vez só (o ciclo da janela não relê nada).
    expect(calls(view, STATUS_URL)).toBe(1);
  });

  it("mover pela barra: o mesmo iframe; durante o gesto, pointer-events none (o arrasto não começa nas ferramentas)", async () => {
    const restoreCapture = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      await mountRun({ app: { status: running() }, extra: { scheduler } });
      const iframe = frame();
      const win = appWindow();
      const bar = document.getElementById(windowTitleBarId("app"));
      if (bar === null) throw new Error("barra ausente");
      const left = win.style.left;
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 60 }));
        bar.dispatchEvent(pointerEvent("pointermove", { clientX: 360, clientY: 120 }));
      });
      act(() => {
        scheduler.runFrames();
      });
      expect(iframe.style.pointerEvents).toBe("none");
      expect(document.querySelector(".nd-root")?.getAttribute("data-gesture")).toBe("true");
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerup", { clientX: 360, clientY: 120 }));
      });
      await settle();
      expect(win.style.left).not.toBe(left);
      expect(frame()).toBe(iframe);
      expect(iframe.style.pointerEvents).toBe("");
      expect(iframe.getAttribute("src")).toBe(APP_URL);
    } finally {
      restoreCapture();
    }
  });
});

describe("App: respostas atrasadas", () => {
  it("resposta do estado de outro projeto/especificação chegada depois da troca é descartada", async () => {
    resetStore();
    const held: Array<() => void> = [];
    const urls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((input: string | URL | Request) => {
        urls.push(String(input));
        return new Promise<Response>((resolve) => held.push(() => resolve(new Response(JSON.stringify(running()), { status: 200 }))));
      }),
    );
    const layout = { windows: { app: { x: 0, y: 0, w: 600, h: 400, z: 1, mode: "normal" as const, open: true } } };
    const runner = (projectId: string) => (
      <WindowLayoutContext.Provider value={layout}>
        <AppRunner key={projectId} projectId={projectId} specId={S} size="wide" viewport="free" setSelection={() => undefined} />
      </WindowLayoutContext.Provider>
    );
    const rendered = render(runner("p1"));
    await settle();
    // Spec 130: o "Como usar" e as provas (`/app/guide/<spec>`) são outra leitura.
    const statusUrls = (): string[] => urls.filter((u) => u.endsWith("/app"));
    const heldOf = (url: string): (() => void) | undefined => held[urls.indexOf(url)];
    expect(statusUrls()).toEqual(["/api/projects/p1/app"]);
    rendered.rerender(runner("p2"));
    await settle();
    expect(statusUrls()).toEqual(["/api/projects/p1/app", "/api/projects/p2/app"]);
    // Só a resposta da montagem antiga (p1) é liberada; a de p2 continua segurada.
    await act(async () => {
      heldOf("/api/projects/p1/app")?.();
    });
    await settle();
    expect(rendered.container.querySelector("iframe")).toBeNull();
    // Parado, com o guia ainda segurado: o estado neutro da spec 131 T05 (sem "Iniciar").
    expect(rendered.container.querySelector('[data-runview="loading"]')).not.toBeNull();
    // A resposta atual (p2) vale.
    await act(async () => {
      heldOf("/api/projects/p2/app")?.();
    });
    await settle();
    expect(rendered.container.querySelector("iframe")?.getAttribute("src")).toBe(APP_URL);
  });
});
