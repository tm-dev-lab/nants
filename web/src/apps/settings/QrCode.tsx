/*
 * QR code do endereço de entrada (spec 112, T02), gerado no cliente, sem rede.
 *
 * Biblioteca: `uqr` (MIT, sem dependências, só ESM), que codifica o texto na matriz de módulos; o
 * desenho é nosso: um `<svg>` com um `<path>` só, escuro sobre fundo claro nos dois temas (os
 * leitores de QR esperam esse contraste), com a zona de silêncio de 4 módulos.
 */
import { useMemo } from "react";
import { encode } from "uqr";
import { QR_COLORS } from "../../desktop/theme/tokens";
import { qrPath } from "./devicesModel";

const QUIET_ZONE = 4;

export function QrCode({ value, label, size = 168 }: { value: string; label: string; size?: number }) {
  const qr = useMemo(() => {
    const result = encode(value, { ecc: "M", border: QUIET_ZONE });
    return { size: result.size, path: qrPath(result.data) };
  }, [value]);
  return (
    <svg
      className="nd-qr"
      role="img"
      aria-label={label}
      viewBox={`0 0 ${qr.size} ${qr.size}`}
      width={size}
      height={size}
      shapeRendering="crispEdges"
      data-qr={value}
    >
      <rect width={qr.size} height={qr.size} fill={QR_COLORS.light} />
      <path d={qr.path} fill={QR_COLORS.dark} />
    </svg>
  );
}
