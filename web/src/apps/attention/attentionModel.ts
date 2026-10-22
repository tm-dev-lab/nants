/*
 * "Precisa de você" (spec 093, decisão 10), regras puras:
 * - itens que EXIGEM ação, por projeto, a partir do `/api/attention` (todos os projetos):
 *   permissões (inclui a confirmação da correção final), perguntas do Arquiteto, plano para
 *   aprovar, execução pausada (com as opções de escalada) ou parada; e o setup incompleto;
 * - informativos ("Revisor aprovou", "Imagem gerada") nunca entram na contagem: ficam no
 *   histórico da janela Atividade (T07).
 */
import type { Attention, AttentionProject, PausedOrStalled } from "../../desktop/api";

/** Tipos de item que exigem ação (contam no sino, nos selos e no título da aba). */
type ActionKind = "permission" | "choices" | "plan" | "paused" | "stalled" | "setup";
/** Tipos informativos (aba Histórico da Atividade, T07): fora da contagem. */
export type InfoKind = "reviewer_approved" | "image_generated";
type AttentionKind = ActionKind | InfoKind;

export const ACTION_KINDS: readonly ActionKind[] = ["permission", "choices", "plan", "paused", "stalled", "setup"];
export const INFO_KINDS: readonly InfoKind[] = ["reviewer_approved", "image_generated"];

/** A regra de contagem: só o que exige ação conta. */
export function countsTowardAttention(kind: AttentionKind): boolean {
  return (ACTION_KINDS as readonly string[]).includes(kind);
}

export type AttentionItem =
  | { kind: "permission"; key: string; requestId: string; agent: string; permissionKind: string; taskId: string | null; alwaysAllowed: boolean }
  | { kind: "choices"; key: string }
  | { kind: "plan"; key: string }
  | { kind: "paused"; key: string; specId: string; taskId: string; options: NonNullable<Extract<PausedOrStalled, { kind: "paused" }>["escalation_options"]> }
  | { kind: "stalled"; key: string; specId: string };

export interface AttentionGroup {
  projectId: string;
  name: string;
  items: AttentionItem[];
}

/** Itens de um projeto, na ordem do painel (permissões primeiro, depois o resto). */
export function projectItems(project: AttentionProject): AttentionItem[] {
  const out: AttentionItem[] = project.pending_permissions.map((p) => ({
    kind: "permission",
    key: `permission:${p.request_id}`,
    requestId: p.request_id,
    agent: p.agent,
    permissionKind: p.kind,
    taskId: p.task_id ?? null,
    alwaysAllowed: p.always_allowed,
  }));
  if (project.pending_choices) out.push({ kind: "choices", key: "choices" });
  if (project.plan_awaiting_approval) out.push({ kind: "plan", key: "plan" });
  const paused = project.paused_or_stalled;
  if (paused !== null) {
    out.push(
      paused.kind === "paused"
        ? { kind: "paused", key: `paused:${paused.spec_id}:${paused.task_id}`, specId: paused.spec_id, taskId: paused.task_id, options: paused.escalation_options ?? [] }
        : { kind: "stalled", key: `stalled:${paused.spec_id}`, specId: paused.spec_id },
    );
  }
  return out;
}

/**
 * Grupos por projeto (só os que têm item), com o espaço ativo primeiro e os outros na ordem do
 * servidor.
 */
export function attentionGroups(attention: Attention | null, activeId: string | null): AttentionGroup[] {
  if (attention === null) return [];
  const groups = attention.projects
    .map((p) => ({ projectId: p.project_id, name: p.name, items: projectItems(p) }))
    .filter((g) => g.items.length > 0);
  const active = groups.filter((g) => g.projectId === activeId);
  return [...active, ...groups.filter((g) => g.projectId !== activeId)];
}

/** Contagem total (igual à do sino): itens de ação de todos os projetos mais o setup. */
export function attentionTotal(attention: Attention | null): number {
  if (attention === null) return 0;
  const items = attention.projects.reduce((sum, p) => sum + projectItems(p).filter((i) => countsTowardAttention(i.kind)).length, 0);
  return items + (attention.setup_incomplete ? 1 : 0);
}

/** Entrada do projeto no `/api/attention` (identidade estável entre respostas iguais). */
export function attentionFor(attention: Attention | null, projectId: string | null): AttentionProject | null {
  if (attention === null || projectId === null) return null;
  return attention.projects.find((p) => p.project_id === projectId) ?? null;
}
