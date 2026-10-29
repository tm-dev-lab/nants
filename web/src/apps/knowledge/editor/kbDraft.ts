/*
 * Rascunho local do editor (spec 136 T11, critério 5): o texto ainda não salvo na base fica no
 * armazenamento do navegador e sobrevive a recarregar a página. Uma chave por projeto e documento
 * (`new` para o documento ainda sem id). Todo acesso em try/catch: sem armazenamento, o editor
 * segue sem rascunho.
 */
import { defaultStorage, type StorageLike } from "../../../desktop/browserStorage";

const PREFIX = "nants.kbDraft.";

interface KbDraft {
  title: string;
  content: string;
  /** Versão do texto em que o rascunho começou (`""` no documento novo). */
  base: string;
  /** Quando foi guardado (ms). */
  at: number;
}

function key(projectId: string, docId: string | null): string {
  return `${PREFIX}${projectId}.${docId ?? "new"}`;
}

export function readDraft(projectId: string, docId: string | null, storage: StorageLike | null = defaultStorage()): KbDraft | null {
  try {
    const raw = storage?.getItem(key(projectId, docId));
    if (raw === null || raw === undefined) return null;
    const value = JSON.parse(raw) as Partial<KbDraft>;
    if (typeof value.content !== "string" || typeof value.base !== "string") return null;
    return { title: typeof value.title === "string" ? value.title : "", content: value.content, base: value.base, at: typeof value.at === "number" ? value.at : 0 };
  } catch {
    return null;
  }
}

export function writeDraft(projectId: string, docId: string | null, draft: KbDraft, storage: StorageLike | null = defaultStorage()): void {
  try {
    storage?.setItem(key(projectId, docId), JSON.stringify(draft));
  } catch {
    // Cota cheia ou armazenamento bloqueado: o rascunho fica só na memória.
  }
}

export function clearDraft(projectId: string, docId: string | null, storage: StorageLike | null = defaultStorage()): void {
  try {
    storage?.removeItem(key(projectId, docId));
  } catch {
    // Sem armazenamento: nada a apagar.
  }
}
