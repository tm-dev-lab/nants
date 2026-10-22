/*
 * Acesso pela rede (spec 097, decisão 5): validação da visão de rede vinda da API (`GET /api/config`
 * e `GET .../app`), a seção `network` do estado da interface global (`{released: {start, end}}`) e
 * a regra "a faixa em uso está marcada como liberada".
 *
 * O Nants não tem como verificar se a faixa foi liberada na rede: "Já liberei" só registra a
 * declaração de quem usa. Se a faixa em uso mudar, a marca deixa de valer e o aviso volta.
 */
import type { LanRange, NetworkView } from "../api/types";
import { t } from "../i18n";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isPort = (value: unknown): value is number => typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= 65535;

/** Faixa válida (`start <= end`, portas de 1 a 65535), ou `null`. */
export function parseLanRange(value: unknown): LanRange | null {
  if (!isRecord(value) || !isPort(value.start) || !isPort(value.end) || value.start > value.end) return null;
  return { start: value.start, end: value.end };
}

/** Visão de rede a partir de um objeto com `nants_port`, `lan_view_ports` e `wsl`, ou `null`. */
export function parseNetworkView(value: unknown): NetworkView | null {
  if (!isRecord(value) || !isPort(value.nants_port)) return null;
  return { nants_port: value.nants_port, lan_view_ports: parseLanRange(value.lan_view_ports), wsl: value.wsl === true };
}

/** Seção `network` do estado da interface global: a faixa marcada como liberada. */
interface NetworkUiState {
  released: LanRange | null;
}

export function parseNetworkState(value: unknown): NetworkUiState {
  return { released: isRecord(value) ? parseLanRange(value.released) : null };
}

/** Valor gravado na seção `network` ao marcar (faixa) ou desmarcar (`null`). */
export function networkStateValue(released: LanRange | null): NetworkUiState {
  return { released: released === null ? null : { start: released.start, end: released.end } };
}

function sameRange(a: LanRange | null, b: LanRange | null): boolean {
  return a !== null && b !== null && a.start === b.start && a.end === b.end;
}

/** A faixa em uso foi marcada como liberada (outra faixa marcada não vale). */
export function rangeReleased(released: LanRange | null, current: LanRange | null): boolean {
  return sameRange(released, current);
}

/** "3001 a 3010" (ou só "3001" com uma porta). */
export function rangeLabel(range: LanRange): string {
  return range.start === range.end ? String(range.start) : t("network.range", { start: range.start, end: range.end });
}
