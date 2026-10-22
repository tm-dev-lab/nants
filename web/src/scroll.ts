/** Rola SOMENTE o contêiner até o alvo (nunca ancestrais de layout nem a página, ao contrário de scrollIntoView). */
export function scrollWithin(container: HTMLElement, target: HTMLElement, behavior: ScrollBehavior = "smooth"): void {
  const top = target.getBoundingClientRect().top - container.getBoundingClientRect().top + container.scrollTop;
  if (typeof container.scrollTo === "function") container.scrollTo({ top, behavior });
  else container.scrollTop = top;
}
