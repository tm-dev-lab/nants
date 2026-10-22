/* Acesso pela rede (spec 097, T02): validação da visão de rede, da seção `network` e da marca. */
import { describe, expect, it } from "vitest";
import { networkStateValue, parseLanRange, parseNetworkState, parseNetworkView, rangeLabel, rangeReleased } from "./network";

describe("visão de rede da API", () => {
  it("lê porta do Nants, faixa e WSL; faixa inválida ou ausente vira null", () => {
    expect(parseNetworkView({ nants_port: 3000, lan_view_ports: { start: 3001, end: 3010 }, wsl: true })).toEqual({
      nants_port: 3000,
      lan_view_ports: { start: 3001, end: 3010 },
      wsl: true,
    });
    expect(parseNetworkView({ nants_port: 3000, lan_view_ports: null, wsl: false })).toEqual({ nants_port: 3000, lan_view_ports: null, wsl: false });
    expect(parseNetworkView({ nants_port: 3000, lan_view_ports: { start: 3010, end: 3001 } })?.lan_view_ports).toBeNull();
    expect(parseNetworkView({ nants_port: 3000, wsl: "sim" })?.wsl).toBe(false);
    expect(parseNetworkView({ lan_view_ports: { start: 3001, end: 3010 } })).toBeNull();
    expect(parseNetworkView(undefined)).toBeNull();
    expect(parseLanRange({ start: 0, end: 10 })).toBeNull();
    expect(parseLanRange({ start: 3001.5, end: 3010 })).toBeNull();
  });
});

describe("seção network do estado da interface", () => {
  it("grava e lê a faixa liberada; lixo vira nenhuma", () => {
    const value = networkStateValue({ start: 3001, end: 3010 });
    expect(value).toEqual({ released: { start: 3001, end: 3010 } });
    expect(parseNetworkState(value).released).toEqual({ start: 3001, end: 3010 });
    expect(networkStateValue(null)).toEqual({ released: null });
    expect(parseNetworkState(undefined).released).toBeNull();
    expect(parseNetworkState({ released: "3001-3010" }).released).toBeNull();
  });

  it("a marca só vale para a mesma faixa em uso", () => {
    const range = { start: 3001, end: 3010 };
    expect(rangeReleased(range, { start: 3001, end: 3010 })).toBe(true);
    expect(rangeReleased(range, { start: 3101, end: 3110 })).toBe(false);
    expect(rangeReleased(range, { start: 3001, end: 3005 })).toBe(false);
    expect(rangeReleased(null, range)).toBe(false);
    expect(rangeReleased(range, null)).toBe(false);
  });

  it("rótulo da faixa", () => {
    expect(rangeLabel({ start: 3001, end: 3010 })).toBe("3001 a 3010");
    expect(rangeLabel({ start: 3001, end: 3001 })).toBe("3001");
  });
});
