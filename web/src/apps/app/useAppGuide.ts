/*
 * Conteúdo da janela de execução (spec 130 T04): o "Como usar" do relatório final, as provas e o
 * aviso do que não roda no sandbox, de `GET .../app/guide/:spec_id`. Uma leitura por projeto e
 * especificação (guardada no módulo: App, Console e a troca entre eles não repetem a consulta); só
 * com a janela visível.
 */
import { useEffect, useState } from "react";
import { getAppGuide, type AppGuide } from "../../api/client";

type GuideState = { status: "loading" } | { status: "ok"; guide: AppGuide } | { status: "error" };

const cache = new Map<string, AppGuide>();

/** Testes: esquece as leituras guardadas. */
export function clearGuideCache(): void {
  cache.clear();
}

export function useAppGuide(projectId: string, specId: string | null, visible: boolean): GuideState | null {
  const key = specId === null ? null : `${projectId}:${specId}`;
  const cached = key === null ? undefined : cache.get(key);
  const [state, setState] = useState<GuideState>(cached === undefined ? { status: "loading" } : { status: "ok", guide: cached });
  useEffect(() => {
    if (key === null || specId === null || !visible) return undefined;
    const hit = cache.get(key);
    if (hit !== undefined) {
      setState({ status: "ok", guide: hit });
      return undefined;
    }
    let live = true;
    setState({ status: "loading" });
    void getAppGuide(projectId, specId).then((r) => {
      if (!live) return;
      if (r.ok) {
        cache.set(key, r.data);
        setState({ status: "ok", guide: r.data });
      } else {
        setState({ status: "error" });
      }
    });
    return () => {
      live = false;
    };
  }, [key, projectId, specId, visible]);
  return key === null ? null : state;
}
