/*
 * Rascunho que o Nanty deixou na caixa do Arquiteto (spec 140 T15, C-15). O clique no cartão "levar ao
 * Arquiteto" grava o texto no servidor, e nada vai ao Arquiteto: a janela do projeto lê o rascunho
 * (`GET /chat/draft`), põe o texto na caixa e a pessoa revê, envia ou descarta. A janela relê pelo
 * `nanty_changed` com `part: architect_draft` e nas ressincronizações do fluxo.
 */
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { discardArchitectDraft, getArchitectDraft, type ArchitectDraft } from "../../api/client";

let version = 0;
const listeners = new Set<() => void>();

/** O rascunho de algum projeto mudou: cada janela relê o seu. */
export function bumpArchitectDraft(): void {
  version += 1;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

interface ArchitectDraftState {
  draft: ArchitectDraft | null;
  /** A primeira leitura deste projeto chegou: antes dela, `null` não quer dizer "sem rascunho". */
  loaded: boolean;
  /** Descarta no servidor e some daqui. */
  discard: () => Promise<void>;
  /** A pessoa enviou: o servidor consumiu o rascunho no turno. */
  sent: () => void;
}

export function useArchitectDraft(projectId: string): ArchitectDraftState {
  const seen = useSyncExternalStore(subscribe, () => version, () => version);
  const [state, setState] = useState<{ projectId: string; draft: ArchitectDraft | null; loaded: boolean }>({ projectId, draft: null, loaded: false });
  useEffect(() => {
    let alive = true;
    void getArchitectDraft(projectId).then((r) => {
      if (alive && r.ok) setState({ projectId, draft: r.data, loaded: true });
    });
    return () => {
      alive = false;
    };
  }, [projectId, seen]);
  const draft = state.projectId === projectId ? state.draft : null;
  const loaded = state.projectId === projectId && state.loaded;
  const discard = useCallback(async (): Promise<void> => {
    if (draft === null) return;
    setState({ projectId, draft: null, loaded: true });
    await discardArchitectDraft(projectId, draft.id);
  }, [projectId, draft]);
  const sent = useCallback(() => setState({ projectId, draft: null, loaded: true }), [projectId]);
  return { draft, loaded, discard, sent };
}
