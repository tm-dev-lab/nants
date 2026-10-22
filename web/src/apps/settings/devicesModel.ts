/*
 * Jornada guiada do Acesso pela rede e dispositivos (spec 112, decisão 8; T02). Funções puras.
 *
 * Passos por ambiente:
 * - WSL NAT (e WSL sem o modo conferido): ligar, preparar o Windows, adicionar;
 * - WSL espelhado: ligar, firewall, adicionar;
 * - Linux direto: ligar, adicionar.
 *
 * Cada passo mostra o estado conferido pelo servidor (`GET /api/config/network`), e o seguinte só
 * fica habilitado quando o anterior estiver pronto. Com o Windows não conferido, o passo de
 * adicionar fica habilitado com a ressalva, para não travar a pessoa. A "volta automática" sai de
 * reler o estado: se o Windows deixar de estar pronto, o passo 2 volta a ficar pendente.
 */
import type { NetworkConfig } from "../../api/types";
import type { MessageKey } from "../../i18n";
import { windowsReleased } from "./networkModel";

type StepId = "enable" | "windows" | "add";

/**
 * Estado do passo:
 * - `done`: pronto (ligar e Windows);
 * - `pending`: falta fazer (ligar e Windows);
 * - `unknown`: não deu para conferir (Windows) ou habilitado com a ressalva (adicionar);
 * - `ready`: habilitado (adicionar);
 * - `blocked`: o passo anterior não está pronto.
 */
type StepStatus = "done" | "pending" | "unknown" | "ready" | "blocked";

export interface JourneyStep {
  id: StepId;
  status: StepStatus;
  title: MessageKey;
  /** Frase do estado (ou o motivo, quando bloqueado). */
  detail: MessageKey;
}

/** O passo 2 existe só no WSL (NAT, espelhado ou sem conferir). */
function hasWindowsStep(net: Pick<NetworkConfig, "env">): boolean {
  return net.env !== "linux";
}

/**
 * Passo 1 pronto com a opção salva como ligada ou com esta execução na rede (`--lan` ou `--host`,
 * mesmo com a opção salva desligada: o servidor já exige dispositivo). Sem reiniciar, ele avisa de leve que vale ao reiniciar, sem bloquear o passo 2: o
 * script do Windows não depende do reinício (ligar, rodar o script e reiniciar uma vez só).
 */
function enableStep(net: NetworkConfig): JourneyStep {
  const title = "devices.step.enable.title";
  const on = net.saved_lan === true || net.lan;
  if (!on) return { id: "enable", status: "pending", title, detail: "devices.step.enable.off" };
  return { id: "enable", status: "done", title, detail: net.lan ? "devices.step.enable.done" : "devices.step.enable.restart" };
}

function windowsStep(net: NetworkConfig, enable: JourneyStep): JourneyStep {
  const title = net.env === "wsl_mirrored" ? "devices.step.firewall.title" : "devices.step.windows.title";
  if (enable.status !== "done") return { id: "windows", status: "blocked", title, detail: "devices.reason.enableFirst" };
  const released = windowsReleased(net);
  if (released === null) return { id: "windows", status: "unknown", title, detail: "devices.step.windows.unknown" };
  return released
    ? { id: "windows", status: "done", title, detail: "devices.step.windows.done" }
    : { id: "windows", status: "pending", title, detail: "devices.step.windows.pending" };
}

/**
 * Passo de adicionar: exige o passo anterior pronto (ou o Windows desconhecido, com a ressalva) e o
 * Nants rodando em modo rede nesta execução (reiniciado). O motivo segue a ordem da jornada: ligar,
 * rodar o script e, por último, reiniciar.
 */
function addStep(net: NetworkConfig, previous: JourneyStep): JourneyStep {
  const title = "devices.step.add.title";
  if (previous.status === "blocked" || (previous.id === "enable" && previous.status !== "done")) {
    return { id: "add", status: "blocked", title, detail: "devices.reason.enableFirst" };
  }
  if (previous.status === "pending") {
    return { id: "add", status: "blocked", title, detail: net.env === "wsl_mirrored" ? "devices.reason.firewallFirst" : "devices.reason.scriptFirst" };
  }
  if (!net.lan) return { id: "add", status: "blocked", title, detail: "devices.reason.restartFirst" };
  if (previous.status === "unknown") return { id: "add", status: "unknown", title, detail: "devices.step.add.unknown" };
  return { id: "add", status: "ready", title, detail: "devices.step.add.ready" };
}

/** Os passos do ambiente, em ordem, cada um com o estado conferido. */
export function journeySteps(net: NetworkConfig): JourneyStep[] {
  const enable = enableStep(net);
  if (!hasWindowsStep(net)) return [enable, addStep(net, enable)];
  const windows = windowsStep(net, enable);
  return [enable, windows, addStep(net, windows)];
}

/** O botão de adicionar fica habilitado (pronto ou com a ressalva). */
export function canAddDevice(step: JourneyStep): boolean {
  return step.status === "ready" || step.status === "unknown";
}

/** O passo conta como concluído (marca de feito). */
export function stepIsDone(status: StepStatus): boolean {
  return status === "done" || status === "ready";
}

// --- Código de 6 dígitos ---

/** Só os dígitos, no máximo 6 (colar "123 456" vira "123456"). */
export function codeDigits(raw: string): string {
  return raw.replace(/\D/g, "").slice(0, 6);
}

/** Código em dois grupos de 3, para ler em voz alta: "123 456". */
export function groupedCode(code: string): string {
  return code.length === 6 ? `${code.slice(0, 3)} ${code.slice(3)}` : code;
}

/** Erro da troca do código para a página de entrada. */
type PairErrorKind =
  | { kind: "invalid"; attemptsLeft: number }
  | { kind: "locked" }
  | { kind: "inactive" }
  | { kind: "rate_limited" }
  | { kind: "other" };

/** Classifica a falha de `POST /api/devices/pair` pelo `code` estável do servidor. */
export function pairErrorKind(failure: { code?: string; params?: Readonly<Record<string, string | number>> }): PairErrorKind {
  switch (failure.code) {
    case "device_code_invalid": {
      const left = failure.params?.attempts_left;
      const attemptsLeft = typeof left === "number" && Number.isSafeInteger(left) && left >= 0 ? left : 0;
      return attemptsLeft === 0 ? { kind: "locked" } : { kind: "invalid", attemptsLeft };
    }
    case "device_code_locked":
      return { kind: "locked" };
    case "device_code_inactive":
      return { kind: "inactive" };
    case "device_pair_rate_limited":
      return { kind: "rate_limited" };
    default:
      return { kind: "other" };
  }
}

// --- QR code ---

/**
 * Caminho SVG dos módulos escuros (um retângulo por sequência horizontal), com a borda já incluída
 * na matriz. Um `<path>` só, sem HTML cru.
 */
export function qrPath(modules: readonly (readonly boolean[])[]): string {
  const parts: string[] = [];
  modules.forEach((row, y) => {
    let x = 0;
    while (x < row.length) {
      if (row[x] !== true) {
        x += 1;
        continue;
      }
      const start = x;
      while (x < row.length && row[x] === true) x += 1;
      parts.push(`M${start} ${y}h${x - start}v1h${start - x}z`);
    }
  });
  return parts.join("");
}
