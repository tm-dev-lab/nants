/*
 * Página de entrada (spec 112, T02): logo e slogan logo depois dela, o campo numérico do código,
 * o acerto (vai para `/` ou para o `next` validado, spec 128), as mensagens de erro de cada caso,
 * o link expirado e o aparelho que já está autorizado.
 */
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { deviceAuth } from "../api/client";
import { BRAND_NAME, BRAND_SLOGAN } from "../desktop/brand";
import { findForbiddenGlyphs } from "../desktop/glyphs";
import { setLanguage } from "../i18n";
import { installFetch, type RouteReply } from "../test-utils";
import { ConnectPage, connectEnvironment, entryReason, entryTarget } from "./ConnectPage";

const original = { ...connectEnvironment };

beforeEach(() => {
  connectEnvironment.enter = vi.fn();
  connectEnvironment.search = () => "";
  // A consulta de abertura recebe 401 aqui; a navegação do cliente fica de fora.
  vi.spyOn(deviceAuth, "onDeviceRequired").mockImplementation(() => undefined);
});

afterEach(() => {
  cleanup();
  Object.assign(connectEnvironment, original);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  setLanguage("pt-BR", { persist: false, storage: null });
});

async function flush(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 10; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function setup(reply: (body: unknown) => RouteReply, authorized = false) {
  const bodies: unknown[] = [];
  const fetchMock = installFetch((method, url, body) => {
    if (method === "POST" && url === "/api/devices/pair") {
      bodies.push(body);
      return reply(body);
    }
    if (authorized && method === "GET" && url === "/api/devices") {
      return { status: 200, body: { devices: [], current: "0011223344556677" } };
    }
    return { status: 401, body: { error: "x", code: "device_required" } };
  });
  render(<ConnectPage />);
  return { bodies, fetchMock };
}

function input(): HTMLInputElement {
  return screen.getByLabelText("Código de 6 dígitos") as HTMLInputElement;
}

async function type(code: string) {
  fireEvent.change(input(), { target: { value: code } });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Entrar" }));
  });
  await flush();
}

describe("página de entrada (112/T02)", () => {
  it("logo, slogan logo depois dela e o campo numérico", async () => {
    const { fetchMock } = setup(() => ({ status: 200 }));
    const main = screen.getByRole("main");
    const brand = main.querySelector("[data-brand]");
    expect(brand?.textContent).toBe(BRAND_NAME);
    expect(brand?.querySelector("svg[data-brand-symbol]")).not.toBeNull();
    // O slogan é o elemento logo depois da logo.
    expect(brand?.nextElementSibling?.textContent).toBe(BRAND_SLOGAN);
    expect(screen.getByRole("heading", { name: "Entrar neste aparelho" })).toBeTruthy();
    expect(input().getAttribute("inputmode")).toBe("numeric");
    expect(input().getAttribute("autocomplete")).toBe("one-time-code");
    expect(main.querySelector("[data-connect-link-invalid]")).toBeNull();
    expect(findForbiddenGlyphs(main)).toEqual([]);
    // Só a consulta de abertura (já autorizado?); sem dispositivo, a página fica.
    await flush();
    expect(fetchMock.mock.calls.map((c) => String(c[0]))).toEqual(["/api/devices"]);
    expect(connectEnvironment.enter).not.toHaveBeenCalled();
  });

  it("só dígitos (colar com espaço funciona) e acerto vai para /", async () => {
    const { bodies } = setup(() => ({ status: 200, body: { device: { id: "0011223344556677", name: "Safari no iPhone", created_at: 1, last_seen: 1, this_computer: false, current: true } } }));
    fireEvent.change(input(), { target: { value: "12a 34-56 7" } });
    expect(input().value).toBe("123456");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Entrar" }));
    });
    await flush();
    expect(bodies).toEqual([{ code: "123456" }]);
    expect(connectEnvironment.enter).toHaveBeenCalledTimes(1);
    expect(connectEnvironment.enter).toHaveBeenCalledWith("/");
  });

  it("acerto volta para o next validado (spec 128)", async () => {
    connectEnvironment.search = () => `?next=${encodeURIComponent("/projects/abc?tab=chat#fim")}`;
    setup(() => ({ status: 200, body: { device: { id: "0011223344556677", name: "Safari no iPhone", created_at: 1, last_seen: 1, this_computer: false, current: true } } }));
    await type("123456");
    expect(connectEnvironment.enter).toHaveBeenCalledWith("/projects/abc?tab=chat#fim");
  });

  it("já autorizado: vai para o next sem pedir código", async () => {
    connectEnvironment.search = () => "?next=%2Fsettings";
    setup(() => ({ status: 200 }), true);
    await flush();
    expect(connectEnvironment.enter).toHaveBeenCalledWith("/settings");
  });

  it("next fora da regra vira a raiz", () => {
    for (const bad of ["//evil.com", "http://evil.com", "%5Cevil", "", "%2Fconnect", "javascript:alert(1)", "%2F%09%2Fevil"]) {
      expect(entryTarget(`?next=${bad}`)).toBe("/");
    }
    expect(entryTarget("")).toBe("/");
    expect(entryTarget("?next=%2Fx%3Fa%3D1")).toBe("/x?a=1");
  });

  it("menos de 6 dígitos: pede os 6, sem chamar a API", async () => {
    const { bodies } = setup(() => ({ status: 200 }));
    await type("123");
    expect(screen.getByRole("alert").textContent).toBe("Digite os 6 dígitos.");
    expect(bodies).toEqual([]);
  });

  it("código errado: as tentativas que restam (plural e singular), e o campo limpa", async () => {
    let left = 3;
    setup(() => ({ status: 400, body: { error: "código errado", code: "device_code_invalid", params: { attempts_left: left } } }));
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Código errado. Restam 3 tentativas.");
    expect(input().value).toBe("");
    expect(input().getAttribute("aria-invalid")).toBe("true");
    left = 1;
    await type("222222");
    expect(screen.getByRole("alert").textContent).toBe("Código errado. Resta 1 tentativa.");
    expect(connectEnvironment.enter).not.toHaveBeenCalled();
  });

  it("última tentativa errada, janela invalidada, sem código ativo e 429 por segundo", async () => {
    const replies: RouteReply[] = [
      { status: 400, body: { error: "x", code: "device_code_invalid", params: { attempts_left: 0 } } },
      { status: 429, body: { error: "x", code: "device_code_locked" } },
      { status: 400, body: { error: "x", code: "device_code_inactive" } },
      { status: 429, body: { error: "x", code: "device_pair_rate_limited", params: { retry_after_ms: 400 } } },
      { status: 500, body: { error: "x" } },
    ];
    setup(() => replies.shift() ?? { status: 500 });
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Várias tentativas erradas. No computador, gere outro código.");
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Várias tentativas erradas. No computador, gere outro código.");
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Nenhum código ativo. No computador, abra Adicionar dispositivo.");
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Aguarde um segundo e tente de novo.");
    await type("111111");
    expect(screen.getByRole("alert").textContent).toBe("Não foi possível entrar: falha interna do servidor");
  });

  it("link de entrada expirado: a explicação, e como obter um link novo", () => {
    connectEnvironment.search = () => "?reason=link_invalid";
    setup(() => ({ status: 200 }));
    expect(screen.getByRole("status").textContent).toBe("Este link de entrada expirou ou já foi usado.");
    expect(screen.getByRole("main").querySelector("[data-connect-this-computer]")?.textContent).toContain("reinicie o Nants");
    expect(entryReason("?reason=outro")).toBeNull();
  });

  it("em inglês", async () => {
    setLanguage("en", { persist: false, storage: null });
    setup(() => ({ status: 400, body: { error: "x", code: "device_code_invalid", params: { attempts_left: 2 } } }));
    expect(screen.getByRole("heading", { name: "Sign in on this device" })).toBeTruthy();
    fireEvent.change(screen.getByLabelText("6 digit code"), { target: { value: "999999" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    });
    await flush();
    expect(screen.getByRole("alert").textContent).toBe("Wrong code. 2 attempts left.");
  });
});
