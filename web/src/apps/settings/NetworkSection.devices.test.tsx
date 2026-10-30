/*
 * Configurações > Acesso pela rede (spec 112, T02): a jornada guiada por ambiente, o adicionar
 * dispositivo (código, contagem, QR, "Não abriu?", fechar, aviso de tentativas erradas) e a lista
 * (renomear, revogar, revogar todos), com a API e o fluxo de eventos simulados.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { NetworkConfig } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState, type RouteReply } from "../../test-utils";
import { reachEnvironment } from "../app/runSession";
import { devicesEnvironment } from "./DevicesStep";
import { networkRefresh } from "./NetworkSection";

const originalReach = { ...reachEnvironment };
const originalDevices = { ...devicesEnvironment };
const originalRefresh = { ...networkRefresh };

let clock = 1_000_000;

beforeEach(() => {
  clock = 1_000_000;
  devicesEnvironment.now = () => clock;
  devicesEnvironment.leave = vi.fn();
});

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, originalReach);
  Object.assign(devicesEnvironment, originalDevices);
  Object.assign(networkRefresh, originalRefresh);
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const RANGE = { start: 3001, end: 3010 };
const URL_A = "http://192.168.0.68:3000/connect";
const URL_B = "http://10.0.0.5:3000/connect";

function netConfig(over: Partial<NetworkConfig> = {}): NetworkConfig {
  return {
    env: "wsl_nat",
    saved_lan: true,
    lan: true,
    mode_source: "saved",
    port: 3000,
    lan_port: 3000,
    view_ports: RANGE,
    windows_redirect: "yes",
    windows_redirect_target: "this_wsl",
    firewall_rule: "yes",
    restart_required: false,
    restart_command: "nants",
    coherence: "ok",
    ...over,
  };
}

const DEVICES = [
  { id: "a1b2c3d4e5f60718", name: "Este computador", created_at: 1_760_000_000_000, last_seen: 1_760_000_600_000, this_computer: true, current: true },
  { id: "0011223344556677", name: "Safari no iPhone", created_at: 1_760_000_100_000, last_seen: 1_760_000_500_000, this_computer: false, current: false },
];

function openCode(code: string, secondsToNext = 42, urls = [URL_A, URL_B]) {
  return { state: "open", code, seconds_to_next: secondsToNext, expires_in: 600, entry_path: "/connect", entry_urls: urls, max_errors: 5 };
}

const CLOSED = { state: "closed", entry_path: "/connect", entry_urls: [URL_A], max_errors: 5 };

interface Api {
  net: NetworkConfig;
  devices: unknown[];
  code: unknown;
  /** Resposta do próximo `POST /api/devices/code`. */
  opened: unknown;
}

async function openMachine(api: Api) {
  reachEnvironment.pageHostname = () => "192.168.0.68";
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    ui: { p1: { rev: 1, data: { selection: { settingsTab: "machine" } } } },
  });
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [{ id: "git", label: "Git", ok: true }] }));
  view.server.reply("GET", /^\/api\/config\/network$/, () => ({ status: 200, body: api.net }));
  view.server.reply("GET", /^\/api\/devices$/, () => ({ status: 200, body: { devices: api.devices, current: "a1b2c3d4e5f60718" } }));
  view.server.reply("GET", /^\/api\/devices\/code$/, () => ({ status: 200, body: api.code }));
  view.server.reply("POST", /^\/api\/devices\/code$/, () => {
    api.code = api.opened;
    return { status: 200, body: api.opened };
  });
  view.server.reply("DELETE", /^\/api\/devices\/code$/, () => {
    api.code = CLOSED;
    return { status: 204 };
  });
  await act(async () => {
    activateApp("Configurações");
  });
  await settle();
  return view;
}

function section(): HTMLElement {
  const el = screen.getByRole("region", { name: "Configurações" }).querySelector<HTMLElement>("[data-network-section]");
  if (el === null) throw new Error("seção ausente");
  return el;
}

function step(id: string): HTMLElement {
  const el = section().querySelector<HTMLElement>(`[data-step='${id}']`);
  if (el === null) throw new Error(`passo ${id} ausente`);
  return el;
}

function addButton(): HTMLButtonElement {
  return within(step("add")).getByRole("button", { name: "Adicionar dispositivo" }) as HTMLButtonElement;
}

function api(over: Partial<Api> = {}): Api {
  return { net: netConfig(), devices: DEVICES, code: CLOSED, opened: openCode("123456"), ...over };
}

describe("jornada guiada por ambiente (112/T02)", () => {
  it("WSL NAT: ligar, preparar o Windows, adicionar; tudo pronto habilita o botão", async () => {
    await openMachine(api());
    const steps = within(section()).getByRole("list", { name: "Passos para abrir o Nants em outro aparelho" });
    expect([...steps.querySelectorAll(":scope > li")].map((li) => li.getAttribute("aria-label"))).toEqual([
      "Passo 1: Ligar a rede local",
      "Passo 2: Preparar o Windows",
      "Passo 3: Adicionar dispositivos",
    ]);
    expect(step("enable").getAttribute("data-step-status")).toBe("done");
    expect(step("windows").querySelector("[data-step-detail]")?.textContent).toBe("Pronto no Windows.");
    expect(step("windows").querySelector("details[data-network-script-block='enable']")).not.toBeNull();
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe("Pronto para adicionar.");
    expect(addButton().disabled).toBe(false);
    expect(findForbiddenGlyphs(section())).toEqual([]);
  });

  it("WSL espelhado: o passo 2 é o firewall; pendente bloqueia o adicionar com o motivo", async () => {
    await openMachine(api({ net: netConfig({ env: "wsl_mirrored", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "no", coherence: "missing_rule" }) }));
    expect(step("windows").getAttribute("aria-label")).toBe("Passo 2: Liberar o firewall do Windows");
    expect(step("windows").getAttribute("data-step-status")).toBe("pending");
    expect(step("windows").querySelector("[data-network-coherence]")?.textContent).toBe("Falta a regra de firewall Nants no Windows: rode o script de ligar.");
    expect(step("add").getAttribute("data-step-status")).toBe("blocked");
    const reason = step("add").querySelector("[data-step-detail]");
    expect(reason?.textContent).toBe("Crie a regra de firewall do passo 2 primeiro.");
    expect(addButton().disabled).toBe(true);
    expect(addButton().getAttribute("aria-describedby")).toBe(reason?.id);
  });

  it("Linux: só ligar e adicionar, com a dica curta do firewall", async () => {
    await openMachine(api({ net: netConfig({ env: "linux", windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "unknown" }) }));
    expect(section().querySelector("[data-step='windows']")).toBeNull();
    expect(step("add").getAttribute("aria-label")).toBe("Passo 2: Adicionar dispositivos");
    expect(step("enable").querySelector("[data-network-linux]")).not.toBeNull();
    expect(addButton().disabled).toBe(false);
  });

  it("rede desligada: o passo 2 e o adicionar esperam o passo 1, e a lista continua", async () => {
    await openMachine(api({ net: netConfig({ saved_lan: false, lan: false, windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "no" }) }));
    expect(step("enable").querySelector("[data-step-detail]")?.textContent).toBe("Desligada.");
    expect(step("windows").getAttribute("data-step-status")).toBe("blocked");
    expect(step("windows").querySelector("[data-step-detail]")?.textContent).toBe("Ligue a rede local no passo 1 primeiro.");
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe("Ligue a rede local no passo 1 primeiro.");
    expect(addButton().disabled).toBe(true);
    // Desligar mostra o script de desfazer; a lista e o Revogar todos continuam.
    expect(step("windows").querySelector("details[data-network-script-block='undo']")).not.toBeNull();
    expect(step("add").querySelectorAll("[data-device]")).toHaveLength(2);
    expect(within(step("add")).getByRole("button", { name: "Revogar todos" })).toBeTruthy();
  });

  it("salva ligada, sem reiniciar: o passo 1 fica pronto com a pendência discreta, o passo 2 libera e o adicionar pede o reinício", async () => {
    await openMachine(api({ net: netConfig({ lan: false, windows_redirect: "no", windows_redirect_target: "none", coherence: "needs_script" }) }));
    expect(step("enable").getAttribute("data-step-status")).toBe("done");
    expect(step("enable").querySelector("[data-step-detail]")?.textContent).toBe("Ligada; vale ao reiniciar (Ctrl+C e nants).");
    expect(section().querySelector("[data-network-restart]")).toBeNull();
    // O script do Windows não depende do reinício: aparece aberto enquanto o Windows não está pronto.
    expect(step("windows").getAttribute("data-step-status")).toBe("pending");
    expect(step("windows").querySelector("[data-step-detail]")?.textContent).toBe("Falta rodar o script de ligar no Windows.");
    expect(step("windows").querySelector("details[data-network-script-block='enable']")?.hasAttribute("open")).toBe(true);
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe("Rode o script do passo 2 primeiro.");
    expect(addButton().disabled).toBe(true);
    cleanup();
    // Script rodado, falta só reiniciar.
    await openMachine(api({ net: netConfig({ lan: false }) }));
    expect(step("windows").getAttribute("data-step-status")).toBe("done");
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe("Reinicie o Nants para valer a rede local.");
    expect(addButton().disabled).toBe(true);
  });

  it("execução com --lan e a opção salva desligada: passo 1 pronto e o adicionar habilitado", async () => {
    await openMachine(api({ net: netConfig({ saved_lan: false, lan: true, mode_source: "flag" }) }));
    expect(step("enable").getAttribute("data-step-status")).toBe("done");
    expect(step("enable").querySelector("[data-step-detail]")?.textContent).toBe("Ligada nesta execução.");
    expect(step("windows").getAttribute("data-step-status")).toBe("done");
    expect(step("add").getAttribute("data-step-status")).toBe("ready");
    expect(addButton().disabled).toBe(false);
  });

  it("releitura periódica durante a do interruptor: a coerência volta a aparecer", async () => {
    networkRefresh.intervalMs = 5;
    networkRefresh.visible = () => true;
    const state = api({ net: netConfig({ saved_lan: false, lan: false, windows_redirect: "no", windows_redirect_target: "none", firewall_rule: "no" }) });
    const view = await openMachine(state);
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let gets = 0;
    // A primeira leitura depois de gravar fica presa; a periódica seguinte a substitui.
    view.server.reply("PUT", /^\/api\/config\/network$/, () => {
      state.net = { ...state.net, saved_lan: true, coherence: "needs_script" };
      gets = 0;
      return { status: 200, body: { saved_lan: true, lan: false, applies_on_next_start: true, restart_required: true, restart_command: "nants" } };
    });
    view.server.reply("GET", /^\/api\/config\/network$/, () => {
      gets += 1;
      if (gets === 1) return gate.then(() => ({ status: 200, body: state.net })) as unknown as RouteReply;
      return { status: 200, body: state.net };
    });
    await act(async () => {
      fireEvent.click(within(section()).getByRole("switch", { name: "Acesso pela rede local" }));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 40));
    });
    await settle();
    release();
    await settle();
    expect(step("windows").querySelector("[data-network-coherence='needs_script']")?.textContent).toBe("Falta rodar o script de ligar no Windows.");
  });

  it("Windows não conferido: o passo 2 diz isso e o adicionar fica habilitado com a ressalva", async () => {
    await openMachine(api({ net: netConfig({ windows_redirect: "unknown", windows_redirect_target: "unknown", firewall_rule: "unknown", coherence: "unknown" }) }));
    expect(step("windows").querySelector("[data-step-detail]")?.textContent).toBe("Não deu para conferir o Windows.");
    expect(step("add").getAttribute("data-step-status")).toBe("unknown");
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe(
      "Não deu para conferir o Windows. Se o outro aparelho não abrir, confira o passo 2.",
    );
    expect(addButton().disabled).toBe(false);
  });

  it("volta automática: o Windows deixa de estar pronto e o passo 2 volta a pendente, com o script", async () => {
    networkRefresh.intervalMs = 5;
    networkRefresh.visible = () => true;
    const state = api();
    await openMachine(state);
    expect(step("windows").getAttribute("data-step-status")).toBe("done");
    state.net = netConfig({ windows_redirect_target: "other", coherence: "other_target" });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    await settle();
    expect(step("windows").getAttribute("data-step-status")).toBe("pending");
    // O aviso do servidor (mais preciso) entra no lugar do estado do passo.
    expect(step("windows").querySelector("[data-step-detail]")?.textContent).toBe(
      "O Windows redireciona a porta para outro endereço (o IP do WSL muda quando o Windows reinicia): rode o script de ligar de novo.",
    );
    expect(step("windows").querySelectorAll("[data-network-coherence]")).toHaveLength(1);
    expect(step("windows").querySelector("details[data-network-script-block='enable']")?.hasAttribute("open")).toBe(true);
    expect(step("add").querySelector("[data-step-detail]")?.textContent).toBe("Rode o script do passo 2 primeiro.");
    // Os dispositivos continuam na lista.
    expect(step("add").querySelectorAll("[data-device]")).toHaveLength(2);
  });
});

describe("adicionar dispositivo (112/T02)", () => {
  it("código grande, contagem, endereço com QR, outros endereços e Não abriu; fechar fecha a janela", async () => {
    const view = await openMachine(api());
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    expect(view.server.count("POST", /^\/api\/devices\/code$/)).toBe(1);
    const panel = step("add").querySelector<HTMLElement>("[data-device-code-state='open']");
    expect(panel).not.toBeNull();
    const p = panel as HTMLElement;
    expect(p.querySelector("[data-device-code]")?.textContent).toBe("123 456");
    expect(p.querySelector("[data-device-countdown]")?.textContent).toBe("Troca em 42 s");
    expect(p.querySelector("[data-device-entry-url]")?.textContent).toBe(URL_A);
    const qr = within(p).getByRole("img", { name: `QR code de ${URL_A}` });
    expect(qr.getAttribute("data-qr")).toBe(URL_A);
    expect(qr.querySelector("path")?.getAttribute("d")).toMatch(/^M\d/);
    // Trocar o endereço do QR.
    await act(async () => {
      fireEvent.click(within(p).getByRole("button", { name: `Usar ${URL_B} no QR code` }));
    });
    expect(within(p).getByRole("img", { name: `QR code de ${URL_B}` })).toBeTruthy();
    const help = p.querySelector("details[data-device-help]");
    expect(help?.querySelector("summary")?.textContent).toBe("Não abriu no outro aparelho?");
    expect([...(help?.querySelectorAll("li") ?? [])].map((li) => li.textContent)).toEqual([
      "O aparelho está em outra rede ou na rede de visitantes.",
      "O roteador isola os aparelhos entre si.",
      "O passo 2 está pendente.",
      "O endereço foi digitado errado. Prefira o QR code.",
    ]);
    expect(findForbiddenGlyphs(p)).toEqual([]);
    await act(async () => {
      fireEvent.click(within(p).getByRole("button", { name: "Fechar código" }));
    });
    await settle();
    expect(view.server.count("DELETE", /^\/api\/devices\/code$/)).toBe(1);
    expect(step("add").querySelector("[data-device-code-state]")).toBeNull();
    expect(addButton().disabled).toBe(false);
  });

  it("contagem: ao chegar a zero, relê e mostra o código novo", async () => {
    devicesEnvironment.tickMs = 5;
    const state = api({ opened: openCode("111222", 2) });
    await openMachine(state);
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    expect(step("add").querySelector("[data-device-countdown]")?.textContent).toBe("Troca em 2 s");
    clock += 1000;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect(step("add").querySelector("[data-device-countdown]")?.textContent).toBe("Troca em 1 s");
    state.code = openCode("333444", 60);
    clock += 1500;
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    await settle();
    expect(step("add").querySelector("[data-device-code]")?.textContent).toBe("333 444");
    expect(step("add").querySelector("[data-device-countdown]")?.textContent).toBe("Troca em 60 s");
  });

  it("tentativas erradas: o evento mostra o aviso e Gerar outro código reabre", async () => {
    const view = await openMachine(api());
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    act(() => {
      view.stream().emit("device_code_locked", { event_type: "device_code_locked", project_id: null, payload: { reason: "too_many_errors" } });
    });
    const locked = step("add").querySelector("[data-device-code-state='locked']");
    expect(within(locked as HTMLElement).getByRole("alert").textContent).toBe("Várias tentativas erradas. Gere outro código.");
    expect(step("add").querySelector("[data-device-code]")).toBeNull();
    await act(async () => {
      fireEvent.click(within(locked as HTMLElement).getByRole("button", { name: "Gerar outro código" }));
    });
    await settle();
    expect(view.server.count("POST", /^\/api\/devices\/code$/)).toBe(2);
    expect(step("add").querySelector("[data-device-code]")?.textContent).toBe("123 456");
  });

  it("o aparelho entrou: devices_changed relê a lista e a janela fechada vira Dispositivo adicionado", async () => {
    const state = api();
    const view = await openMachine(state);
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    state.code = CLOSED;
    state.devices = [...DEVICES, { id: "8899aabbccddeeff", name: "Chrome no Android", created_at: 1_760_000_700_000, last_seen: 1_760_000_700_000, this_computer: false, current: false }];
    const before = view.server.count("GET", /^\/api\/devices$/);
    act(() => {
      view.stream().emit("devices_changed", { event_type: "devices_changed", project_id: null, payload: { reason: "added" } });
    });
    await settle();
    expect(view.server.count("GET", /^\/api\/devices$/)).toBe(before + 1);
    expect(step("add").querySelectorAll("[data-device]")).toHaveLength(3);
    expect(step("add").querySelector("[data-device-code-state='added']")?.textContent).toContain("Dispositivo adicionado.");
  });

  it("falha ao gerar o código: mensagem e o botão continua", async () => {
    const view = await openMachine(api());
    view.server.reply("POST", /^\/api\/devices\/code$/, () => ({ status: 500, body: { error: "x", code: "device_store_failed" } }));
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    expect(step("add").querySelector("[data-device-code-error]")?.textContent).toBe(
      "Não foi possível gerar o código: Não foi possível gravar a lista de dispositivos.",
    );
    expect(addButton().disabled).toBe(false);
  });

  it("sem endereço da rede conhecido: o texto com o IP deste computador, sem QR", async () => {
    await openMachine(api({ opened: openCode("123456", 42, []) }));
    await act(async () => {
      fireEvent.click(addButton());
    });
    await settle();
    expect(step("add").querySelector("[data-device-no-address]")?.textContent).toBe("No outro aparelho, abra http://<IP deste computador>:3000/connect.");
    expect(step("add").querySelector("svg[data-qr]")).toBeNull();
  });
});

describe("lista de dispositivos (112/T02)", () => {
  it("nome, entrada, último acesso e Este dispositivo marcado", async () => {
    await openMachine(api());
    const list = within(step("add")).getByRole("list", { name: "Dispositivos autorizados" });
    const rows = [...list.querySelectorAll<HTMLElement>("[data-device]")];
    expect(rows.map((r) => r.querySelector("[data-device-name]")?.textContent)).toEqual(["Este computador", "Safari no iPhone"]);
    expect(rows[0]?.querySelector("[data-device-this]")?.textContent).toBe("Este dispositivo");
    expect(rows[1]?.querySelector("[data-device-this]")).toBeNull();
    expect(rows[1]?.querySelector("[data-device-meta]")?.textContent).toMatch(/^Entrou em .+, último acesso em .+$/);
  });

  it("renomear: PATCH com o nome e a lista relida; Esc cancela", async () => {
    const state = api();
    const view = await openMachine(state);
    const patches: unknown[] = [];
    view.server.reply("PATCH", /^\/api\/devices\/0011223344556677$/, (body) => {
      patches.push(body);
      state.devices = [DEVICES[0], { ...DEVICES[1], name: "iPhone da Ana" }];
      return { status: 200, body: { device: state.devices[1] } };
    });
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Renomear Safari no iPhone" }));
    });
    const input = within(step("add")).getByRole("textbox", { name: "Nome do dispositivo" });
    fireEvent.change(input, { target: { value: "  iPhone da Ana " } });
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Salvar" }));
    });
    await settle();
    expect(patches).toEqual([{ name: "iPhone da Ana" }]);
    expect([...step("add").querySelectorAll("[data-device-name]")].map((n) => n.textContent)).toEqual(["Este computador", "iPhone da Ana"]);
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Renomear Este computador" }));
    });
    fireEvent.keyDown(within(step("add")).getByRole("textbox", { name: "Nome do dispositivo" }), { key: "Escape" });
    expect(within(step("add")).queryByRole("textbox")).toBeNull();
  });

  it("revogar outro aparelho pede confirmação e relê a lista", async () => {
    const state = api();
    const view = await openMachine(state);
    view.server.reply("DELETE", /^\/api\/devices\/0011223344556677$/, () => {
      state.devices = [DEVICES[0]];
      return { status: 204 };
    });
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Revogar Safari no iPhone" }));
    });
    const dialog = screen.getByRole("alertdialog", { name: "Revogar Safari no iPhone?" });
    expect(dialog.textContent).toContain("O aparelho perde o acesso na hora.");
    // Cancelar não revoga.
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Cancelar" }));
    });
    expect(view.server.count("DELETE", /^\/api\/devices\/0011223344556677$/)).toBe(0);
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Revogar Safari no iPhone" }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Revogar" }));
    });
    await settle();
    expect(view.server.count("DELETE", /^\/api\/devices\/0011223344556677$/)).toBe(1);
    expect(step("add").querySelectorAll("[data-device]")).toHaveLength(1);
    expect(devicesEnvironment.leave).not.toHaveBeenCalled();
  });

  it("revogar este aparelho avisa e sai pela raiz", async () => {
    const view = await openMachine(api());
    view.server.reply("DELETE", /^\/api\/devices\/a1b2c3d4e5f60718$/, () => ({ status: 204 }));
    await act(async () => {
      fireEvent.click(within(step("add")).getByRole("button", { name: "Revogar Este computador" }));
    });
    const dialog = screen.getByRole("alertdialog", { name: "Revogar Este computador?" });
    expect(dialog.textContent).toContain("É este aparelho.");
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Revogar" }));
    });
    await settle();
    expect(devicesEnvironment.leave).toHaveBeenCalledTimes(1);
  });

  it("revogar todos: discreto, com confirmação", async () => {
    const view = await openMachine(api());
    view.server.reply("DELETE", /^\/api\/devices$/, () => ({ status: 200, body: { revoked: 2 } }));
    const all = within(step("add")).getByRole("button", { name: "Revogar todos" });
    expect(all.className).toBe("nd-link-btn");
    await act(async () => {
      fireEvent.click(all);
    });
    const dialog = screen.getByRole("alertdialog", { name: "Revogar todos os dispositivos?" });
    await act(async () => {
      fireEvent.click(within(dialog).getByRole("button", { name: "Revogar todos" }));
    });
    await settle();
    expect(view.server.count("DELETE", /^\/api\/devices$/)).toBe(1);
    expect(devicesEnvironment.leave).toHaveBeenCalledTimes(1);
  });

  it("lista vazia e falha de leitura", async () => {
    await openMachine(api({ devices: [] }));
    expect(step("add").querySelector("[data-device-list-empty]")?.textContent).toBe("Nenhum dispositivo ainda.");
    expect(within(step("add")).queryByRole("button", { name: "Revogar todos" })).toBeNull();
    cleanup();
    const view = await openMachine(api());
    view.server.reply("GET", /^\/api\/devices$/, (): RouteReply => ({ status: 500, body: {} }));
    // Força a releitura pela mudança na lista.
    act(() => {
      view.stream().emit("devices_changed", { event_type: "devices_changed", project_id: null, payload: { reason: "revoked" } });
    });
    await settle();
    expect(step("add").querySelector("[data-device-list-failed]")?.textContent).toBe("Não foi possível ler a lista de dispositivos.");
  });
});
