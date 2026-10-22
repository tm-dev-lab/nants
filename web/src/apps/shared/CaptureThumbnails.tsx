/*
 * Miniaturas das capturas avaliadas pelo Revisor: cada uma é um botão que abre a imagem ampliada. Até
 * `MAX_CAPTURE_THUMBNAILS`; altura fixa; `loading="lazy"`; imagem que falha vira o nome em texto.
 */
import { useState } from "react";
import { useT } from "../../i18n/react";
import { ImageLightbox } from "./ImageLightbox";

const MAX_CAPTURE_THUMBNAILS = 8;

/** URL de `GET /api/projects/:id/specs/:spec/captures/:name`. */
export function captureUrl(projectId: string, specId: string, name: string): string {
  const enc = encodeURIComponent;
  return `/api/projects/${enc(projectId)}/specs/${enc(specId)}/captures/${enc(name)}`;
}

function Thumbnail({ name, url, onOpen }: { name: string; url: string; onOpen: (name: string) => void }) {
  const [failed, setFailed] = useState(false);
  const { t } = useT();
  if (failed) {
    return (
      <span data-testid="capture-failed" className="nd-thumb-failed nd-mono">
        {name}
      </span>
    );
  }
  return (
    <button type="button" title={name} aria-label={t("captures.zoom", { name })} onClick={() => onOpen(name)} className="nd-thumb-btn">
      <img src={url} alt={name} loading="lazy" decoding="async" onError={() => setFailed(true)} className="nd-thumb-img" />
    </button>
  );
}

export function CaptureThumbnails({ projectId, specId, names }: { projectId: string; specId: string; names: readonly string[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const { t } = useT();
  const shown = names.slice(0, MAX_CAPTURE_THUMBNAILS);
  if (shown.length === 0) return null;
  return (
    <section className="nd-captures">
      <h4 className="nd-section-label">{t("captures.label")}</h4>
      <ul aria-label={t("captures.label")} className="nd-thumb-list">
        {shown.map((name) => (
          <li key={name}>
            <Thumbnail name={name} url={captureUrl(projectId, specId, name)} onOpen={setOpen} />
          </li>
        ))}
      </ul>
      {open !== null ? <ImageLightbox src={captureUrl(projectId, specId, open)} alt={open} onClose={() => setOpen(null)} /> : null}
    </section>
  );
}
