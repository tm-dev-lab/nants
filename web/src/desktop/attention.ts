/*
 * Aviso fora da aba (spec 093, decisão 10): com algo que exige ação (a contagem do "Precisa de
 * você", de todos os projetos), o título vira "(N) Nants · aguardando você" e o favicon ganha o
 * ponto âmbar (desenhado pelos tokens, `theme/favicon.ts`); com zero, voltam ao original.
 * Os informativos não contam (a contagem é a do `/api/attention`).
 */
import { useEffect } from "react";
import { attentionTitle } from "../copy";
import { attentionFavicon } from "./theme/favicon";

interface IconAttrs {
  href: string;
  type: string | null;
}

/** Título e ícones originais, guardados na primeira troca e restaurados com zero. */
let original: { title: string; icons: Map<HTMLLinkElement, IconAttrs> } | null = null;

function iconLinks(doc: Document): HTMLLinkElement[] {
  return Array.from(doc.querySelectorAll<HTMLLinkElement>('link[rel~="icon"]'));
}

/** Aplica o aviso para `count` itens; com zero, devolve título e ícones originais. Idempotente. */
export function applyAttention(count: number, doc: Document = document): void {
  if (count > 0) {
    if (original === null) {
      const icons = new Map<HTMLLinkElement, IconAttrs>();
      for (const link of iconLinks(doc)) icons.set(link, { href: link.getAttribute("href") ?? "", type: link.getAttribute("type") });
      original = { title: doc.title, icons };
    }
    doc.title = attentionTitle(count);
    const href = attentionFavicon();
    for (const link of original.icons.keys()) {
      link.setAttribute("href", href);
      link.setAttribute("type", "image/svg+xml");
    }
    return;
  }
  if (original === null) return;
  doc.title = original.title;
  for (const [link, attrs] of original.icons) {
    link.setAttribute("href", attrs.href);
    if (attrs.type === null) link.removeAttribute("type");
    else link.setAttribute("type", attrs.type);
  }
  original = null;
}

/** Montado uma vez pelo shell: acompanha a contagem e restaura tudo ao desmontar. */
export function useAttentionSignal(count: number): void {
  useEffect(() => {
    applyAttention(count);
  }, [count]);
  useEffect(() => () => applyAttention(0), []);
}
