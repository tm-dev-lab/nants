/*
 * Imagens do usuário na Conversa:
 * - `useChatAttachments`: anexos do campo (até 4), reduzidos no navegador e enviados ao anexar;
 *   "Enviar" aguarda os uploads (`waitAll`);
 * - `AttachmentPreview`: miniaturas acima do campo, com remover, "enviando" e erro; amplia;
 * - `ChatImageThumbs`: miniaturas na mensagem do usuário, que ampliam.
 * As imagens chegam por colar, arrastar e soltar ou pelo botão (o arrasto é tratado na Conversa).
 */
import { useCallback, useRef, useState } from "react";
import { uploadChatAttachment } from "../../api/client";
import { chatAttachmentUrl, MAX_CHAT_ATTACHMENTS } from "../../chatImages";
import { Icon } from "../../desktop/Icon";
import { t, translate } from "../../i18n";
import { useT } from "../../i18n/react";
import { ptBR } from "../../i18n/ptBR";
import { prepareImage } from "../../imageResize";
import { apiFailureText } from "../../i18n/serverCodes";
import { ImageLightbox } from "../shared/ImageLightbox";

/*
 * Reservas pt-BR dos textos dos anexos (testes em pt-BR); a tela usa as chaves `attachments.*` no
 * idioma ativo.
 */
export const ATTACH_LABEL = ptBR["attachments.attach"];
export const ATTACHMENTS_LIMIT_NOTICE = translate("pt-BR", "attachments.limit", { max: MAX_CHAT_ATTACHMENTS });
export const ATTACHMENTS_FAILED_NOTICE = ptBR["attachments.failed"];
export const ATTACHMENTS_PREVIEW_LABEL = ptBR["attachments.previewLabel"];
export const USER_IMAGES_LABEL = ptBR["attachments.sentLabel"];
export const THUMB_PLACEHOLDER_TEXT = ptBR["attachments.placeholder"];
export const ATTACHED_IMAGE_TITLE = ptBR["attachments.attachedTitle"];

type DraftStatus = "uploading" | "ready" | "error";

interface DraftImage {
  key: number;
  preview: string;
  status: DraftStatus;
  name?: string;
  error?: string;
}

function readDataUrl(file: Blob): Promise<string> {
  if (typeof FileReader !== "function") return Promise.resolve("");
  return new Promise((resolve) => {
    try {
      const reader = new FileReader();
      reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
      reader.onerror = () => resolve("");
      reader.onabort = () => resolve("");
      reader.readAsDataURL(file);
    } catch {
      resolve("");
    }
  });
}

let draftSeq = 0;

interface ChatAttachmentsApi {
  items: readonly DraftImage[];
  notice: string | null;
  add: (files: readonly Blob[]) => void;
  remove: (key: number) => void;
  clear: () => void;
  waitAll: () => Promise<readonly DraftImage[]>;
  setNotice: (notice: string | null) => void;
}

export function useChatAttachments(projectId: string): ChatAttachmentsApi {
  const [items, setItems] = useState<readonly DraftImage[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const current = useRef<readonly DraftImage[]>([]);
  const tasks = useRef(new Map<number, Promise<void>>());

  const commit = useCallback((next: readonly DraftImage[]) => {
    current.current = next;
    setItems(next);
  }, []);

  const patch = useCallback(
    (key: number, change: Partial<DraftImage>) => {
      if (!current.current.some((i) => i.key === key)) return;
      commit(current.current.map((i) => (i.key === key ? { ...i, ...change } : i)));
    },
    [commit],
  );

  const upload = useCallback(
    async (key: number, file: Blob): Promise<void> => {
      let prepared;
      try {
        prepared = await prepareImage(file);
      } catch {
        patch(key, { status: "error", error: t("attachments.readFailed") });
        return;
      }
      const result = await uploadChatAttachment(projectId, prepared.blob, prepared.type);
      if (result.ok) patch(key, { status: "ready", name: result.data.name, preview: chatAttachmentUrl(projectId, result.data.name) });
      else patch(key, { status: "error", error: apiFailureText(result.error) });
    },
    [projectId, patch],
  );

  const add = useCallback(
    (files: readonly Blob[]) => {
      const images = files.filter((f) => f.type.startsWith("image/"));
      if (images.length === 0) return;
      const room = Math.max(0, MAX_CHAT_ATTACHMENTS - current.current.length);
      setNotice(images.length > room ? t("attachments.limit", { max: MAX_CHAT_ATTACHMENTS }) : null);
      const accepted = images.slice(0, room);
      if (accepted.length === 0) return;
      const fresh: DraftImage[] = accepted.map(() => {
        draftSeq += 1;
        return { key: draftSeq, preview: "", status: "uploading" };
      });
      commit([...current.current, ...fresh]);
      fresh.forEach((item, i) => {
        const file = accepted[i];
        if (file === undefined) return;
        void readDataUrl(file).then((url) => {
          const now = current.current.find((d) => d.key === item.key);
          if (url !== "" && now !== undefined && now.status !== "ready") patch(item.key, { preview: url });
        });
        const task = upload(item.key, file);
        tasks.current.set(item.key, task);
        void task.finally(() => tasks.current.delete(item.key));
      });
    },
    [commit, patch, upload],
  );

  const remove = useCallback(
    (key: number) => {
      tasks.current.delete(key);
      commit(current.current.filter((i) => i.key !== key));
      setNotice(null);
    },
    [commit],
  );

  const clear = useCallback(() => {
    tasks.current.clear();
    commit([]);
    setNotice(null);
  }, [commit]);

  const waitAll = useCallback(async () => {
    await Promise.all([...tasks.current.values()]);
    return current.current;
  }, []);

  return { items, notice, add, remove, clear, waitAll, setNotice };
}

/** Imagens de uma lista de arquivos ou itens arrastados/colados. */
export function imageFiles(list: FileList | DataTransferItemList | null | undefined): File[] {
  if (list === null || list === undefined) return [];
  const out: File[] = [];
  for (const entry of Array.from(list as ArrayLike<File | DataTransferItem>)) {
    if (entry instanceof File) {
      if (entry.type.startsWith("image/")) out.push(entry);
      continue;
    }
    if (entry.kind !== "file" || !entry.type.startsWith("image/")) continue;
    const file = entry.getAsFile();
    if (file !== null) out.push(file);
  }
  return out;
}

function ThumbPlaceholder() {
  const { t } = useT();
  return (
    <span className="nd-thumb-placeholder">
      <Icon name="image" size={18} />
      <span>{t("attachments.placeholder")}</span>
    </span>
  );
}

function DraftThumb({ item, index, onOpen }: { item: DraftImage; index: number; onOpen: () => void }) {
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const showImage = item.preview !== "" && failedSrc !== item.preview;
  const { t } = useT();
  return (
    <button type="button" aria-label={t("attachments.zoom", { index: index + 1 })} onClick={onOpen} className="nd-draft-thumb">
      {showImage ? <img src={item.preview} alt={t("attachments.alt", { index: index + 1 })} onError={() => setFailedSrc(item.preview)} /> : <ThumbPlaceholder />}
    </button>
  );
}

export function AttachmentPreview({ items, onRemove }: { items: readonly DraftImage[]; onRemove: (key: number) => void }) {
  const [open, setOpen] = useState<number | null>(null);
  const { t } = useT();
  if (items.length === 0) return null;
  const opened = open === null ? undefined : items.find((i) => i.key === open);
  return (
    <>
      <ul aria-label={t("attachments.previewLabel")} className="nd-draft-list">
        {items.map((item, index) => (
          <li key={item.key} className="nd-draft-item" data-status={item.status}>
            <DraftThumb
              item={item}
              index={index}
              onOpen={() => {
                if (item.preview !== "") setOpen(item.key);
              }}
            />
            {item.status === "uploading" ? (
              <span role="status" className="nd-draft-status">
                {t("attachments.uploading")}
              </span>
            ) : null}
            {item.status === "error" ? (
              <span role="alert" className="nd-draft-error">
                {item.error}
              </span>
            ) : null}
            <button type="button" aria-label={t("attachments.remove", { index: index + 1 })} onClick={() => onRemove(item.key)} className="nd-wbtn nd-draft-remove">
              <Icon name="close" size={14} />
            </button>
          </li>
        ))}
      </ul>
      {opened !== undefined && opened.preview !== "" ? (
        <ImageLightbox src={opened.preview} alt={t("attachments.attachedTitle")} title={t("attachments.attachedTitle")} onClose={() => setOpen(null)} />
      ) : null}
    </>
  );
}

function UserThumb({ url, name, onOpen }: { url: string; name: string; onOpen: () => void }) {
  const [failed, setFailed] = useState(false);
  const { t } = useT();
  if (failed) return <span className="nd-thumb-failed">{t("attachments.unavailable")}</span>;
  return (
    <button type="button" aria-label={t("attachments.zoomSent")} data-name={name} onClick={onOpen} className="nd-thumb-btn">
      <img src={url} alt={t("attachments.sentAlt")} loading="lazy" decoding="async" onError={() => setFailed(true)} className="nd-thumb-img" />
    </button>
  );
}

export function ChatImageThumbs({ projectId, names }: { projectId: string; names: readonly string[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const { t } = useT();
  if (names.length === 0) return null;
  return (
    <>
      <ul aria-label={t("attachments.sentLabel")} className="nd-thumb-list nd-thumb-list-end">
        {names.map((name) => (
          <li key={name}>
            <UserThumb name={name} url={chatAttachmentUrl(projectId, name)} onOpen={() => setOpen(name)} />
          </li>
        ))}
      </ul>
      {open !== null ? (
        <ImageLightbox src={chatAttachmentUrl(projectId, open)} alt={t("attachments.sentAlt")} title={t("attachments.sentAlt")} onClose={() => setOpen(null)} />
      ) : null}
    </>
  );
}
