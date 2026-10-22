/* Acesso pela rede (spec 097, T02): script do PowerShell para o WSL e textos com os valores reais. */
import { describe, expect, it } from "vitest";
import { forbiddenGlyphIn } from "../../desktop/glyphs";
import {
  NETWORK_TEXT,
  nantsLanAddress,
  wslPowerShellScript,
} from "./networkModel";

const EXPECTED_3000 = [
  "$ip = (wsl hostname -I).Trim().Split(' ')[0]",
  "foreach ($p in @(3000) + (3001..3010)) {",
  "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
  "  netsh interface portproxy add v4tov4 listenport=$p listenaddress=0.0.0.0 connectport=$p connectaddress=$ip",
  "}",
  'Remove-NetFirewallRule -DisplayName "Nants" -ErrorAction SilentlyContinue',
  'New-NetFirewallRule -DisplayName "Nants" -Direction Inbound -Action Allow -Protocol TCP -LocalPort 3000,3001-3010 -Profile Private -RemoteAddress LocalSubnet',
].join("\n");

describe("script do PowerShell (WSL)", () => {
  it("usa a porta do Nants e a faixa reais", () => {
    expect(wslPowerShellScript(3000, { start: 3001, end: 3010 })).toBe(EXPECTED_3000);
    const other = wslPowerShellScript(8080, { start: 9000, end: 9019 });
    expect(other).toContain("foreach ($p in @(8080) + (9000..9019)) {");
    expect(other).toContain("-LocalPort 8080,9000-9019 -Profile Private -RemoteAddress LocalSubnet");
    expect(other).not.toContain("3000");
    expect(wslPowerShellScript(3000, { start: 3001, end: 3001 }).endsWith("-LocalPort 3000,3001 -Profile Private -RemoteAddress LocalSubnet")).toBe(
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
    // Só a rede local: perfil Privado e sub-rede local, no fim da linha (spec 099).
    expect(lines[create]).toMatch(/ -Profile Private -RemoteAddress LocalSubnet$/);
    expect(lines.filter((l) => l.includes("NetFirewallRule"))).toHaveLength(2);
    expect(lines[0]).toContain("wsl hostname -I");
    // Nenhum IP fixo: o destino é sempre o lido na hora.
    expect(lines.join("\n").replaceAll("0.0.0.0", "")).not.toMatch(/\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}/);
    expect(lines.filter((l) => l.includes("connectaddress=$ip"))).toHaveLength(1);
  });
});

describe("textos da seção", () => {
  it("endereço do Nants com o host da página; no loopback pede o IP do computador", () => {
    expect(nantsLanAddress("192.168.0.68", 3000)).toBe("http://192.168.0.68:3000");
    expect(nantsLanAddress("localhost", 3000)).toBe("http://<IP deste computador>:3000");
  });

  it("sem travessão nem emoji", () => {
    for (const text of [NETWORK_TEXT.hint, NETWORK_TEXT.unverified, NETWORK_TEXT.wslIntro, NETWORK_TEXT.wslPrivate, NETWORK_TEXT.wslRestart, NETWORK_TEXT.wslMirrored]) {
      expect(forbiddenGlyphIn(text)).toBeNull();
    }
    // O script é código (tem `+` do PowerShell), mas também sem travessão nem emoji.
    expect(EXPECTED_3000).not.toMatch(/[\u2013\u2014]|\p{Extended_Pictographic}/u);
    expect(NETWORK_TEXT.wslIntro).toContain("PowerShell como administrador");
    expect(NETWORK_TEXT.wslIntro).toContain("só para aparelhos da rede local");
    expect(NETWORK_TEXT.wslIntro).toContain("marcada como privada no Windows");
    expect(NETWORK_TEXT.wslPrivate).toContain("rede como pública");
    expect(NETWORK_TEXT.wslPrivate).toContain("Configurações do Windows, Rede e Internet, propriedades da rede, Rede privada");
    expect(NETWORK_TEXT.wslRestart).toContain("reiniciar");
    expect(NETWORK_TEXT.wslMirrored).toContain("networkingMode=mirrored");
  });
});
