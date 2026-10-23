/*
 * Relógio da linha do tempo da conversa (114/T01). Tudo o que entra ao vivo na conversa
 * (mensagens, pedidos de permissão e suas resoluções) é ordenado por esta hora, que nunca recua.
 *
 * Por quê: o relógio de parede pode voltar alguns segundos (no WSL, cerca de 2 s a cada ~32 s;
 * o journald registra "Time jumped backwards"). Com `Date.now()` cru, um item novo ganhava hora
 * anterior à do item de antes e ia para antes dele na conversa. Fora de um recuo, é a hora real.
 */
let last = 0;

/** Hora atual em ms, sem nunca ficar abaixo da última devolvida. */
export function liveNow(): number {
  last = Math.max(last, Date.now());
  return last;
}
