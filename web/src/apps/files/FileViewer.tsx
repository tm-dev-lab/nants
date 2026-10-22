/*
 * Visualizador de arquivo da janela Arquivos (spec 093, T08; spec 066/T03-T04): texto com número de linha, quebra opcional, copiar, estados
 * binário/grande demais/erro/carregando e máscara de valores ("Mostrar valores", não guardado).
 * Realce CodeMirror (claro ou escuro, pelo tema do desktop) carregado sob demanda, só dentro do
 * teto; falha ao carregar ou montar mantém o texto puro. Em `source=task`, arquivo alterado
 * ganha "Diff" (unificado, linhas marcadas por tipo em `<pre>`).
 * Todo conteúdo vem do servidor e entra como texto React (nunca como HTML).
 */
import { useEffect, useRef, useState, type ComponentType } from "react";
import { getProjectFile, type ApiFailure, type FileContent, type FileSource } from "../../api/client";
import { Icon } from "../../desktop/Icon";
import { useAppearance } from "../../desktop/theme/appearanceContext";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import type { CodeViewProps } from "./CodeView";
import { diffLineKind, formatBytes, HIGHLIGHT_MAX_BYTES, HIGHLIGHT_MAX_LINES, splitLines } from "./filesModel";

interface FileViewerProps {
  projectId: string;
  source: FileSource;
  path: string;
  /** Muda a cada atualização do projeto: relê o arquivo com If-None-Match. */
  refreshKey: number;
  /** Arquivo alterado na tarefa (só em `source=task`): habilita o "Diff". */
  changed?: boolean;
  /** Fecha o arquivo (volta só para a árvore). */
  onClose?: () => void;
}

type CodeViewComponent = ComponentType<CodeViewProps>;

let codeViewLoad: Promise<CodeViewComponent> | null = null;
/** Import dinâmico (chunk próprio). Uma falha não fica em cache: a próxima abertura tenta de novo. */
function loadCodeView(): Promise<CodeViewComponent> {
  if (codeViewLoad === null) {
    codeViewLoad = import("./CodeView").then(
      (m) => m.default,
      (error: unknown) => {
        codeViewLoad = null;
        throw error;
      },
    );
  }
  return codeViewLoad;
}

type View = { status: "loading" } | { status: "error"; failure: ApiFailure } | { status: "ok"; file: FileContent };

export function FileViewer({ projectId, source, path, refreshKey, changed = false, onClose }: FileViewerProps) {
  const { theme } = useAppearance();
  const { t } = useT();
  const [view, setView] = useState<View>({ status: "loading" });
  const [revealed, setRevealed] = useState(false);
  const [wrap, setWrap] = useState(false);
  const [diff, setDiff] = useState(false);
  const [CodeView, setCodeView] = useState<CodeViewComponent | null>(null);
  const [highlightFailed, setHighlightFailed] = useState(false);
  const [copied, setCopied] = useState<"ok" | "fail" | null>(null);
  const etag = useRef<string | null>(null);
  const loadedKey = useRef<string | null>(null);
  const seq = useRef(0);
  const hasContent = useRef(false);

  // Trocar de arquivo descarta o "mostrar valores" (não guardado) e o conteúdo anterior.
  const [shownPath, setShownPath] = useState(path);
  if (shownPath !== path) {
    setShownPath(path);
    setRevealed(false);
    setDiff(false);
    setHighlightFailed(false);
    setView({ status: "loading" });
    setCopied(null);
  }

  useEffect(() => {
    const key = `${projectId}|${source}|${path}|${revealed ? 1 : 0}|${diff ? 1 : 0}`;
    const same = loadedKey.current === key;
    if (!same) {
      etag.current = null;
      hasContent.current = false;
      setView({ status: "loading" });
    }
    const mine = ++seq.current;
    void getProjectFile(projectId, source, path, {
      reveal: revealed,
      diff: diff && source === "task",
      etag: same ? etag.current : null,
    }).then((result) => {
      if (seq.current !== mine) return; // resposta superada
      if (!result.ok) {
        // Falha numa releitura de fundo mantém o que já está na tela.
        if (!hasContent.current) setView({ status: "error", failure: result.error });
        return;
      }
      loadedKey.current = key;
      if (result.data.status === "not_modified") return;
      etag.current = result.data.etag;
      hasContent.current = true;
      setView({ status: "ok", file: result.data.file });
    });
  }, [projectId, source, path, revealed, diff, refreshKey]);

  useEffect(() => {
    if (copied === null) return;
    const timer = setTimeout(() => setCopied(null), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  async function copy(content: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(content);
      setCopied("ok");
    } catch {
      setCopied("fail");
    }
  }

  const file = view.status === "ok" ? view.file : null;
  const lines = file?.kind === "text" ? splitLines(file.content) : [];
  const diffLines = file?.kind === "diff" ? splitLines(file.content) : [];
  const highlightable =
    file?.kind === "text" && !highlightFailed && file.content.length <= HIGHLIGHT_MAX_BYTES && lines.length <= HIGHLIGHT_MAX_LINES;

  // Carrega o realce só quando há um arquivo elegível; falha: texto puro.
  useEffect(() => {
    if (!highlightable || CodeView !== null) return;
    let live = true;
    loadCodeView().then(
      (component) => {
        if (live) setCodeView(() => component);
      },
      () => {
        if (live) setHighlightFailed(true);
      },
    );
    return () => {
      live = false;
    };
  }, [highlightable, CodeView]);

  const slash = path.lastIndexOf("/") + 1;
  return (
    <section aria-label={t("files.content")} className="nd-file-viewer" data-file-viewer="">
      <div className="nd-file-head">
        <div className="nd-file-path-row">
          <span data-testid="file-path" title={path} className="nd-file-path nd-mono">
            <span className="nd-file-dir">{path.slice(0, slash)}</span>
            <span className="nd-file-name">{path.slice(slash)}</span>
          </span>
          {onClose !== undefined ? (
            <button type="button" className="nd-wbtn" aria-label={t("files.closeFile")} title={t("files.closeFile")} onClick={onClose}>
              <Icon name="close" size={14} />
            </button>
          ) : null}
        </div>
        <div data-testid="file-actions" className="nd-row nd-file-actions">
          {file !== null ? <span className="nd-muted nd-file-size">{formatBytes(file.size)}</span> : null}
          {changed && source === "task" ? (
            <button type="button" className="nd-btn nd-btn-sm" aria-pressed={diff} onClick={() => setDiff((v) => !v)}>
              <Icon name="diff" size={14} />
              Diff
            </button>
          ) : null}
          {file?.kind === "text" ? (
            <button type="button" className="nd-btn nd-btn-sm" aria-pressed={wrap} onClick={() => setWrap((v) => !v)}>
              <Icon name="wrap" size={14} />
              {t("files.wrap")}
            </button>
          ) : null}
          {file?.kind === "text" || file?.kind === "diff" ? (
            <button type="button" className="nd-btn nd-btn-sm" onClick={() => void copy(file.content)}>
              <Icon name="copy" size={14} />
              {t("files.copy")}
            </button>
          ) : null}
          {copied !== null ? (
            <span role="status" className={copied === "ok" ? "nd-ok" : "nd-field-error"}>
              {copied === "ok" ? t("result.copied") : t("files.copyFailed")}
            </span>
          ) : null}
        </div>
      </div>

      {file?.kind === "text" && file.masked && !revealed ? (
        <div role="status" className="nd-strip nd-file-masked" data-tone="amber">
          <span>{t("files.masked")}</span>
          <button type="button" className="nd-btn nd-btn-sm" onClick={() => setRevealed(true)}>
            <Icon name="eye" size={14} />
            {t("files.reveal")}
          </button>
        </div>
      ) : null}

      <div className="nd-file-body">
        {view.status === "loading" ? (
          <p role="status" className="nd-file-note">
            {t("files.loadingFile")}
          </p>
        ) : null}
        {view.status === "error" ? (
          <p role="alert" className="nd-file-note nd-field-error">
            {t("files.openError", { message: apiFailureText(view.failure) })}
          </p>
        ) : null}
        {file?.kind === "binary" ? (
          <p role="status" className="nd-file-note">
            {t("files.binary", { size: formatBytes(file.size) })}
          </p>
        ) : null}
        {file?.kind === "too_large" ? (
          <p role="status" className="nd-file-note">
            {t("files.tooLarge", { size: formatBytes(file.size) })}
          </p>
        ) : null}
        {file?.kind === "diff" ? (
          diffLines.length === 0 ? (
            <p role="status" className="nd-file-note">
              {t("files.noDiff")}
            </p>
          ) : (
            <pre data-testid="file-diff" className="nd-file-diff nd-mono" data-wrap={wrap ? "true" : undefined}>
              {diffLines.map((line, index) => (
                <span key={index} className="nd-diff-line" data-diff={diffLineKind(line)}>
                  {line === "" ? " " : line}
                </span>
              ))}
            </pre>
          )
        ) : null}
        {file?.kind === "text" && highlightable && CodeView !== null ? (
          <CodeView content={file.content} filename={path} wrap={wrap} theme={theme} onError={() => setHighlightFailed(true)} />
        ) : null}
        {file?.kind === "text" && !(highlightable && CodeView !== null) ? (
          <div data-testid="file-code" className="nd-file-code nd-mono" data-wrap={wrap ? "true" : undefined}>
            {lines.map((line, index) => (
              <div key={index} className="nd-code-line">
                <span aria-hidden="true" className="nd-code-gutter">
                  {index + 1}
                </span>
                <span className="nd-code-text">{line === "" ? " " : line}</span>
              </div>
            ))}
          </div>
        ) : null}
      </div>
    </section>
  );
}
