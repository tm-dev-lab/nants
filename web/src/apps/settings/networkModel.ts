/*
 * Seção "Acesso pela rede" das Configurações (spec 097, decisão 5; spec 111, T03): o interruptor da
 * opção salva "Acesso pela rede local", os avisos de reinício e de coerência com o Windows e as
 * instruções por ambiente (Linux, WSL NAT, WSL espelhado ou WSL sem o modo conferido), com os
 * scripts do PowerShell para ligar e para desfazer. O Nants nunca executa os scripts: eles só
 * aparecem para copiar. Funções puras.
 */
import type { LanRange, NetworkCoherenceCode, NetworkConfig, NetworkEnv, NetworkModeSource, RedirectTarget, TriState } from "../../api/types";
import { localized, t } from "../../i18n";
import { parseLanRange } from "../../desktop/network";
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
  wslIntro: "settings.network.wslIntro",
  wslRestart: "settings.network.wslRestart",
  wslMirrored: "settings.network.wslMirrored",
  lanLabel: "settings.network.lanLabel",
  lanHint: "settings.network.lanHint",
  security: "settings.network.security",
  loading: "settings.network.loading",
  loadFailed: "settings.network.loadFailed",
  nextStart: "settings.network.nextStart",
  restartTail: "settings.network.restartTail",
  savedNow: "settings.network.savedNow",
  runOn: "settings.network.runOn",
  runOff: "settings.network.runOff",
  fromFlag: "settings.network.fromFlag",
  missingScript: "settings.network.missingScript",
  missingFirewall: "settings.network.missingFirewall",
  otherTarget: "settings.network.otherTarget",
  willAdopt: "settings.network.willAdopt",
  enableTitle: "settings.network.enableTitle",
  undoTitle: "settings.network.undoTitle",
  undoIntro: "settings.network.undoIntro",
  mirroredIntro: "settings.network.mirroredIntro",
  mirroredEnableIntro: "settings.network.mirroredEnableIntro",
  mirroredUndoIntro: "settings.network.mirroredUndoIntro",
  unknownNote: "settings.network.unknownNote",
  windowsReleased: "settings.network.windowsReleased",
  linuxHintAbove: "settings.network.linuxHintAbove",
  windowsPending: "settings.network.windowsPending",
});

/** Nome da regra de firewall criada pelo script (nenhum nome legado). */
const FIREWALL_RULE_NAME = "Nants";

/**
 * Endereço do Nants na rede, com o host da página. Aberto neste mesmo computador (loopback), o host
 * da página não serve para outro aparelho: o texto pede o IP do computador.
 */
export function nantsLanAddress(pageHostname: string, nantsPort: number): string {
  if (isLoopbackHost(pageHostname)) return `http://${t("settings.network.thisComputer")}:${nantsPort}`;
  return `http://${pageHostname}:${nantsPort}`;
}

/** Portas do firewall: "3000,3001-3010" (uma porta só na faixa vira "3000,3001"; sem faixa, "3000"). */
function firewallPorts(nantsPort: number, range: LanRange | null): string {
  if (range === null) return String(nantsPort);
  const span = range.start === range.end ? String(range.start) : `${range.start}-${range.end}`;
  return `${nantsPort},${span}`;
}

/** Lista de portas do PowerShell: `@(3000) + (3001..3010)` (sem faixa, `@(3000)`). */
function portList(nantsPort: number, range: LanRange | null): string {
  return range === null ? `@(${nantsPort})` : `@(${nantsPort}) + (${range.start}..${range.end})`;
}

const REMOVE_RULE = `Remove-NetFirewallRule -DisplayName "${FIREWALL_RULE_NAME}" -ErrorAction SilentlyContinue`;

/**
 * Regra "Nants" para qualquer perfil de rede (Privado e Público, spec 112, decisão 5), ainda restrita
 * à sub-rede local. Sem `-Profile`: quem protege o Nants é o dispositivo autorizado, não o perfil.
 */
function newRule(nantsPort: number, range: LanRange | null): string {
  return `New-NetFirewallRule -DisplayName "${FIREWALL_RULE_NAME}" -Direction Inbound -Action Allow -Protocol TCP -LocalPort ${firewallPorts(nantsPort, range)} -RemoteAddress LocalSubnet`;
}

/**
 * Script de ligar no WSL NAT (PowerShell como administrador), idempotente: lê o IP do WSL na hora,
 * remove e recria o `portproxy` da porta do Nants e de cada porta da faixa e recria a regra de
 * firewall, para qualquer perfil e restrita à sub-rede local (spec 112).
 *
 * Segurança (spec 112): o `connectaddress` é sempre o IP do WSL (`$ip`), nunca 127.0.0.1. Apontando
 * para 127.0.0.1, as conexões dos aparelhos chegariam ao Nants como loopback e entrariam sem
 * dispositivo autorizado.
 */
export function wslPowerShellScript(nantsPort: number, range: LanRange | null): string {
  return [
    "$ip = (wsl hostname -I).Trim().Split(' ')[0]",
    `foreach ($p in ${portList(nantsPort, range)}) {`,
    "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
    "  netsh interface portproxy add v4tov4 listenport=$p listenaddress=0.0.0.0 connectport=$p connectaddress=$ip",
    "}",
    REMOVE_RULE,
    newRule(nantsPort, range),
  ].join("\n");
}

/**
 * Script de desfazer no WSL NAT, idempotente: remove os `portproxy v4tov4 listenaddress=0.0.0.0` da
 * porta do Nants e da faixa e a regra "Nants", e mostra a tabela que sobrou. Nunca cria nada.
 */
export function wslUndoScript(nantsPort: number, range: LanRange | null): string {
  return [
    `foreach ($p in ${portList(nantsPort, range)}) {`,
    "  netsh interface portproxy delete v4tov4 listenport=$p listenaddress=0.0.0.0 | Out-Null",
    "}",
    REMOVE_RULE,
    "netsh interface portproxy show v4tov4",
  ].join("\n");
}

/** Script de ligar no WSL espelhado: só a regra de firewall (sem `portproxy`), idempotente. */
export function mirroredEnableScript(nantsPort: number, range: LanRange | null): string {
  return [REMOVE_RULE, newRule(nantsPort, range)].join("\n");
}

/** Script de desfazer no WSL espelhado: só remove a regra de firewall. */
export function mirroredUndoScript(): string {
  return REMOVE_RULE;
}

/** Scripts de ligar e de desfazer do ambiente; `null` no Linux direto (sem script). */
export function networkScripts(net: Pick<NetworkConfig, "env" | "lan_port" | "view_ports">): { enable: string; undo: string } | null {
  switch (net.env) {
    case "linux":
      return null;
    case "wsl_mirrored":
      return { enable: mirroredEnableScript(net.lan_port, net.view_ports), undo: mirroredUndoScript() };
    case "wsl_nat":
    case "wsl_unknown":
      return { enable: wslPowerShellScript(net.lan_port, net.view_ports), undo: wslUndoScript(net.lan_port, net.view_ports) };
  }
}

// --- Estado da API ---

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isPort = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 65535;

function oneOf<T extends string>(value: unknown, options: readonly T[], fallback: T): T {
  return typeof value === "string" && (options as readonly string[]).includes(value) ? (value as T) : fallback;
}

const ENVS: readonly NetworkEnv[] = ["linux", "wsl_nat", "wsl_mirrored", "wsl_unknown"];
const SOURCES: readonly NetworkModeSource[] = ["flag", "host", "saved", "adopted", "default"];
const TRI: readonly TriState[] = ["yes", "no", "unknown"];
const TARGETS: readonly RedirectTarget[] = ["this_wsl", "other", "none", "unknown"];
const COHERENCE: readonly NetworkCoherenceCode[] = ["ok", "will_adopt", "needs_script", "other_target", "missing_rule", "still_redirected", "unknown"];

/**
 * Estado de rede do `GET /api/config/network`, validado; `null` se faltar o essencial (ambiente,
 * modo e portas). Valores fora do conhecido do Windows viram "não deu para conferir".
 */
export function parseNetworkConfig(value: unknown): NetworkConfig | null {
  if (!isRecord(value)) return null;
  if (typeof value.env !== "string" || !(ENVS as readonly string[]).includes(value.env)) return null;
  if (typeof value.lan !== "boolean" || !isPort(value.port)) return null;
  const lanPort = isPort(value.lan_port) && value.lan_port > 0 ? value.lan_port : value.port;
  return {
    env: value.env as NetworkEnv,
    saved_lan: typeof value.saved_lan === "boolean" ? value.saved_lan : null,
    lan: value.lan,
    mode_source: oneOf(value.mode_source, SOURCES, "default"),
    port: value.port,
    lan_port: lanPort,
    view_ports: parseLanRange(value.view_ports),
    windows_redirect: oneOf(value.windows_redirect, TRI, "unknown"),
    windows_redirect_target: oneOf(value.windows_redirect_target, TARGETS, "unknown"),
    coherence: oneOf(value.coherence, COHERENCE, "unknown"),
    firewall_rule: oneOf(value.firewall_rule, TRI, "unknown"),
    restart_required: value.restart_required === true,
    restart_command: typeof value.restart_command === "string" && value.restart_command.trim() !== "" ? value.restart_command : "nants",
  };
}

/** A opção salva (nunca escolhida conta como desligada, o padrão do próximo início). */
export function savedLan(net: Pick<NetworkConfig, "saved_lan">): boolean {
  return net.saved_lan === true;
}

/**
 * Pede reinício (observação da revisão da T02): só quando a opção salva difere do modo desta
 * execução E a pessoa mudou a opção nesta tela, ou o modo não veio do `--lan`. O `restart_required`
 * da API não basta: ele vem `true` numa execução com `--lan` sem opção salva.
 */
export function restartNeeded(net: Pick<NetworkConfig, "saved_lan" | "lan" | "mode_source">, changedHere: boolean): boolean {
  if (savedLan(net) === net.lan) return false;
  return changedHere || net.mode_source !== "flag";
}

/**
 * Texto do código de coerência que vem do servidor (`coherence`, a mesma regra do `nants doctor`);
 * `null` quando não há o que dizer (de acordo ou não deu para conferir).
 */
export function coherenceText(code: NetworkCoherenceCode, net: Pick<NetworkConfig, "lan_port">): string | null {
  switch (code) {
    case "needs_script":
      return NETWORK_TEXT.missingScript;
    case "missing_rule":
      return NETWORK_TEXT.missingFirewall;
    case "other_target":
      return NETWORK_TEXT.otherTarget;
    case "still_redirected":
      return t("settings.network.stillRedirected", { port: net.lan_port });
    case "will_adopt":
      return NETWORK_TEXT.willAdopt;
    case "ok":
    case "unknown":
      return null;
  }
}

/** O código pede uma ação (aviso em âmbar); a adoção só informa. */
export function coherenceIsWarning(code: NetworkCoherenceCode): boolean {
  return code === "needs_script" || code === "missing_rule" || code === "other_target" || code === "still_redirected";
}

/** Script que já abre pelo código: o de ligar, o de desfazer ou nenhum. */
export function scriptForCoherence(code: NetworkCoherenceCode): "enable" | "undo" | null {
  if (code === "needs_script" || code === "missing_rule" || code === "other_target") return "enable";
  return code === "still_redirected" ? "undo" : null;
}

// --- Estado real no Windows ---

/**
 * Estado da liberação conferido no Windows, quando dá para saber: no WSL NAT (ou desconhecido), o
 * redirecionamento da porta para este WSL e a regra de firewall; no espelhado, só a regra. `null`
 * no Linux e quando o Windows não deu para conferir: aí vale a marcação manual.
 */
export function windowsReleased(net: NetworkConfig): boolean | null {
  if (net.env === "linux" || net.firewall_rule === "unknown") return null;
  if (net.env === "wsl_mirrored") return net.firewall_rule === "yes";
  if (net.windows_redirect_target === "unknown") return null;
  return net.windows_redirect_target === "this_wsl" && net.firewall_rule === "yes";
}
