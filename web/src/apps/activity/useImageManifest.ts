/*
 * Manifesto de imagens da spec (`.sdd/specs/<spec>/images.jsonl`) para a aba Histórico: é a
 * evidência de que a imagem foi de fato gravada (a trilha só registra a chamada). Lido pela API de
 * arquivos (`source=base`, somente leitura), relido a cada mudança do estado e na recarga ao vivo,
 * sem polling; com a janela fora de vista, não busca. Sem manifesto (404) ou com falha: nenhuma
 * imagem.
 */
import { useEffect, useState } from "react";
import { getProjectFile } from "../../api/client";
import { useAppStore } from "../../store";
import { imageManifestPath, parseImageManifest, type ManifestImage } from "./activityModel";

const NONE: readonly ManifestImage[] = [];

export function useImageManifest(projectId: string, specId: string | null, enabled: boolean): readonly ManifestImage[] {
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  const liveRevision = useAppStore((s) => s.liveRevisions[projectId] ?? 0);
  const key = `${projectId}\u0000${specId ?? ""}`;
  const [state, setState] = useState<{ key: string; images: readonly ManifestImage[] }>({ key: "", images: NONE });

  useEffect(() => {
    if (!enabled || specId === null) return undefined;
    let current = true;
    void getProjectFile(projectId, "base", imageManifestPath(specId))
      .then((r) => {
        if (!current) return;
        if (r.ok && r.data.status === "ok" && r.data.file.kind === "text") setState({ key, images: parseImageManifest(r.data.file.content) });
        else if (!r.ok && r.error.status === 404) setState({ key, images: NONE });
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [projectId, specId, enabled, key, revision, liveRevision]);

  return state.key === key ? state.images : NONE;
}
