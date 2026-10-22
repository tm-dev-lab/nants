/*
 * Varredura de português que sobrou na tela (testes em en da spec 102): textos visíveis e
 * acessíveis (`aria-label`, `title`, `placeholder`, `alt`) de um elemento que contêm uma frase
 * pt-BR do dicionário (valores que mudam em en, cortados nos parâmetros, com 4 letras ou mais), uma
 * frase pt-BR comum escrita sem acento ou uma letra que o inglês não usa. Usado só pelos testes
 * (inclusive pela auditoria do código-fonte, `ptAudit.test.ts`, com `looksPortuguese`).
 */
import { en } from "./en";
import { ptBR, type MessageKey } from "./ptBR";

/**
 * Frases que a interface mostra em pt-BR sem acento e que a derivação do dicionário pode não
 * cobrir sozinha (mensagens fixas do servidor repassadas pelo cliente).
 */
const EXTRA_PHRASES: readonly string[] = ["falha interna", "projeto não encontrado"];

const PT_PHRASES: readonly string[] = (() => {
  const out = new Set<string>(EXTRA_PHRASES);
  for (const key of Object.keys(ptBR) as MessageKey[]) {
    if (ptBR[key] === en[key]) continue;
    for (const part of ptBR[key].split(/\{\w+\}/)) {
      const phrase = part.trim().replace(/^[:,.;]\s*/, "").replace(/[:,.;]$/, "").trim();
      if (phrase.length >= 4) out.add(phrase);
    }
  }
  return [...out];
})();

/** Cada frase como palavra inteira ("Conversa" não casa dentro de "Conversation"). */
const PT_PHRASE_PATTERNS: readonly RegExp[] = PT_PHRASES.map(
  (phrase) => new RegExp(`(?<!\\p{L})${phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?!\\p{L})`, "u"),
);

/** Letras do português que o inglês não usa. */
const PT_LETTERS = /[ãõçáéíóúâêôàÃÕÇÁÉÍÓÚÂÊÔÀ]/;

/** Palavras comuns do português escritas sem acento (nenhuma é palavra do inglês). */
const UNACCENTED: ReadonlySet<string> = new Set([
  "nao", "voce", "voces", "tambem", "entao", "execucao", "configuracao", "conexao", "sessao",
  "opcoes", "acoes", "pendencia", "pendencias", "informacao", "descricao", "atencao", "criterios",
]);

const WORD_RE = /\p{L}+/gu;

function wordsOf(text: string): string[] {
  return text.toLowerCase().match(WORD_RE) ?? [];
}

/** Palavras do pt-BR do dicionário (4 letras ou mais) que nenhum texto do en usa. */
const PT_WORDS: ReadonlySet<string> = (() => {
  const english = new Set(Object.values(en).flatMap(wordsOf));
  return new Set(Object.values(ptBR).flatMap(wordsOf).filter((w) => w.length >= 4 && !english.has(w)));
})();

/** Palavras curtas que só o português usa como ligação ("de", "para", "com"...). */
const PT_LINKS: ReadonlySet<string> = new Set(["de", "da", "das", "dos", "para", "com", "uma", "que", "em", "na", "nas", "nos", "ao", "aos", "pelo", "pela", "sem"]);

/**
 * O texto parece pt-BR: letra do português, palavra do pt-BR do dicionário que o en não usa,
 * palavra comum sem acento, ou frase (3 palavras ou mais) com uma palavra de ligação do português.
 */
export function looksPortuguese(text: string): boolean {
  if (PT_LETTERS.test(text)) return true;
  const words = wordsOf(text).filter((w) => w.length >= 2);
  if (words.some((w) => PT_WORDS.has(w) || UNACCENTED.has(w))) return true;
  return words.length >= 3 && words.some((w) => PT_LINKS.has(w));
}

function textsOf(root: Element): string[] {
  const out: string[] = [];
  const walker = root.ownerDocument.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const text = node.textContent?.trim() ?? "";
    if (text !== "") out.push(text);
  }
  for (const el of [root, ...root.querySelectorAll("*")]) {
    for (const attr of ["aria-label", "title", "placeholder", "alt"]) {
      const value = el.getAttribute(attr);
      if (value !== null && value.trim() !== "") out.push(value.trim());
    }
  }
  return out;
}

/** Textos de `root` com frase pt-BR do dicionário ou letra do português (em en, deve ser vazio). */
export function portugueseLeftovers(root: Element): string[] {
  return textsOf(root).filter(
    (text) => PT_LETTERS.test(text) || wordsOf(text).some((w) => UNACCENTED.has(w)) || PT_PHRASE_PATTERNS.some((pattern) => pattern.test(text)),
  );
}
