/*
 * Imagem ampliada sobre o `Dialog` do
 * desktop: foco preso, Esc e o foco de volta a quem abriu. A imagem cabe na largura; a que falha
 * vira o nome em texto. Usada pelas capturas do Revisor e pelas imagens enviadas ao Arquiteto.
 */
import { useId, useState } from "react";
import { Dialog } from "../../desktop/Dialog";
import { useT } from "../../i18n/react";

interface ImageLightboxProps {
  src: string;
  alt: string;
  title?: string;
  onClose: () => void;
}

export function ImageLightbox({ src, alt, title, onClose }: ImageLightboxProps) {
  const titleId = useId();
  const [failed, setFailed] = useState(false);
  const { t } = useT();
  const heading = title ?? alt;
  return (
    <Dialog titleId={titleId} title={<span className="nd-mono nd-break">{heading}</span>} onClose={onClose} width={960}>
      {failed ? (
        <p role="status" className="nd-muted nd-break">
          {heading}: {t("image.loadFailed")}
        </p>
      ) : (
        <img src={src} alt={alt} decoding="async" onError={() => setFailed(true)} className="nd-lightbox-img" />
      )}
    </Dialog>
  );
}
