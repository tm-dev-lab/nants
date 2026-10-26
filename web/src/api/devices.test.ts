/*
 * Cliente dos dispositivos (spec 112, T02): validação das respostas, rotas e corpos, os eventos
 * `devices_changed`/`device_code_locked`, o 401 `device_required` e os codes do servidor.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { en } from "../i18n/en";
import { looksPortuguese } from "../i18n/ptLeftovers";
import { ptBR } from "../i18n/ptBR";
import { MAX_NEXT_BYTES, deviceAuth, entryLocation, getConfig, safeNext } from "./client";
import { isDeviceEvent, publishDeviceEvent, subscribeDeviceEvents } from "./deviceEvents";
import {
  closeDeviceCode,
  listDevices,
  openDeviceCode,
  pairDevice,
  parseCodeWindow,
  parseDeviceList,
  renameDevice,
  revokeAllDevices,
  revokeDevice,
} from "./devices";
import { KNOWN_EVENT_TYPES, parseApiEvent } from "./events";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function stub(status: number, body: unknown) {
  const calls: { url: string; method: string; body: unknown }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, method: init?.method ?? "GET", body: typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined });
      return new Response(body === undefined ? null : JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
    }),
  );
  return calls;
}

describe("respostas validadas", () => {
  it("janela aberta, fechada e invalidada", () => {
    expect(parseCodeWindow({ state: "open", code: "012345", seconds_to_next: 7, expires_in: 300, entry_path: "/connect", entry_urls: ["http://192.168.0.68:3000/connect", "javascript:alert(1)", 3], max_errors: 5 })).toEqual({
      state: "open",
      code: "012345",
      secondsToNext: 7,
      expiresIn: 300,
      entryUrls: ["http://192.168.0.68:3000/connect"],
      entryPath: "/connect",
      maxErrors: 5,
    });
    expect(parseCodeWindow({ state: "closed" })?.code).toBeNull();
    expect(parseCodeWindow({ state: "locked", entry_urls: [] })?.state).toBe("locked");
    expect(parseCodeWindow({ state: "open", code: "12345", seconds_to_next: 1, expires_in: 1 })).toBeNull();
    expect(parseCodeWindow({ state: "open", code: "123456", seconds_to_next: -1, expires_in: 1 })).toBeNull();
    expect(parseCodeWindow({ state: "talvez" })).toBeNull();
  });

  it("lista: itens fora do contrato saem", () => {
    const list = parseDeviceList({
      devices: [
        { id: "0011223344556677", name: "Safari no iPhone", created_at: 1, last_seen: 2, this_computer: false, current: true },
        { id: "", name: "x", created_at: 1, last_seen: 2 },
        { id: "a", name: 3, created_at: 1, last_seen: 2 },
      ],
      current: "0011223344556677",
    });
    expect(list).toEqual([{ id: "0011223344556677", name: "Safari no iPhone", createdAt: 1, lastSeen: 2, thisComputer: false, current: true }]);
    expect(parseDeviceList({})).toBeNull();
  });
});

describe("rotas e corpos", () => {
  it("abrir, fechar, trocar o código, listar, renomear e revogar", async () => {
    let calls = stub(200, { state: "open", code: "123456", seconds_to_next: 60, expires_in: 600, entry_urls: [] });
    expect((await openDeviceCode()).ok).toBe(true);
    expect(calls[0]).toMatchObject({ url: "/api/devices/code", method: "POST" });
    calls = stub(204, undefined);
    expect((await closeDeviceCode()).ok).toBe(true);
    expect(calls[0]).toMatchObject({ url: "/api/devices/code", method: "DELETE" });
    calls = stub(200, { device: { id: "0011223344556677", name: "Chrome no Android", created_at: 1, last_seen: 1, current: true } });
    const paired = await pairDevice("654321");
    expect(paired.ok && paired.data.name).toBe("Chrome no Android");
    expect(calls[0]).toMatchObject({ url: "/api/devices/pair", method: "POST", body: { code: "654321" } });
    calls = stub(200, { devices: [], current: null });
    expect(await listDevices()).toEqual({ ok: true, data: [] });
    calls = stub(200, { device: { id: "0011223344556677", name: "Celular", created_at: 1, last_seen: 1 } });
    expect((await renameDevice("0011223344556677", "Celular")).ok).toBe(true);
    expect(calls[0]).toMatchObject({ url: "/api/devices/0011223344556677", method: "PATCH", body: { name: "Celular" } });
    calls = stub(204, undefined);
    expect((await revokeDevice("0011223344556677")).ok).toBe(true);
    expect(calls[0]).toMatchObject({ url: "/api/devices/0011223344556677", method: "DELETE" });
    calls = stub(200, { revoked: 3 });
    expect(await revokeAllDevices()).toEqual({ ok: true, data: 3 });
    expect(calls[0]).toMatchObject({ url: "/api/devices", method: "DELETE" });
  });

  it("resposta fora do contrato vira invalid_response", async () => {
    stub(200, { devices: "x" });
    const r = await listDevices();
    expect(!r.ok && r.error.kind).toBe("invalid_response");
  });

  it("erro com code e params chega para a tela", async () => {
    stub(400, { error: "código errado", code: "device_code_invalid", params: { attempts_left: 2 } });
    const r = await pairDevice("000000");
    expect(!r.ok && r.error.code).toBe("device_code_invalid");
    expect(!r.ok && r.error.params).toEqual({ attempts_left: 2 });
  });
});

describe("aparelho sem autorização", () => {
  it("401 device_required leva à página de entrada; outro 401 não", async () => {
    const spy = vi.spyOn(deviceAuth, "onDeviceRequired").mockImplementation(() => undefined);
    stub(401, { error: "x", code: "device_required" });
    await getConfig();
    expect(spy).toHaveBeenCalledTimes(1);
    stub(401, { error: "x" });
    await getConfig();
    expect(spy).toHaveBeenCalledTimes(1);
  });

  function fakeLocation(pathname: string, search: string, hash: string) {
    vi.spyOn(deviceAuth, "here").mockReturnValue({ pathname, search, hash });
    return vi.spyOn(deviceAuth, "go").mockImplementation(() => undefined);
  }

  it("leva a página atual como next, percent-encodada (spec 128)", () => {
    const assign = fakeLocation("/projects/abc", "?tab=chat", "#fim");
    deviceAuth.onDeviceRequired();
    expect(assign).toHaveBeenCalledWith(`/connect?next=${encodeURIComponent("/projects/abc?tab=chat#fim")}`);
  });

  it("na raiz, em /connect ou com caminho fora da regra, sem next", () => {
    let assign = fakeLocation("/", "", "");
    deviceAuth.onDeviceRequired();
    expect(assign).toHaveBeenCalledWith("/connect");
    vi.restoreAllMocks();
    assign = fakeLocation("/connect", "?next=%2Fx", "");
    deviceAuth.onDeviceRequired();
    expect(assign).not.toHaveBeenCalled();
    vi.restoreAllMocks();
    assign = fakeLocation(`/${"a".repeat(MAX_NEXT_BYTES)}`, "", "");
    deviceAuth.onDeviceRequired();
    expect(assign).toHaveBeenCalledWith("/connect");
  });

  it("safeNext: a mesma regra do servidor", () => {
    for (const ok of ["/", "/projects/abc", "/x?a=1#sec", "/connectx", "/%2F%2Fevil"]) expect(safeNext(ok)).toBe(ok);
    for (const bad of ["", "//evil", "http://evil.com", "javascript:alert(1)", "x/y", "\\evil", "/\\evil", "/\t/evil", "/a b", "/connect", "/connect?next=/x", "/connect#x"]) {
      expect(safeNext(bad)).toBeNull();
    }
    expect(safeNext(null)).toBeNull();
    expect(safeNext(`/${"a".repeat(MAX_NEXT_BYTES - 1)}`)).not.toBeNull();
    expect(safeNext(`/${"a".repeat(MAX_NEXT_BYTES)}`)).toBeNull();
    expect(entryLocation("/p?a=1&b=2#h")).toBe("/connect?next=%2Fp%3Fa%3D1%26b%3D2%23h");
    expect(entryLocation("//evil")).toBe("/connect");
  });
});

describe("eventos dos dispositivos", () => {
  it("conhecidos, validados e repassados aos assinantes", () => {
    expect(KNOWN_EVENT_TYPES).toContain("devices_changed");
    expect(KNOWN_EVENT_TYPES).toContain("device_code_locked");
    // Payloads como o servidor publica (devices.rs).
    const changed = parseApiEvent({ event_type: "devices_changed", project_id: null, payload: { reason: "added" } });
    const locked = parseApiEvent({ event_type: "device_code_locked", project_id: null, payload: { reason: "too_many_errors" } });
    expect(changed).not.toBeNull();
    expect(locked).not.toBeNull();
    expect(parseApiEvent({ event_type: "devices_changed", project_id: null, payload: { reason: 3 } })).toBeNull();
    const seen: string[] = [];
    const stop = subscribeDeviceEvents((e) => seen.push(`${e.event_type}:${e.payload.reason}`));
    for (const ev of [changed, locked]) if (ev !== null && isDeviceEvent(ev)) publishDeviceEvent(ev);
    stop();
    if (changed !== null && isDeviceEvent(changed)) publishDeviceEvent(changed);
    expect(seen).toEqual(["devices_changed:added", "device_code_locked:too_many_errors"]);
  });

  it("os eventos e os codes do servidor (lidos do devices.rs) estão no cliente e nos dois idiomas", () => {
    const root = resolve(process.cwd(), "..");
    const rust = readFileSync(resolve(root, "src/server/devices.rs"), "utf8");
    const codes = [...rust.matchAll(/pub const CODE_[A-Z_]+: &str = "([a-z_]+)";/g)].map((m) => m[1] ?? "");
    expect(codes.length).toBeGreaterThanOrEqual(8);
    for (const code of codes) {
      const key = `serverError.${code}`;
      expect(Object.keys(ptBR), key).toContain(key);
      expect(looksPortuguese(en[key as keyof typeof en]), key).toBe(false);
    }
    const events = readFileSync(resolve(root, "src/server/events.rs"), "utf8");
    for (const name of ["devices_changed", "device_code_locked"]) expect(events).toContain(`"${name}"`);
  });
});
