/*
 * Idioma enviado ao servidor em `X-Nants-Lang` (spec 102, decisão 3). Fica num módulo sem
 * dependências para o i18n poder defini-lo sem importar o cliente da API: o i18n e o cliente não
 * formam ciclo, e a ordem de carga dos módulos não importa (spec 134 T06).
 */
let requestLanguage: "pt-BR" | "en" | null = null;

/** Define o idioma enviado em `X-Nants-Lang` (chamado pelo i18n a cada troca). */
export function setRequestLanguage(lang: "pt-BR" | "en" | null): void {
  requestLanguage = lang;
}

/** Idioma enviado hoje em `X-Nants-Lang`. */
export function getRequestLanguage(): "pt-BR" | "en" | null {
  return requestLanguage;
}
