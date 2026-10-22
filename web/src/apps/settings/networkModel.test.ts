/* Acesso pela rede (spec 097, T02): script do PowerShell para o WSL e textos com os valores reais. */
import { describe, expect, it } from "vitest";
import { forbiddenGlyphIn } from "../../desktop/glyphs";
import type { NetworkConfig } from "../../api/types";
import {
  NETWORK_TEXT,
  coherenceIsWarning,
  coherenceText,
  mirroredEnableScript,
  mirroredUndoScript,
  nantsLanAddress,
  networkScripts,
  parseNetworkConfig,
  restartNeeded,
  scriptForCoherence,
  wslPowerShellScript,
  windowsReleased,
  wslUndoScript,
} from "./networkModel";

const EXPECTED_3000 = [
  "$ip = (wsl hostname -I).Trim().Split(' ')[0]",
  "foreach ($p in @(3000) + (3001..3010)) {",
  "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
  "  netsh interface portproxy add v4tov4 listenport=$p listenaddress=0.0.0.0 connectport=$p connectaddress=$ip",
  "}",
  'Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue',
  'New-NetFirewallRule -DisplayName "Nants" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 3000,3001-3010 -RemoteAddress LocalSubnet',
].join("\n");

describe("script do PowerShell (WSL)", () => {
  it("usa a porta do Nants e a faixa reais", () => {
    expect(wslPowerShellScript(3000, { start: 3001, end: 3010 })).toBe(EXPECTED_3000);
    const other = wslPowerShellScript(8080, { start: 9000, end: 9019 });
    expect(other).toContain("foreach ($p in @(8080) + (9000..9019)) {");
    expect(other).toContain("-LocalPort 8080,9000-9019 -RemoteAddress LocalSubnet");
    expect(other).not.toContain("3000");
    expect(wslPowerShellScript(3000, { start: 3001, end: 3001 }).endsWith("-LocalPort 3000,3001 -RemoteAddress LocalSubnet")).toBe(
      true,
    );
  });

  it("idempotente: remove antes de criar, no portproxy e no firewall; IP lido na hora", () => {
    const lines = wslPowerShellScript(3000, { start: 3001, end: 3010 }).split("\n");
    const del = lines.findIndex((l) => l.includes("portproxy delete"));
    const add = lines.findIndex((l) => l.includes("portproxy add"));
    expect(del).toBeGreaterThan(-1);
    expect(del).toBeLessThan(add);
    const remove = lines.findIndex((l) => l.startsWith("Remove-NetFirewallRule"));
    const create = lines.findIndex((l) => l.startsWith("New-NetFirewallRule"));
    expect(remove).toBeGreaterThan(-1);
    expect(remove).toBeLessThan(create);
    expect(lines[remove]).toBe('Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue');
    // Qualquer perfil (sem -Profile, spec 112), ainda só a sub-rede local, no fim da linha.
    expect(lines[create]).toMatch(/ -RemoteAddress LocalSubnet$/);
    expect(lines[create]).not.toContain("-Profile");
    expect(lines.filter((l) => l.includes("NetFirewallRule"))).toHaveLength(2);
    expect(lines[0]).toContain("wsl hostname -I");
    // Nenhum IP fixo: o destino é sempre o lido na hora.
    expect(lines.join("\n").replaceAll("0.0.0.0", "")).not.toMatch(/\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}/);
    expect(lines.filter((l) => l.includes("connectaddress=$ip"))).toHaveLength(1);
  });

  it("o redirecionamento aponta para o IP do WSL, nunca para o loopback (spec 112)", () => {
    // Apontando para 127.0.0.1, os aparelhos chegariam ao Nants como loopback e entrariam sem dispositivo.
    for (const script of [wslPowerShellScript(3000, { start: 3001, end: 3010 }), wslPowerShellScript(8080, null)]) {
      const lines = script.split("\n");
      expect(lines[0]).toBe("$ip = (wsl hostname -I).Trim().Split(' ')[0]");
      const targets = lines.filter((l) => l.includes("connectaddress=")).map((l) => l.match(/connectaddress=(\S+)/)?.[1]);
      expect(targets).toEqual(["$ip"]);
      expect(script).not.toMatch(/127\.0\.0\.1|localhost|::1/);
    }
  });

  it("qualquer perfil de rede: nenhum script de ligar restringe o perfil (spec 112)", () => {
    for (const script of [wslPowerShellScript(3000, { start: 3001, end: 3010 }), mirroredEnableScript(3000, { start: 3001, end: 3010 }), mirroredEnableScript(8080, null)]) {
      expect(script).not.toContain("-Profile");
      expect(script).toMatch(/New-NetFirewallRule .* -RemoteAddress LocalSubnet$/m);
    }
  });
});

describe("textos da seção", () => {
  it("endereço do Nants com o host da página; no loopback pede o IP do computador", () => {
    expect(nantsLanAddress("192.168.0.68", 3000)).toBe("http://192.168.0.68:3000");
    expect(nantsLanAddress("localhost", 3000)).toBe("http://<IP deste computador>:3000");
  });

  it("sem travessão nem emoji", () => {
    for (const text of [NETWORK_TEXT.hint, NETWORK_TEXT.unverified, NETWORK_TEXT.wslIntro, NETWORK_TEXT.security, NETWORK_TEXT.lanHint, NETWORK_TEXT.wslRestart, NETWORK_TEXT.wslMirrored]) {
      expect(forbiddenGlyphIn(text)).toBeNull();
    }
    // O script é código (tem `+` do PowerShell), mas também sem travessão nem emoji.
    expect(EXPECTED_3000).not.toMatch(/[\u2013\u2014]|\p{Extended_Pictographic}/u);
    expect(NETWORK_TEXT.wslIntro).toContain("PowerShell como administrador");
    expect(NETWORK_TEXT.wslIntro).toContain("só para aparelhos da rede local");
    // Spec 112: a regra vale para qualquer perfil; nada manda marcar a rede como privada.
    expect(NETWORK_TEXT.wslIntro).toContain("qualquer tipo de rede");
    expect(NETWORK_TEXT.wslIntro).not.toContain("marcada como privada");
    expect(NETWORK_TEXT.security).toContain("Só aparelhos que você adicionar abrem o Nants");
    expect(NETWORK_TEXT.security).toContain("HTTP, sem criptografia");
    expect(NETWORK_TEXT.hint).toContain("não o Nants, e não pedem dispositivo");
    expect(NETWORK_TEXT.wslRestart).toContain("reiniciar");
    expect(NETWORK_TEXT.wslMirrored).toContain("networkingMode=mirrored");
  });
});

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

const ADD = /\badd\b/i;

describe("scripts por ambiente (111/T03)", () => {
  it("WSL NAT, desfazer: remove o portproxy da porta e da faixa e a regra Nants, mostra a tabela; nunca add", () => {
    const undo = wslUndoScript(3000, { start: 3001, end: 3010 });
    expect(undo).toBe(
      [
        "foreach ($p in @(3000) + (3001..3010)) {",
        "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
        "}",
        'Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue',
        "netsh interface portproxy show v4tov4",
      ].join("\n"),
    );
    expect(undo).not.toMatch(ADD);
    expect(undo).not.toContain("New-NetFirewallRule");
    expect(undo.split("\n").at(-1)).toBe("netsh interface portproxy show v4tov4");
    expect(wslUndoScript(8080, null)).toContain("foreach ($p in @(8080)) {");
  });

  it("WSL espelhado: só a regra de firewall, para ligar e para desfazer", () => {
    const enable = mirroredEnableScript(3000, { start: 3001, end: 3010 });
    expect(enable).toBe(
      [
        'Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue',
        'New-NetFirewallRule -DisplayName "Nants" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 3000,3001-3010 -RemoteAddress LocalSubnet',
      ].join("\n"),
    );
    expect(mirroredUndoScript()).toBe('Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue');
    expect(mirroredUndoScript()).not.toMatch(ADD);
    expect(enable + mirroredUndoScript()).not.toMatch(/portproxy|netsh/);
  });

  it("por ambiente: Linux sem script; NAT e desconhecido com portproxy; espelhado só firewall; só o nome Nants", () => {
    expect(networkScripts(net({ env: "linux" }))).toBeNull();
    for (const env of ["wsl_nat", "wsl_unknown"] as const) {
      const s = networkScripts(net({ env }));
      expect(s?.enable).toBe(wslPowerShellScript(3000, { start: 3001, end: 3010 }));
      expect(s?.undo).toBe(wslUndoScript(3000, { start: 3001, end: 3010 }));
    }
    const m = networkScripts(net({ env: "wsl_mirrored" }));
    expect(m?.enable).toBe(mirroredEnableScript(3000, { start: 3001, end: 3010 }));
    expect(m?.undo).toBe(mirroredUndoScript());
    for (const env of ["wsl_nat", "wsl_unknown", "wsl_mirrored"] as const) {
      const s = networkScripts(net({ env, lan_port: 8080, port: 8081 }));
      expect(s?.undo).not.toMatch(ADD);
      // Nome legado da regra montado em partes, para o pr\u00f3prio teste n\u00e3o cit\u00e1-lo.
      expect(`${s?.enable}${s?.undo}`).not.toContain(["Ne", "mo"].join(""));
      expect(`${s?.enable}${s?.undo}`).not.toMatch(/[\u2013\u2014]|\p{Extended_Pictographic}/u);
      expect(s?.enable).toContain("8080");
      expect(s?.enable).not.toContain("8081");
    }
  });
});

describe("estado da API (111/T03)", () => {
  it("valida o essencial e trata valores estranhos do Windows como não conferido", () => {
    expect(parseNetworkConfig(null)).toBeNull();
    expect(parseNetworkConfig({ env: "mac", lan: true, port: 3000 })).toBeNull();
    const parsed = parseNetworkConfig({ env: "wsl_nat", lan: false, port: 3001, lan_port: 3000, saved_lan: "x", windows_redirect: "talvez", view_ports: { start: 3001, end: 3010 } });
    expect(parsed).toMatchObject({ env: "wsl_nat", saved_lan: null, lan: false, port: 3001, lan_port: 3000, windows_redirect: "unknown", windows_redirect_target: "unknown", firewall_rule: "unknown", mode_source: "default", restart_command: "nants", view_ports: { start: 3001, end: 3010 } });
  });

  it("reinício: só quando a opção salva difere do modo atual e houve mudança aqui ou o modo não veio do --lan", () => {
    expect(restartNeeded(net({ saved_lan: null, lan: true, mode_source: "flag" }), false)).toBe(false);
    expect(restartNeeded(net({ saved_lan: null, lan: true, mode_source: "flag" }), true)).toBe(true);
    expect(restartNeeded(net({ saved_lan: false, lan: true, mode_source: "host" }), false)).toBe(true);
    expect(restartNeeded(net({ saved_lan: true, lan: true }), true)).toBe(false);
    expect(restartNeeded(net({ saved_lan: null, lan: false, mode_source: "default" }), false)).toBe(false);
  });

  it("coerência: o código do servidor vira texto, tom e o script que abre", () => {
    const cases: [NetworkConfig["coherence"], string | null, boolean, "enable" | "undo" | null][] = [
      ["ok", null, false, null],
      ["unknown", null, false, null],
      ["will_adopt", "O Windows já está configurado para este WSL: o próximo início liga o acesso pela rede local.", false, null],
      ["needs_script", "Falta rodar o script de ligar no Windows.", true, "enable"],
      ["missing_rule", "Falta a regra de firewall Nants no Windows: rode o script de ligar.", true, "enable"],
      ["other_target", NETWORK_TEXT.otherTarget, true, "enable"],
      [
        "still_redirected",
        "O Windows ainda redireciona as portas: rode o script de desfazer. Por isso o Nants pode estar numa porta diferente da 3000.",
        true,
        "undo",
      ],
    ];
    for (const [code, text, warning, script] of cases) {
      expect(coherenceText(code, { lan_port: 3000 }), code).toBe(text);
      expect(coherenceIsWarning(code), code).toBe(warning);
      expect(scriptForCoherence(code), code).toBe(script);
    }
    expect(parseNetworkConfig({ env: "wsl_nat", lan: true, port: 3000, coherence: "will_adopt" })?.coherence).toBe("will_adopt");
    expect(parseNetworkConfig({ env: "wsl_nat", lan: true, port: 3000, coherence: "estranho" })?.coherence).toBe("unknown");
  });

  it("estado real no Windows: só no WSL e quando deu para conferir", () => {
    expect(windowsReleased(net())).toBe(true);
    expect(windowsReleased(net({ windows_redirect_target: "none" }))).toBe(false);
    expect(windowsReleased(net({ windows_redirect_target: "other" }))).toBe(false);
    expect(windowsReleased(net({ firewall_rule: "no" }))).toBe(false);
    expect(windowsReleased(net({ firewall_rule: "unknown" }))).toBeNull();
    expect(windowsReleased(net({ windows_redirect_target: "unknown" }))).toBeNull();
    expect(windowsReleased(net({ env: "wsl_unknown" }))).toBe(true);
    expect(windowsReleased(net({ env: "wsl_mirrored", windows_redirect_target: "none" }))).toBe(true);
    expect(windowsReleased(net({ env: "wsl_mirrored", windows_redirect_target: "unknown", firewall_rule: "no" }))).toBe(false);
    expect(windowsReleased(net({ env: "linux" }))).toBeNull();
  });

  it("textos novos sem travessão nem emoji", () => {
    for (const text of Object.values(NETWORK_TEXT)) expect(forbiddenGlyphIn(text)).toBeNull();
  });
});
