/*
 * Seção "Acesso pela rede" das Configurações (spec 097, decisão 5): textos com os valores reais e o
 * script do PowerShell para o WSL. O Nants nunca executa o script: ele só aparece para copiar, e só
 * quando o servidor roda no WSL.
 */
import type { LanRange } from "../../api/types";
import { localized, t } from "../../i18n";
import { isLoopbackHost } from "../app/appModel";

/** Textos da seção, no idioma ativo (spec 102 T05). */
export const NETWORK_TEXT = localized({
  title: "settings.network.title",
  hint: "settings.network.hint",
  unverified: "settings.network.unverified",
  released: "settings.network.released",
  pending: "settings.network.pending",
  markReleased: "settings.network.markReleased",
  markPending: "settings.network.markPending",
  address: "settings.network.address",
  ports: "settings.network.ports",
  state: "settings.network.state",
  script: "settings.network.script",
  wslTitle: "settings.network.wslTitle",
  wslIntro: "settings.network.wslIntro",
  wslRestart: "settings.network.wslRestart",
  wslPrivate: "settings.network.wslPrivate",
  wslMirrored: "settings.network.wslMirrored",
});

/** Nome da regra de firewall criada pelo script. */
const FIREWALL_RULE_NAME = "Nants";

/**
 * Endereço do Nants na rede, com o host da página. Aberto neste mesmo computador (loopback), o host
 * da página não serve para outro aparelho: o texto pede o IP do computador.
 */
export function nantsLanAddress(pageHostname: string, nantsPort: number): string {
  if (isLoopbackHost(pageHostname)) return `http://${t("settings.network.thisComputer")}:${nantsPort}`;
  return `http://${pageHostname}:${nantsPort}`;
}

/** Portas do firewall: "3000,3001-3010" (uma porta só na faixa vira "3000,3001"). */
function firewallPorts(nantsPort: number, range: LanRange): string {
  const span = range.start === range.end ? String(range.start) : `${range.start}-${range.end}`;
  return `${nantsPort},${span}`;
}

/**
 * Script do PowerShell (como administrador), idempotente: lê o IP do WSL na hora, remove e recria
 * o `portproxy` da porta do Nants e de cada porta da faixa e recria a regra de firewall, restrita ao
 * perfil Privado e à sub-rede local (spec 099).
 */
export function wslPowerShellScript(nantsPort: number, range: LanRange): string {
  return [
    "$ip = (wsl hostname -I).Trim().Split(' ')[0]",
    `foreach ($p in @(${nantsPort}) + (${range.start}..${range.end})) {`,
    "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
    "  netsh interface portproxy add v4tov4 listenport=$p listenaddress=0.0.0.0 connectport=$p connectaddress=$ip",
    "}",
    `Remove-NetFirewallRule -DisplayName "${FIREWALL_RULE_NAME}" -ErrorAction SilentlyContinue`,
    `New-NetFirewallRule -DisplayName "${FIREWALL_RULE_NAME}" -Direction Inbound -Action Allow -Protocol TCP -LocalPort ${firewallPorts(nantsPort, range)} -Profile Private -RemoteAddress LocalSubnet`,
  ].join("\n");
}
