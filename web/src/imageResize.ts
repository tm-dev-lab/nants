/*
 * Preparo de uma imagem anexada ao Arquiteto antes do upload (spec 082 T05): reduz para no máximo
 * `MAX_IMAGE_SIDE` px no maior lado e reencoda em JPEG (qualidade `JPEG_QUALITY`) via canvas.
 * PNG pequeno (≤ `MAX_IMAGE_SIDE` px e ≤ `SMALL_PNG_BYTES`) vai sem conversão (preserva texto nítido
 * de capturas de tela).
 */

import { JPEG_FLATTEN_BACKGROUND } from "./desktop/theme/tokens";

export const MAX_IMAGE_SIDE = 1600;
export const JPEG_QUALITY = 0.85;
export const SMALL_PNG_BYTES = 1024 * 1024;

type UploadImageType = "image/png" | "image/jpeg";

interface PreparedImage {
  blob: Blob;
  type: UploadImageType;
}

/** Dimensões finais: o maior lado vira no máximo `MAX_IMAGE_SIDE`, mantendo a proporção. */
export function targetSize(width: number, height: number): { width: number; height: number } {
  const side = Math.max(width, height);
  if (side <= MAX_IMAGE_SIDE || side <= 0) return { width, height };
  const scale = MAX_IMAGE_SIDE / side;
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

/** `true` quando a imagem vai como está: PNG pequeno nas duas medidas (lado e bytes). */
export function keepsOriginal(type: string, bytes: number, width: number, height: number): boolean {
  return type === "image/png" && bytes <= SMALL_PNG_BYTES && Math.max(width, height) <= MAX_IMAGE_SIDE;
}

interface Decoded {
  source: CanvasImageSource;
  width: number;
  height: number;
  release: () => void;
}

/** Decodifica a imagem (createImageBitmap, ou `<img>` com URL de objeto como alternativa). */
async function decode(file: Blob): Promise<Decoded> {
  if (typeof createImageBitmap === "function") {
    const bitmap = await createImageBitmap(file);
    return { source: bitmap, width: bitmap.width, height: bitmap.height, release: () => bitmap.close() };
  }
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.decoding = "async";
    img.src = url;
    await img.decode();
    return { source: img, width: img.naturalWidth, height: img.naturalHeight, release: () => URL.revokeObjectURL(url) };
  } catch (err: unknown) {
    URL.revokeObjectURL(url);
    throw err;
  }
}

/** Reduz e reencoda em JPEG; PNG pequeno vai sem conversão. Lança se a imagem não decodificar. */
export async function prepareImage(file: Blob): Promise<PreparedImage> {
  const decoded = await decode(file);
  try {
    if (keepsOriginal(file.type, file.size, decoded.width, decoded.height)) return { blob: file, type: "image/png" };
    const { width, height } = targetSize(decoded.width, decoded.height);
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d");
    if (ctx === null) throw new Error("canvas indisponível");
    // JPEG não tem transparência: fundo dos tokens no lugar do canal alfa.
    ctx.fillStyle = JPEG_FLATTEN_BACKGROUND;
    ctx.fillRect(0, 0, width, height);
    ctx.drawImage(decoded.source, 0, 0, width, height);
    const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/jpeg", JPEG_QUALITY));
    if (blob === null) throw new Error("falha ao converter a imagem");
    return { blob, type: "image/jpeg" };
  } finally {
    decoded.release();
  }
}
