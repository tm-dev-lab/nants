/*
 * Quem pode ser editado no Nants (spec 136 T10 e T11): o documento escrito e o enviado em markdown
 * ou texto, até o teto do texto escrito. Acima dele, o servidor recusa a gravação
 * (`kb_written_too_large`), então o editor nem abre: a prévia mostra "Só leitura" com o motivo.
 */
import type { KbDoc } from "../../../api/kb";

/** Teto do texto editável, em MiB (`kb::MAX_WRITTEN_BYTES`). */
export const KB_MAX_WRITTEN_MIB = 2;

type Editability = "editable" | "too_large" | "not_editable";

export function editability(doc: KbDoc): Editability {
  const textual = (doc.kind === "markdown" || doc.kind === "text") && (doc.source === "file" || doc.source === "written");
  if (!textual) return "not_editable";
  return doc.size > KB_MAX_WRITTEN_MIB * 1024 * 1024 ? "too_large" : "editable";
}
