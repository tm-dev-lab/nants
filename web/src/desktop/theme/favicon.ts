/*
 * Favicon de atenção pelos tokens (spec 093, decisão 13): a marca sobre o fundo e um ponto
 * âmbar no canto. Gerado como data URL (sem rede; `img-src data:` na CSP). O uso (título e
 * favicon com a contagem do "Precisa de você") vem na T06.
 */
import { FAVICON_TOKENS } from "./tokens";

/** Caminhos do símbolo "N" (viewBox 285×330), iguais aos de `/brand/favicon.svg`. */
const SYMBOL_PATHS = {
  violet: "M0 102 Q0 90 10 98 L75 140 Q89 149 89 167 V310 Q89 324 77 316 L13 276 Q0 268 0 253 Z",
  cyan: "M15 78 L67 48 Q81 41 94 49 L179 102 Q193 111 193 126 V187 Q193 201 205 209 L277 254 L227 286 Q215 294 202 286 L124 237 Q111 229 111 214 V153 Q111 139 98 131 Z",
  amber:
    "M185 9 Q185 -5 199 3 L271 48 Q285 57 285 72 V237 Q285 251 273 243 L229 215 Q215 207 215 193 V130 Q215 116 203 108 L196 104 Q185 98 185 85 Z",
} as const;

/** SVG do favicon de atenção. */
export function attentionFaviconSvg(): string {
  const { bg, dot, brand } = FAVICON_TOKENS;
  return (
    '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">' +
    `<rect width="64" height="64" rx="14" fill="${bg}"/>` +
    '<g transform="translate(14.1875 12) scale(0.125)">' +
    `<path fill="${brand.violet}" d="${SYMBOL_PATHS.violet}"/>` +
    `<path fill="${brand.cyan}" d="${SYMBOL_PATHS.cyan}"/>` +
    `<path fill="${brand.amber}" d="${SYMBOL_PATHS.amber}"/>` +
    "</g>" +
    `<circle cx="51" cy="13" r="12" fill="${dot}" stroke="${bg}" stroke-width="3"/>` +
    "</svg>"
  );
}

/** Favicon de atenção como data URL. */
export function attentionFavicon(): string {
  return `data:image/svg+xml,${encodeURIComponent(attentionFaviconSvg())}`;
}
