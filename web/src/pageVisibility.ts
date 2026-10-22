/*
 * Aba oculta (`document.hidden`): põe a classe `page-hidden` no <html> para o CSS pausar as
 * animações infinitas (animation-play-state: paused). Poupa bateria no celular.
 */
export const PAGE_HIDDEN_CLASS = "page-hidden";

/** Sincroniza a classe com o estado atual e a mantém por `visibilitychange`; devolve o desinstalador. */
export function installPageVisibility(doc: Document = document): () => void {
  const sync = (): void => {
    doc.documentElement.classList.toggle(PAGE_HIDDEN_CLASS, doc.hidden);
  };
  sync();
  doc.addEventListener("visibilitychange", sync);
  return () => {
    doc.removeEventListener("visibilitychange", sync);
    doc.documentElement.classList.remove(PAGE_HIDDEN_CLASS);
  };
}
