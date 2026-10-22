/*
 * Dispositivos autorizados na rede local (spec 112, T02): cliente da API da T01.
 *
 * - `GET`/`POST`/`DELETE /api/devices/code`: estado da janela de adicionar (aberta, fechada ou
 *   invalidada por erros), com o código de 6 dígitos, a contagem até a troca e os endereços de
 *   entrada para o outro aparelho;
 * - `POST /api/devices/pair {code, name?}`: a troca do código pelo aparelho novo (rota aberta);
 * - `GET /api/devices`, `PATCH`/`DELETE /api/devices/:id` e `DELETE /api/devices`: a lista.
 *
 * Toda resposta é validada: campo fora do contrato vira `invalid_response`.
 */
import { ptBR } from "../i18n/ptBR";
import { request, type ApiResult } from "./client";

type CodeWindowState = "open" | "closed" | "locked";

/** Janela de adicionar dispositivo, como a interface usa. */
export interface CodeWindow {
  state: CodeWindowState;
  /** Código de 6 dígitos; `null` fora da janela aberta. */
  code: string | null;
  /** Segundos até o código trocar (janela aberta). */
  secondsToNext: number;
  /** Segundos até a janela fechar sozinha (janela aberta). */
  expiresIn: number;
  /** Endereços completos da página de entrada (`http://IP:porta/connect`). */
  entryUrls: string[];
  entryPath: string;
  maxErrors: number;
}

/** Dispositivo autorizado (sem o token, que nunca sai do servidor). */
export interface DeviceView {
  id: string;
  name: string;
  /** Milissegundos desde a época. */
  createdAt: number;
  lastSeen: number;
  thisComputer: boolean;
  /** O dispositivo desta página. */
  current: boolean;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isCount = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;

/** Só endereços `http(s):` (vão para o QR e para o texto). */
function isHttpUrl(value: unknown): value is string {
  if (typeof value !== "string" || value.length > 200) return false;
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

/** Corpo de `/api/devices/code` validado; `null` fora do contrato. */
export function parseCodeWindow(value: unknown): CodeWindow | null {
  if (!isRecord(value)) return null;
  const state = value.state;
  if (state !== "open" && state !== "closed" && state !== "locked") return null;
  const entryUrls = Array.isArray(value.entry_urls) ? value.entry_urls.filter(isHttpUrl) : [];
  const entryPath = typeof value.entry_path === "string" && value.entry_path.startsWith("/") ? value.entry_path : "/connect";
  const maxErrors = isCount(value.max_errors) && value.max_errors > 0 ? value.max_errors : 5;
  if (state !== "open") return { state, code: null, secondsToNext: 0, expiresIn: 0, entryUrls, entryPath, maxErrors };
  if (typeof value.code !== "string" || !/^\d{6}$/.test(value.code)) return null;
  if (!isCount(value.seconds_to_next) || !isCount(value.expires_in)) return null;
  return { state, code: value.code, secondsToNext: value.seconds_to_next, expiresIn: value.expires_in, entryUrls, entryPath, maxErrors };
}

/** Um dispositivo da API validado; `null` fora do contrato. */
function parseDevice(value: unknown): DeviceView | null {
  if (!isRecord(value)) return null;
  if (typeof value.id !== "string" || value.id === "" || typeof value.name !== "string") return null;
  if (!isCount(value.created_at) || !isCount(value.last_seen)) return null;
  return {
    id: value.id,
    name: value.name,
    createdAt: value.created_at,
    lastSeen: value.last_seen,
    thisComputer: value.this_computer === true,
    current: value.current === true,
  };
}

/** Lista de `GET /api/devices`; `null` fora do contrato. Itens inválidos são descartados. */
export function parseDeviceList(value: unknown): DeviceView[] | null {
  if (!isRecord(value) || !Array.isArray(value.devices)) return null;
  return value.devices.map(parseDevice).filter((d): d is DeviceView => d !== null);
}

function invalid<T>(status: number): ApiResult<T> {
  return {
    ok: false,
    error: { kind: "invalid_response", status, message: ptBR["api.invalidResponse"], messageKey: "api.invalidResponse" },
  };
}

function mapped<T>(result: ApiResult<unknown>, parse: (value: unknown) => T | null): ApiResult<T> {
  if (!result.ok) return result;
  const value = parse(result.data);
  return value === null ? invalid(200) : { ok: true, data: value };
}

const CODE_PATH = "/api/devices/code";

/** Estado da janela de adicionar (o código atual, se aberta). */
export function getDeviceCode(): Promise<ApiResult<CodeWindow>> {
  return request<unknown>({ method: "GET", path: CODE_PATH }).then((r) => mapped(r, parseCodeWindow));
}

/** Abre (ou reabre) a janela de adicionar: código novo e erros zerados. */
export function openDeviceCode(): Promise<ApiResult<CodeWindow>> {
  return request<unknown>({ method: "POST", path: CODE_PATH }).then((r) => mapped(r, parseCodeWindow));
}

/** Fecha a janela de adicionar. */
export function closeDeviceCode(): Promise<ApiResult<null>> {
  return request<unknown>({ method: "DELETE", path: CODE_PATH }).then((r) => (r.ok ? { ok: true, data: null } : r));
}

/** Troca o código de 6 dígitos (aparelho novo). O servidor grava o cookie do dispositivo. */
export function pairDevice(code: string, name?: string): Promise<ApiResult<DeviceView>> {
  const body = name === undefined ? { code } : { code, name };
  return request<unknown>({ method: "POST", path: "/api/devices/pair", body: JSON.stringify(body) }).then((r) =>
    mapped(r, (v) => (isRecord(v) ? parseDevice(v.device) : null)),
  );
}

/** Dispositivos autorizados, com o desta página marcado. */
export function listDevices(): Promise<ApiResult<DeviceView[]>> {
  return request<unknown>({ method: "GET", path: "/api/devices" }).then((r) => mapped(r, parseDeviceList));
}

/** Renomeia um dispositivo. */
export function renameDevice(id: string, name: string): Promise<ApiResult<DeviceView>> {
  return request<unknown>({ method: "PATCH", path: `/api/devices/${encodeURIComponent(id)}`, body: JSON.stringify({ name }) }).then((r) =>
    mapped(r, (v) => (isRecord(v) ? parseDevice(v.device) : null)),
  );
}

/** Revoga um dispositivo (as conexões abertas dele caem na hora). */
export function revokeDevice(id: string): Promise<ApiResult<null>> {
  return request<unknown>({ method: "DELETE", path: `/api/devices/${encodeURIComponent(id)}` }).then((r) => (r.ok ? { ok: true, data: null } : r));
}

/** Revoga todos os dispositivos. */
export function revokeAllDevices(): Promise<ApiResult<number>> {
  return request<unknown>({ method: "DELETE", path: "/api/devices" }).then((r) =>
    mapped(r, (v) => (isRecord(v) && isCount(v.revoked) ? v.revoked : null)),
  );
}
