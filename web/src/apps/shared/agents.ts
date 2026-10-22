/*
 * Papéis da conversa (Arquiteto, Desenvolvedor, Revisor, Sistema, Testes, Você): rótulo e tom.
 * Os rótulos vêm do dicionário no idioma ativo (`agentRoleLabel`); a cor é só o tom dos tokens
 * (`data-tone`), nunca uma classe de paleta.
 */
import { agentRoleLabel } from "../../copy";
import type { Tone } from "../../desktop/theme/tokens";

const AGENT_TONE: Readonly<Record<string, Tone>> = {
  architect: "violet",
  developer: "cyan",
  reviewer: "amber",
  system: "muted",
  tests: "green",
  user: "muted",
};

/** Tom do papel (desconhecido: neutro). */
export function agentTone(kind: string): Tone {
  return AGENT_TONE[kind] ?? "muted";
}

/** Nome exibido do papel (desconhecido: "Agente"). */
export function agentLabel(kind: string): string {
  return agentRoleLabel(kind);
}

/** Papel conhecido (tem avatar próprio). */
export function isKnownAgent(kind: string): boolean {
  return kind in AGENT_TONE;
}
