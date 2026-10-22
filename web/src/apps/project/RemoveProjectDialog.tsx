/*
 * Confirmação de "Remover do Nants". Deixa explícito que
 * só o workspace sai do Nants e que a pasta (com tudo dentro dela) permanece no disco; com o caminho
 * desconhecido (vazio), o texto omite a pasta. Cancelar é o
 * foco inicial e não envia nada. Projeto em Docker: a remoção derruba os ambientes -test e -run; a
 * caixa (desmarcada por padrão) confirma apagar também os volumes de dados da execução.
 * Envio único: com a remoção em voo, o botão mostra "Removendo" e não aceita novo clique.
 * Sucesso: `removeSpaceFromView` avisa o shell, que fecha o espaço e mostra o aviso.
 */
import { useEffect, useId, useRef, useState } from "react";
import { removeProject, type ApiFailure } from "../../api/client";
import type { ProjectEntry } from "../../api/types";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { beginLocalRemoval, endLocalRemoval, removalErrorMessage, removeSpaceFromView } from "./projectRemoval";

interface Props {
  project: Pick<ProjectEntry, "id" | "name" | "path">;
  onClose: () => void;
}

export function RemoveProjectDialog({ project, onClose }: Props) {
  const titleId = useId();
  const descId = useId();
  const errorId = useId();
  const [busy, setBusy] = useState(false);
  const { t } = useT();
  // Falha guardada como veio: o texto sai no idioma ativo na hora de exibir.
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const error = failure === null ? null : removalErrorMessage(failure);
  const [removeVolumes, setRemoveVolumes] = useState(false);
  // Guarda síncrona contra clique duplo (o estado ainda não foi redesenhado no segundo clique).
  const sending = useRef(false);
  const mounted = useRef(true);
  const cancelRef = useRef<HTMLButtonElement>(null);
  // Caminho desconhecido (vazio): o texto fala dos "arquivos do projeto", sem pasta vazia.
  const knownPath = project.path.trim() !== "";

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  function close(): void {
    if (!sending.current) onClose();
  }

  async function confirm(): Promise<void> {
    if (sending.current) return;
    sending.current = true;
    setBusy(true);
    setFailure(null);
    beginLocalRemoval(project.id);
    const result = await removeProject(project.id, { removeVolumes });
    endLocalRemoval(project.id);
    sending.current = false;
    if (result.ok) {
      // Sucesso (inclusive 404: já removido). O shell fecha o espaço (e este diálogo com ele).
      if (mounted.current) onClose();
      removeSpaceFromView(project.id, { remote: false });
      return;
    }
    if (!mounted.current) return;
    setBusy(false);
    setFailure(result.error);
  }

  return (
    <Dialog titleId={titleId} title={t("project.removeDialog.title")} describedById={descId} onClose={close} width={480} initialFocus={cancelRef} alert>
      <div className="nd-stack-sm">
        <p className="nd-strong">{project.name}</p>
        {knownPath ? <p className="nd-muted nd-mono nd-break">{project.path}</p> : null}
      </div>
      <div id={descId} className="nd-dialog-text">
        <p>
          {knownPath
            ? t("project.removeDialog.withPath", { path: project.path })
            : t("project.removeDialog.noPath")}
        </p>
        <p>{t("project.removeDialog.readd")}</p>
      </div>
      <label className="nd-check nd-remove-volumes">
        <input type="checkbox" checked={removeVolumes} disabled={busy} onChange={(e) => setRemoveVolumes(e.target.checked)} />
        <span>
          {t("project.removeDialog.volumes")}
          <span className="nd-field-hint nd-block">{t("project.removeDialog.volumesHint")}</span>
        </span>
      </label>
      {error !== null ? (
        <p role="alert" id={errorId} className="nd-dialog-error">
          {error}
        </p>
      ) : null}
      <div className="nd-dialog-actions">
        <button ref={cancelRef} type="button" className="nd-btn" onClick={close} disabled={busy}>
          {t("common.cancel")}
        </button>
        <button
          type="button"
          className="nd-btn"
          data-variant="danger"
          onClick={() => void confirm()}
          disabled={busy}
          aria-busy={busy}
          aria-describedby={error !== null ? errorId : undefined}
        >
          <Icon name="trash" size={14} />
          {busy ? t("project.removeDialog.removing") : t("project.remove")}
        </button>
      </div>
    </Dialog>
  );
}
