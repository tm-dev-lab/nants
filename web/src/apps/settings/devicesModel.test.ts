/*
 * Jornada guiada (spec 112, decisão 8) e auxiliares do código e do QR: funções puras.
 */
import { describe, expect, it } from "vitest";
import { encode } from "uqr";
import type { NetworkConfig } from "../../api/types";
import { canAddDevice, codeDigits, groupedCode, journeySteps, pairErrorKind, qrPath } from "./devicesModel";

function net(over: Partial<NetworkConfig> = {}): NetworkConfig {
  return {
    env: "wsl_nat",
    saved_lan: true,
    lan: true,
    mode_source: "saved",
    port: 3000,
    lan_port: 3000,
    view_ports: { start: 3001, end: 3010 },
    windows_redirect: "yes",
    windows_redirect_target: "this_wsl",
    firewall_rule: "yes",
    restart_required: false,
    restart_command: "nants",
    coherence: "ok",
    ...over,
  };
}

const summary = (n: NetworkConfig) => journeySteps(n).map((s) => `${s.id}:${s.status}:${s.detail}`);

describe("passos por ambiente", () => {
  it("WSL NAT pronto: três passos, adicionar habilitado", () => {
    expect(summary(net())).toEqual([
      "enable:done:devices.step.enable.done",
      "windows:done:devices.step.windows.done",
      "add:ready:devices.step.add.ready",
    ]);
    expect(journeySteps(net())[1]?.title).toBe("devices.step.windows.title");
  });

  it("WSL NAT: script pendente bloqueia o adicionar com o motivo do passo 2", () => {
    const steps = journeySteps(net({ windows_redirect_target: "none", windows_redirect: "no" }));
    expect(steps.map((s) => s.status)).toEqual(["done", "pending", "blocked"]);
    expect(steps[2]?.detail).toBe("devices.reason.scriptFirst");
    expect(canAddDevice(steps[2] as (typeof steps)[number])).toBe(false);
  });

  it("espelhado: firewall; regra faltando bloqueia com o motivo do firewall", () => {
    const steps = journeySteps(net({ env: "wsl_mirrored", windows_redirect_target: "none", firewall_rule: "no" }));
    expect(steps[1]?.title).toBe("devices.step.firewall.title");
    expect(steps.map((s) => s.status)).toEqual(["done", "pending", "blocked"]);
    expect(steps[2]?.detail).toBe("devices.reason.firewallFirst");
    expect(journeySteps(net({ env: "wsl_mirrored", windows_redirect_target: "none", firewall_rule: "yes" }))[1]?.status).toBe("done");
  });

  it("Linux: só ligar e adicionar", () => {
    expect(summary(net({ env: "linux", firewall_rule: "unknown" }))).toEqual(["enable:done:devices.step.enable.done", "add:ready:devices.step.add.ready"]);
    expect(summary(net({ env: "linux", lan: false, saved_lan: false }))).toEqual(["enable:pending:devices.step.enable.off", "add:blocked:devices.reason.enableFirst"]);
  });

  it("salva ligada e sem reiniciar: o passo 2 libera; o adicionar pede o reinício (ou o script antes)", () => {
    expect(summary(net({ lan: false }))).toEqual([
      "enable:done:devices.step.enable.restart",
      "windows:done:devices.step.windows.done",
      "add:blocked:devices.reason.restartFirst",
    ]);
    expect(summary(net({ lan: false, windows_redirect: "no", windows_redirect_target: "none" }))).toEqual([
      "enable:done:devices.step.enable.restart",
      "windows:pending:devices.step.windows.pending",
      "add:blocked:devices.reason.scriptFirst",
    ]);
    expect(summary(net({ lan: false, firewall_rule: "unknown", windows_redirect_target: "unknown" })).at(2)).toBe("add:blocked:devices.reason.restartFirst");
    expect(summary(net({ env: "linux", lan: false, firewall_rule: "unknown" }))).toEqual(["enable:done:devices.step.enable.restart", "add:blocked:devices.reason.restartFirst"]);
  });

  it("desligada nesta execução e na opção salva: tudo espera o passo 1; sem opção salva, vale o --lan", () => {
    expect(summary(net({ lan: false, saved_lan: false })).slice(1)).toEqual(["windows:blocked:devices.reason.enableFirst", "add:blocked:devices.reason.enableFirst"]);
    expect(summary(net({ saved_lan: null, lan: true }))[0]).toBe("enable:done:devices.step.enable.done");
    expect(summary(net({ saved_lan: null, lan: false }))[0]).toBe("enable:pending:devices.step.enable.off");
  });

  it("execução na rede com a opção salva desligada (--lan ou --host): pronto, e dá para adicionar", () => {
    for (const mode_source of ["flag", "host"] as const) {
      expect(summary(net({ saved_lan: false, lan: true, mode_source }))).toEqual([
        "enable:done:devices.step.enable.done",
        "windows:done:devices.step.windows.done",
        "add:ready:devices.step.add.ready",
      ]);
    }
    expect(summary(net({ env: "linux", saved_lan: false, lan: true, mode_source: "flag", firewall_rule: "unknown" }))).toEqual([
      "enable:done:devices.step.enable.done",
      "add:ready:devices.step.add.ready",
    ]);
  });

  it("Windows não conferido: o adicionar fica habilitado com a ressalva", () => {
    const steps = journeySteps(net({ firewall_rule: "unknown", windows_redirect_target: "unknown" }));
    expect(steps.map((s) => s.status)).toEqual(["done", "unknown", "unknown"]);
    expect(steps[2]?.detail).toBe("devices.step.add.unknown");
    expect(canAddDevice(steps[2] as (typeof steps)[number])).toBe(true);
  });

  it("WSL sem o modo conferido segue o NAT", () => {
    expect(journeySteps(net({ env: "wsl_unknown" })).map((s) => s.status)).toEqual(["done", "done", "ready"]);
  });
});

describe("código e erros da troca", () => {
  it("só dígitos, no máximo 6; dois grupos de 3", () => {
    expect(codeDigits(" 12-34 56 78")).toBe("123456");
    expect(codeDigits("abc")).toBe("");
    expect(groupedCode("123456")).toBe("123 456");
    expect(groupedCode("12")).toBe("12");
  });

  it("classifica pelo code estável", () => {
    expect(pairErrorKind({ code: "device_code_invalid", params: { attempts_left: 3 } })).toEqual({ kind: "invalid", attemptsLeft: 3 });
    expect(pairErrorKind({ code: "device_code_invalid", params: { attempts_left: 0 } })).toEqual({ kind: "locked" });
    expect(pairErrorKind({ code: "device_code_invalid" })).toEqual({ kind: "locked" });
    expect(pairErrorKind({ code: "device_code_locked" })).toEqual({ kind: "locked" });
    expect(pairErrorKind({ code: "device_code_inactive" })).toEqual({ kind: "inactive" });
    expect(pairErrorKind({ code: "device_pair_rate_limited" })).toEqual({ kind: "rate_limited" });
    expect(pairErrorKind({})).toEqual({ kind: "other" });
  });
});

describe("QR code", () => {
  it("um retângulo por sequência escura de cada linha", () => {
    expect(qrPath([[true, true, false, true], [false, false, false, false], [false, true, true, true]])).toBe("M0 0h2v1h-2zM3 0h1v1h-1zM1 2h3v1h-3z");
    expect(qrPath([[false]])).toBe("");
  });

  it("o endereço vira uma matriz QR com a zona de silêncio e os três padrões de posição", () => {
    const qr = encode("http://192.168.0.68:3000/connect", { ecc: "M", border: 4 });
    // Versão 3 (29 módulos) + 2 × 4 de borda.
    expect(qr.size).toBe(qr.data.length);
    expect(qr.size).toBeGreaterThanOrEqual(21 + 8);
    // Borda clara.
    expect(qr.data[0]?.every((m) => !m)).toBe(true);
    expect(qr.data[3]?.every((m) => !m)).toBe(true);
    // Canto do padrão de posição superior esquerdo: 7 módulos escuros na primeira linha dele.
    expect(qr.data[4]?.slice(4, 11).every((m) => m)).toBe(true);
    expect(qr.data[4]?.slice(qr.size - 11, qr.size - 4).every((m) => m)).toBe(true);
    expect(qr.data[qr.size - 5]?.slice(4, 11).every((m) => m)).toBe(true);
    expect(qrPath(qr.data).length).toBeGreaterThan(100);
  });
});
