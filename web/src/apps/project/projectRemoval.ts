/*
 * Remoção de projeto no desktop (spec 093, T08): avisa o shell, que fecha o espaço
 * removido, mostra o aviso e vai para outro espaço (ou para as boas-vindas).
 * - `removeSpaceFromView` é o ponto único que tira o projeto do cliente (store) e avisa quem
 *   assina (`subscribeSpaceRemovals`). Idempotente: a segunda chamada para o mesmo id não avisa.
 * - Usado pela ação local (janela Projeto) e pelo evento SSE `project_removed` (outra aba).
 * - Mensagens são fixas (catálogo deste módulo); nunca o corpo cru do servidor.
 * Só lógica: arquivos no disco ficam intactos ("Remover do Nants" não apaga a pasta).
 */
import type { ApiFailure } from "../../api/client";
import type { ProjectEntry } from "../../api/types";
import { t } from "../../i18n";
import { ptBR } from "../../i18n/ptBR";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore } from "../../store";

/** Aviso fixo quando o espaço aberto some por evento de outro cliente (reserva pt-BR; a tela usa `removedNotice`). */
export const REMOVED_NOTICE = ptBR["project.removed"];

/** Aviso de remoção por outro cliente, no idioma ativo. */
export function removedNotice(): string {
  return t("project.removed");
}

const RUN_ACTIVE_MESSAGE = ptBR["serverMessage.runActive"];
const CHAT_ACTIVE_MESSAGE = ptBR["serverMessage.chatActive"];

/** Aviso da remoção feita nesta aba (a pasta continua no disco), no idioma ativo. */
export function localRemovalNotice(name: string): string {
  return t("project.removedLocal", { name });
}

export interface SpaceRemoval {
  projectId: string;
  /** Nome exibido antes da remoção (o id, se não estava na config). */
  name: string;
  /** Veio de outro cliente (mostra `REMOVED_NOTICE`); `false` = remoção local desta aba. */
  remote: boolean;
}

type Listener = (removal: SpaceRemoval) => void;

const listeners = new Set<Listener>();
/** Projetos com remoção local em voo (DELETE enviado, resposta ainda não chegou). */
const inflight = new Set<string>();

/** Assina as remoções; devolve o cancelamento. */
export function subscribeSpaceRemovals(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Marca o início de uma remoção local (antes do DELETE). */
export function beginLocalRemoval(projectId: string): void {
  inflight.add(projectId);
}

/** Marca o fim da remoção local (sucesso ou falha). */
export function endLocalRemoval(projectId: string): void {
  inflight.delete(projectId);
}

/**
 * Tira o projeto do cliente e avisa os assinantes. Chamar de novo para o mesmo id (o evento SSE
 * da própria remoção, por exemplo) não tem efeito adicional.
 */
export function removeSpaceFromView(projectId: string, opts: { remote: boolean }): void {
  const state = appStore.getState();
  if (state.removedProjectIds[projectId] === true) return;
  const name = state.config?.projects.find((p) => p.id === projectId)?.name ?? projectId;
  appStore.forgetProject(projectId);
  const removal: SpaceRemoval = { projectId, name, remote: opts.remote };
  for (const listener of [...listeners]) listener(removal);
}

/** Evento SSE `project_removed`. Sem aviso de "outro cliente" quando a remoção é desta aba (em voo). */
export function handleSpaceRemovedEvent(projectId: string): void {
  removeSpaceFromView(projectId, { remote: !inflight.has(projectId) });
}

/** Para onde o desktop vai depois de o espaço sumir: o primeiro outro projeto, ou as boas-vindas. */
export function nextSpaceAfterRemoval(projects: readonly ProjectEntry[], removedId: string): string | null {
  return projects.find((p) => p.id !== removedId)?.id ?? null;
}

/**
 * Mensagem fixa para a falha de `removeProject`. 409 de execução ou de chat ganha texto
 * explicativo; os demais seguem o catálogo do cliente (já fixo).
 */
export function removalErrorMessage(error: ApiFailure): string {
  if (error.status === 409 && error.message === RUN_ACTIVE_MESSAGE) return t("project.removeRunActive");
  if (error.status === 409 && error.message === CHAT_ACTIVE_MESSAGE) return t("project.removeChatActive");
  return apiFailureText(error);
}
