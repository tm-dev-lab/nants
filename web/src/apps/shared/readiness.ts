/*
 * Bloqueios por prontidão nas janelas (spec 093, decisão 15): o nível que libera cada ação, a aba
 * das Configurações de cada pendência e o que fazer com um 412 `prereq_missing`. Funções puras.
 */
import type { ApiFailure } from "../../api/client";
import { isMessageCode, parseCodeParams, type CodeParams } from "../../api/codes";
import type { PrereqItem } from "../../api/types";
import { READINESS_LEVELS, type Readiness, type ReadinessItem, type ReadinessLevel, type SettingsTab } from "../../desktop/api";
import { PREREQ_LABELS } from "../../copy";
import { formatNumber, getLanguage, t, type MessageKey } from "../../i18n";
import { ptBR } from "../../i18n/ptBR";

/** Nível exigido por ação: conversar (`chat`); aprovar, retomar e executar (`execute`). */
type ReadinessGate = "chat" | "execute";

/**
 * Nível da prontidão para a ação; `null` enquanto a prontidão não carregou (a UI não bloqueia
 * no escuro: o servidor ainda responde 412, tratado à parte).
 */
export function gateLevel(readiness: Readiness | null, gate: ReadinessGate): ReadinessLevel | null {
  if (readiness === null) return null;
  return gate === "chat" ? readiness.chat : readiness.execute;
}

/** A ação está bloqueada pela prontidão (nível carregado e não ok). */
export function gateBlocked(readiness: Readiness | null, gate: ReadinessGate): boolean {
  const level = gateLevel(readiness, gate);
  return level !== null && !level.ok;
}

/** Aba das Configurações de uma pendência pelo id (o servidor também manda `settings_tab`). */
export function settingsTabForPrereq(id: string): SettingsTab {
  if (id === "connection" || id.startsWith("connection_key:") || id.startsWith("connection_url:")) return "connections";
  if (id.startsWith("agent_")) return "agents";
  if (id.startsWith("pricing:")) return "pricing";
  return "machine";
}

/** Item da prontidão com o id pedido, em qualquer nível (o primeiro achado), ou `null`. */
function findReadinessItem(readiness: Readiness | null, id: string): ReadinessItem | null {
  if (readiness === null) return null;
  for (const level of READINESS_LEVELS) {
    const found = readiness[level].missing.find((item) => item.id === id);
    if (found !== undefined) return found;
  }
  return null;
}

/**
 * Rótulo de reserva de um id sem item na prontidão: os fixos do cliente e, para os ids com
 * parâmetro (T12), um texto com o parâmetro ("Chave da conexão c1"), nunca o id cru.
 */
function fallbackPrereqLabel(id: string): string {
  return knownPrereqLabel(id) ?? id;
}

/** Rótulo do cliente para um id conhecido (fixo ou com parâmetro), no idioma ativo; `null` se desconhecido. */
function knownPrereqLabel(id: string): string | null {
  const fixed = PREREQ_LABELS[id];
  if (fixed !== undefined) return fixed;
  const at = id.indexOf(":");
  const kind = at < 0 ? id : id.slice(0, at);
  const param = at < 0 ? "" : id.slice(at + 1);
  if (kind === "connection_key") return t("prereq.connectionKey", { id: param });
  if (kind === "connection_url") return t("prereq.connectionUrl", { id: param });
  if (kind === "pricing") return t("prereq.pricing", { id: param });
  return null;
}

/** Rótulos das pendências de configuração do `/api/readiness` (o servidor manda em pt-BR). */
const READINESS_ITEM_KEYS: Readonly<Record<string, MessageKey>> = {
  connection: "prereq.item.connection",
  agent_architect: "prereq.item.agent_architect",
  agent_developer: "prereq.item.agent_developer",
  agent_reviewer: "prereq.item.agent_reviewer",
};

/** Rótulo de cada item da máquina pelo `id` estável (o mesmo do `nants doctor`). */
const MACHINE_LABEL_KEYS: Readonly<Record<string, MessageKey>> = {
  git: "prereq.machine.git",
  docker_cli: "prereq.machine.docker_cli",
  docker_daemon: "prereq.machine.docker_daemon",
  docker_permission: "prereq.machine.docker_permission",
  docker_compose: "prereq.machine.docker_compose",
  disk: "prereq.machine.disk",
  wsl: "prereq.machine.wsl",
  playwright_images: "prereq.machine.playwright_images",
  windows_port_redirect: "prereq.machine.windows_port_redirect",
};

/** Comando de correção de cada item da máquina pelo `id` estável (o mesmo do `nants doctor`). */
const MACHINE_FIX_KEYS: Readonly<Record<string, MessageKey>> = {
  git: "prereq.fix.git",
  docker_cli: "prereq.fix.docker_cli",
  docker_daemon: "prereq.fix.docker_daemon",
  docker_permission: "prereq.fix.docker_permission",
  docker_compose: "prereq.fix.docker_compose",
  disk: "prereq.fix.disk",
  windows_port_redirect: "prereq.fix.windows_port_redirect",
};

/** Rótulo de cada `code` neutro do `/api/readiness` (spec 102 T05). */
const READINESS_CODE_KEYS: Readonly<Record<string, MessageKey>> = {
  ...READINESS_ITEM_KEYS,
  ...MACHINE_LABEL_KEYS,
  connection_key: "prereq.item.connection_key",
  connection_url: "prereq.item.connection_url",
  pricing: "prereq.item.pricing",
};

/** Texto de `key` com `params`, ou `null` se algum parâmetro do texto faltar. */
function withParams(key: MessageKey, params: CodeParams | undefined): string | null {
  const values = params ?? {};
  const text = t(key, values);
  return /\{\w+\}/.test(text) ? null : text;
}

/**
 * Rótulo exibido de uma pendência (spec 102):
 * - com `code` conhecido (servidor da T05), o do dicionário no idioma ativo, com os `params`
 *   neutros (o nome da conexão, o modelo);
 * - sem `code`: em pt-BR, o do servidor; nos outros idiomas, o do dicionário pelo id estável
 *   quando conhecido; id desconhecido mostra o rótulo do servidor.
 */
export function readinessItemLabel(item: ReadinessItem): string {
  const key = item.code !== undefined ? READINESS_CODE_KEYS[item.code] : undefined;
  const byCode = key !== undefined ? withParams(key, item.params) : null;
  if (byCode !== null) return byCode;
  if (getLanguage() === "pt-BR") return item.label;
  const idKey = READINESS_ITEM_KEYS[item.id];
  const label = idKey !== undefined ? t(idKey) : knownPrereqLabel(item.id);
  return label === null ? item.label : label.charAt(0).toUpperCase() + label.slice(1);
}

/** Comando de correção de uma pendência: pelo `code` da máquina no idioma ativo; senão, o do servidor. */
export function readinessItemFix(item: ReadinessItem): string | undefined {
  if (item.fix === undefined) return undefined;
  const key = item.code !== undefined ? MACHINE_FIX_KEYS[item.code] : undefined;
  return key !== undefined ? t(key) : item.fix;
}

// --- Diagnóstico da máquina (`GET /api/system/prereqs`) ---

/** Rótulo de um item do diagnóstico pelo `id` estável; id desconhecido mostra o do servidor. */
export function prereqLabel(item: Pick<PrereqItem, "id" | "label">): string {
  const key = MACHINE_LABEL_KEYS[item.id];
  return key !== undefined ? t(key) : item.label;
}

/** Comando de correção de um item do diagnóstico pelo `id` estável; senão, o do servidor. */
export function prereqFix(item: Pick<PrereqItem, "id" | "fix">): string | undefined {
  if (item.fix === undefined || item.fix === "") return undefined;
  const key = MACHINE_FIX_KEYS[item.id];
  return key !== undefined ? t(key) : item.fix;
}

const GIB = 1024 * 1024 * 1024;

/**
 * Parâmetros de um detalhe no idioma ativo: `<nome>_bytes` vira `<nome>` em GB com uma casa
 * decimal, sem arredondar para cima (igual ao `nants doctor`: "12,3 GB" ou "12.3 GB").
 */
function detailParams(params: CodeParams | null): Record<string, string | number> {
  const out: Record<string, string | number> = { ...(params ?? {}) };
  for (const [name, value] of Object.entries(params ?? {})) {
    if (!name.endsWith("_bytes") || typeof value !== "number") continue;
    const tenths = Math.floor((value * 10) / GIB);
    out[name.slice(0, -"_bytes".length)] = `${formatNumber(tenths / 10, { minimumFractionDigits: 1, maximumFractionDigits: 1 })} GB`;
  }
  return out;
}

/** Texto de um detalhe neutro `{code, params}` (chave `prereq.detail.<code>`), ou `null`. */
function detailText(code: unknown, params: unknown, extra: Record<string, string> = {}): string | null {
  if (!isMessageCode(code)) return null;
  const key = `prereq.detail.${code}`;
  if (!Object.hasOwn(ptBR, key)) return null;
  return withParams(key as MessageKey, { ...detailParams(parseCodeParams(params)), ...extra });
}

/**
 * Detalhe exibido de um item do diagnóstico (spec 102 T05): o detalhe neutro (`detail_code`,
 * `detail_params` e os itens de `detail_parts`, unidos por "; ") no idioma ativo; sem código
 * traduzível, o `detail` do servidor (pt-BR). A versão (`version`) fica com quem exibe.
 */
export function prereqDetail(item: PrereqItem): string | undefined {
  const parts = Array.isArray(item.detail_parts)
    ? item.detail_parts.map((p: unknown) => (typeof p === "object" && p !== null ? detailText((p as { code?: unknown }).code, (p as { params?: unknown }).params) : null))
    : [];
  if (parts.every((p): p is string => p !== null)) {
    const text = detailText(item.detail_code, item.detail_params, parts.length > 0 ? { list: parts.join("; ") } : {});
    if (text !== null) return text;
  }
  return item.detail === "" ? undefined : item.detail;
}

/** Pendência pelo id: o RÓTULO e a aba do `/api/readiness` quando há item com o mesmo id. */
export function prereqItem(id: string, readiness: Readiness | null): ReadinessItem {
  const found = findReadinessItem(readiness, id);
  if (found !== null) return found;
  return { id, label: fallbackPrereqLabel(id), settings_tab: settingsTabForPrereq(id) };
}

/**
 * 412 `prereq_missing` do servidor: as pendências como itens. Com a prontidão (relida depois do
 * 412), cada id usa o rótulo e a aba do item correspondente; sem ela, os rótulos de reserva.
 */
export function prereqItems(failure: ApiFailure, readiness: Readiness | null = null): ReadinessItem[] | null {
  if (failure.status !== 412 || failure.reason !== "prereq_missing") return null;
  return (failure.missing ?? []).map((id) => prereqItem(id, readiness));
}

/** Pendências já montadas, com os rótulos da prontidão atual (que pode ter sido relida depois). */
export function relabelPrereqs(items: readonly ReadinessItem[], readiness: Readiness | null): ReadinessItem[] {
  return items.map((item) => {
    const found = findReadinessItem(readiness, item.id);
    return found ?? item;
  });
}

/** "Falta configurar: A, B." a partir dos rótulos (sem repetir). */
export function prereqMessage(items: readonly ReadinessItem[]): string {
  const labels = [...new Set(items.map(readinessItemLabel))];
  return labels.length > 0 ? t("prereq.missing", { items: labels.join(", ") }) : t("prereq.missingGeneric");
}

/** A falha é o 412 `prereq_missing`. */
export function isPrereqFailure(failure: ApiFailure): boolean {
  return failure.status === 412 && failure.reason === "prereq_missing";
}

/**
 * Mensagem de um 412 `prereq_missing` com os rótulos da prontidão RELIDA (o mesmo caminho do
 * Plano, do sino e do `PausedRunCard`): relê e usa a resposta nova (ou a atual, se a leitura
 * falhar). Fora do 412, `null`.
 */
export async function prereqFailureMessage(
  failure: ApiFailure,
  reload: () => Promise<Readiness | null>,
  current: Readiness | null,
): Promise<string | null> {
  if (!isPrereqFailure(failure)) return null;
  const fresh = await reload();
  return prereqMessage(prereqItems(failure, fresh ?? current) ?? []);
}
