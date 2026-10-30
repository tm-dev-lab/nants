/*
 * Símbolo da marca (spec 093, decisão 8). Na barra de cima ele é o botão N (spec 137, `NMenu.tsx`):
 * o nome e o slogan saíram da barra e ficam no topo do menu, o slogan logo depois do nome.
 */
import { BRAND } from "../theme/tokens";

/** Símbolo "N" (três peças nas cores da marca). Decorativo: o nome vem ao lado. */
export function NantsSymbol({ size = 20 }: { size?: number }) {
  return (
    <svg aria-hidden="true" focusable="false" viewBox="0 0 285 330" height={size} width={(size * 285) / 330} data-brand-symbol="">
      <path fill={BRAND.violet} d="M0 102 Q0 90 10 98 L75 140 Q89 149 89 167 V310 Q89 324 77 316 L13 276 Q0 268 0 253 Z" />
      <path
        fill={BRAND.cyan}
        d="M15 78 L67 48 Q81 41 94 49 L179 102 Q193 111 193 126 V187 Q193 201 205 209 L277 254 L227 286 Q215 294 202 286 L124 237 Q111 229 111 214 V153 Q111 139 98 131 Z"
      />
      <path
        fill={BRAND.amber}
        d="M185 9 Q185 -5 199 3 L271 48 Q285 57 285 72 V237 Q285 251 273 243 L229 215 Q215 207 215 193 V130 Q215 116 203 108 L196 104 Q185 98 185 85 Z"
      />
    </svg>
  );
}
