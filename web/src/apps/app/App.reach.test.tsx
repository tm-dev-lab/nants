/*
 * Porta talvez não liberada na rede (spec 095, T09): no celular, pela rede, a porta do app pode não
 * chegar ao aparelho (o Nants no WSL, o Windows só encaminha a porta do Nants) e o iframe fica
 * branco. O navegador não consegue sondar isso (no Chrome, `fetch` no-cors para a rede local falha
 * sempre), então não há sonda: com a página do Nants aberta fora do loopback e o app no ar, uma
 * ajuda discreta abaixo do palco e na linha da porta da Execução, sem bloquear o iframe.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { settle } from "../../desktop/shell-test-utils";
import { APP_URL, STATUS_URL, STOPPED, appEmpty, appWindow, calls, execWindow, frame, mountRun, running } from "./app-test-utils";
import { reachEnvironment } from "./runSession";

const original = { ...reachEnvironment };

/** A página do Nants aberta pela rede (outro aparelho). */
function fromLan(): void {
  reachEnvironment.pageHostname = () => "192.168.0.68";
}

const hint = (): HTMLElement | null => appWindow().querySelector<HTMLElement>("[data-reach-hint]");

afterEach(() => {
  cleanup();
  Object.assign(reachEnvironment, original);
  vi.unstubAllGlobals();
});

describe("App: ajuda da porta na rede (095/T09)", () => {
  it("acesso local: o iframe como sempre, sem ajuda nem na App nem na Execução", async () => {
    await mountRun({ app: { status: running() }, open: ["App", "Execução"] });
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(hint()).toBeNull();
    expect(execWindow().querySelector("[data-port-hint]")).toBeNull();
  });

  it("pela rede: o iframe aparece na hora (nenhuma sonda) e a linha discreta fica abaixo do palco", async () => {
    fromLan();
    const view = await mountRun({ app: { status: running() } });
    const iframe = frame();
    expect(iframe.getAttribute("src")).toBe(APP_URL);
    expect(iframe.style.visibility).toBe("");
    expect(iframe.hasAttribute("aria-hidden")).toBe(false);
    expect(appEmpty()).toBeNull();
    // Nenhum pedido ao endereço do app (só a API do Nants).
    const probed = view.fetchMock.mock.calls.filter(([input]) => String(input).startsWith(APP_URL));
    expect(probed).toEqual([]);
    const line = hint();
    if (line === null) throw new Error("ajuda ausente");
    expect(line.textContent).toBe(
      "Página em branco neste aparelho? A porta 18080 do app pode não estar liberada na rede do computador onde o Nants roda. Peça a liberação dessa porta a quem administra o computador ou a rede.Recarregar",
    );
    // Abaixo do palco, não sobre ele.
    expect(line.previousElementSibling?.hasAttribute("data-app-stage")).toBe(true);
    // Só ajuda: sem primária, sem detalhes, sem comandos nem botões de copiar.
    expect(line.querySelector('[data-variant="primary"]')).toBeNull();
    expect(appWindow().querySelector("details")).toBeNull();
    expect(appWindow().querySelector("code")).toBeNull();
    expect(within(appWindow()).queryAllByRole("button", { name: /Copiar/ })).toEqual([]);
    expect(within(line).getAllByRole("button")).toHaveLength(1);
    expect(findForbiddenGlyphs(appWindow())).toEqual([]);
  });

  it("Recarregar da ajuda: recarrega o iframe e relê o estado", async () => {
    fromLan();
    const view = await mountRun({ app: { status: running() } });
    const before = frame();
    const line = hint();
    if (line === null) throw new Error("ajuda ausente");
    await act(async () => {
      fireEvent.click(within(line).getByRole("button", { name: "Recarregar" }));
    });
    await settle();
    expect(frame()).not.toBe(before);
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(calls(view, STATUS_URL)).toBe(2);
  });

  it("pela rede, sem o app no ar ou com a página bloqueada: nenhuma ajuda", async () => {
    fromLan();
    await mountRun({ app: { status: STOPPED } });
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("stopped");
    expect(hint()).toBeNull();
    cleanup();
    await mountRun({ app: { status: running({ embeddable: false, ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: false }] }) } });
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("embed_blocked");
    expect(hint()).toBeNull();
  });

  it("Execução pela rede: a linha da porta traz só a ajuda curta", async () => {
    fromLan();
    await mountRun({ app: { status: running() }, open: ["App", "Execução"] });
    const row = execWindow().querySelector(`[data-exec-port="${APP_URL}"]`);
    expect(row?.querySelector("[data-port-hint]")?.textContent).toBe(
      "Página em branco neste aparelho? A porta 18080 pode não estar liberada na rede.",
    );
    expect(findForbiddenGlyphs(execWindow())).toEqual([]);
  });
});
