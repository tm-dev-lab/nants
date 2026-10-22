/*
 * Linha do tempo dos pedidos de permissão na conversa: pendentes (sempre visíveis) e resolvidos
 * da sessão, na ordem de chegada, misturados com as mensagens do diálogo pelo instante (`at`).
 * Também o sinal de atenção: sobe um contador quando um pedido pendente NOVO chega, para a
 * conversa descer até ele.
 */
import { useEffect, useRef, useState } from "react";
import type { ChatMessage, PendingPermission, ResolvedPermission } from "./store";

/** Entrada da conversa: pedido pendente ou já resolvido nesta sessão. */
export type PermissionEntry = PendingPermission | ResolvedPermission;

/** Resolvido tem `resolvedAt` (o pendente não tem). */
export function isResolvedPermission(entry: PermissionEntry): entry is ResolvedPermission {
  return "resolvedAt" in entry;
}

/**
 * Pedidos a mostrar: TODOS os pendentes (nunca são escondidos por filtro) e os resolvidos que
 * `showResolved` aceitar, em ordem de chegada.
 */
export function permissionEntries(
  pending: readonly PendingPermission[],
  resolved: readonly ResolvedPermission[],
  showResolved: (entry: ResolvedPermission) => boolean = () => true,
): PermissionEntry[] {
  const entries: PermissionEntry[] = [...pending, ...resolved.filter(showResolved)];
  return entries.sort((a, b) => (a.at ?? 0) - (b.at ?? 0));
}

type TimelineItem<T> = { kind: "message"; item: T } | { kind: "permission"; entry: PermissionEntry };

/**
 * Mistura as mensagens (na ordem delas) com os pedidos (ordem de chegada): cada pedido entra antes
 * da primeira mensagem chegada depois dele. Mensagens mantêm a própria ordem.
 */
export function interleave<T extends Pick<ChatMessage, "at">>(
  messages: readonly T[],
  entries: readonly PermissionEntry[],
): TimelineItem<T>[] {
  const out: TimelineItem<T>[] = [];
  let next = 0;
  for (const message of messages) {
    while (next < entries.length && (entries[next]?.at ?? 0) < message.at) {
      const entry = entries[next];
      if (entry !== undefined) out.push({ kind: "permission", entry });
      next += 1;
    }
    out.push({ kind: "message", item: message });
  }
  for (; next < entries.length; next += 1) {
    const entry = entries[next];
    if (entry !== undefined) out.push({ kind: "permission", entry });
  }
  return out;
}

/**
 * Contador que sobe quando chega um pedido pendente NOVO (não na montagem, nem quando um pedido
 * sai da fila). A conversa usa a mudança como `pinKey`: desce até o pedido mesmo se o usuário
 * tinha rolado para cima.
 */
export function usePendingPin(pending: readonly PendingPermission[]): number {
  const [pin, setPin] = useState(0);
  const seenRef = useRef<Set<string> | null>(null);
  useEffect(() => {
    const ids = pending.map((p) => p.requestId);
    const previous = seenRef.current;
    seenRef.current = new Set(ids);
    if (previous === null) return;
    if (ids.some((id) => !previous.has(id))) setPin((count) => count + 1);
  }, [pending]);
  return pin;
}

/** Chave de mudança da fila (ids pendentes): a conversa reavalia a rolagem quando ela muda. */
export function pendingKey(pending: readonly PendingPermission[]): string {
  return pending.map((p) => p.requestId).join("|");
}
