/*
 * Eventos dos dispositivos (spec 112): `devices_changed` e `device_code_locked` chegam pelo fluxo
 * global (uma conexão por aba) e são repassados a quem mostra a lista e o código (Configurações >
 * Acesso pela rede). Não passam pelo store dos projetos.
 */
import type { ApiEvent } from "./types";

type DeviceEvent = Extract<ApiEvent, { event_type: "devices_changed" | "device_code_locked" }>;

const listeners = new Set<(event: DeviceEvent) => void>();

export function isDeviceEvent(event: ApiEvent): event is DeviceEvent {
  return event.event_type === "devices_changed" || event.event_type === "device_code_locked";
}

/** Assina os eventos dos dispositivos; devolve o cancelamento. */
export function subscribeDeviceEvents(listener: (event: DeviceEvent) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Entrega um evento aos assinantes. */
export function publishDeviceEvent(event: DeviceEvent): void {
  for (const listener of [...listeners]) listener(event);
}
