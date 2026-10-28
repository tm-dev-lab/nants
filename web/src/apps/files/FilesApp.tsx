/*
 * Janela Arquivos (`DEFAULT_APPS.files.Content`, em `memo`). Somente leitura:
 * - fonte "Código integrado" ou "Trabalho em andamento · Txx" (sem tarefa ativa, desabilitada; o
 *   409 durante a fonte da tarefa volta para a base com uma nota curta);
 * - a fonte e "Atualizar" ficam no slot da barra de título (095/T06, C-07);
 * - árvore (ocultos, "Só alterados", pastas ignoradas sob demanda) e visualizador (CodeMirror
 *   claro ou escuro, máscara, diff, copiar);
 * - o arquivo aberto e a fonte dele ficam em `selection` (voltam ao recarregar e ao trocar de
 *   espaço); um arquivo que não existe mais é esquecido sem erro;
 * - árvore e visualizador lado a lado, com a divisória redimensionável por ponteiro e teclado
 *   (fração em `selection.filesSplit`); estreita (`data-size="narrow"`), árvore OU arquivo, com
 *   "Voltar";
 * - atualização barata: eventos (com debounce), polling de 8 s e "Atualizar". Tudo pausa com a
 *   janela invisível (`useWindowVisibility`): minimizada, coberta por uma maximizada ou com a
 *   aba oculta não sai nenhuma requisição; ao voltar a ficar visível, atualiza uma vez.
 * - montada já invisível (por exemplo, minimizada no arranjo salvo), a janela não faz nem as
 *   requisições iniciais (sonda da fonte, lista, arquivo salvo): a primeira carga acontece quando
 *   ela fica visível pela primeira vez.
 */
import "./files.css";
import { memo, useCallback, useEffect, useId, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { getProjectFiles, type FileSource } from "../../api/client";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useSpace } from "../../desktop/shellContext";
import type { ContainerSize } from "../../desktop/useContainerSize";
import type { UiSelection } from "../../desktop/uiState";
import { useWindowVisibility } from "../../desktop/useWindowVisibility";
import { WindowTools } from "../../desktop/Window";
import { useT } from "../../i18n/react";
import { consumeOpenFile, useOpenFileRequest } from "./openRequest";
import { apiFailureText } from "../../i18n/serverCodes";
import { useAppStore } from "../../store";
import { useFileTree } from "../../useFileTree";
import type { AppContentProps } from "../registry";
import { FileTree } from "./FileTree";
import { FileViewer } from "./FileViewer";
import {
  clampSplit,
  DEFAULT_FILES_SPLIT,
  FILES_EVENT_DEBOUNCE_MS,
  FILES_POLL_MS,
  filesArrangement,
  splitByKey,
  splitFromPointer,
} from "./filesModel";
import "../shared/apps.css";

type SetSelection = (patch: Partial<UiSelection>) => void;

/**
 * Atualização dos Arquivos. Devolve um contador que
 * o corpo e a sonda da fonte usam como `refreshKey`, e `active`: a janela já esteve visível
 * (antes disso, nada é carregado). Fontes: eventos (`revisions` e a troca de passo dos agentes),
 * com debounce; polling de `FILES_POLL_MS`; e "Atualizar". Com a janela invisível não sai nenhuma
 * requisição.
 */
export function useFilesRefresh(projectId: string, visible: boolean): { tick: number; refreshNow: () => void; active: boolean } {
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  // Assinatura dos passos dos agentes: muda ao começar/terminar um passo, não a cada token.
  const activitySig = useAppStore((s) => (s.activities[projectId] ?? []).map((a) => `${a.key}:${a.activityId}:${a.endedAt ?? ""}`).join("|"));
  const [tick, setTick] = useState(0);
  const bump = useCallback((): void => setTick((n) => n + 1), []);

  const eventSig = `${revision}#${activitySig}`;
  const lastEvent = useRef(eventSig);
  useEffect(() => {
    if (lastEvent.current === eventSig) return;
    lastEvent.current = eventSig;
    if (!visible) return;
    const timer = window.setTimeout(bump, FILES_EVENT_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [eventSig, visible, bump]);

  // Primeira vez visível: ativa (a carga inicial sai da montagem do corpo e da sonda, sem
  // atualização extra). Voltar a ficar visível depois disso (restaurar, descobrir ou voltar à aba)
  // atualiza uma vez.
  const [active, setActive] = useState(visible);
  const wasVisible = useRef(visible);
  useEffect(() => {
    if (visible && !wasVisible.current) {
      if (active) bump();
      else setActive(true);
    }
    wasVisible.current = visible;
  }, [visible, active, bump]);

  useEffect(() => {
    if (!visible) return;
    const timer = window.setInterval(bump, FILES_POLL_MS);
    return () => window.clearInterval(timer);
  }, [visible, bump]);

  return { tick, refreshNow: bump, active };
}

interface BodyProps {
  projectId: string;
  source: FileSource;
  stacked: boolean;
  /** Arquivo aberto salvo para esta fonte (reaberto quando a lista confirma que existe). */
  initialPath: string | null;
  split: number;
  setSelection: SetSelection;
  /** `source=task` respondeu 409 (sem tarefa ativa): o seletor volta para a base. */
  onNoActiveTask: () => void;
  refreshKey: number;
  /** Arquivo pedido por outra janela (spec 130); aberto na hora e marcado como atendido. */
  openRequest?: { id: number; path: string } | null;
  onOpened?: (id: number) => void;
}

/** Divisória entre a árvore e o visualizador: ponteiro e teclado; grava ao soltar. */
function SplitHandle({
  value,
  onPreview,
  onCommit,
  containerRef,
}: {
  value: number;
  onPreview: (value: number) => void;
  onCommit: (value: number) => void;
  containerRef: { current: HTMLElement | null };
}) {
  const { t } = useT();
  const dragging = useRef(false);
  const latest = useRef(value);
  latest.current = value;
  const fromEvent = (event: ReactPointerEvent<HTMLElement>): number => {
    const rect = containerRef.current?.getBoundingClientRect();
    return rect === undefined ? value : splitFromPointer(event.clientX, rect.left, rect.width);
  };
  const percent = Math.round(value * 100);
  return (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation="vertical"
      aria-label={t("files.split")}
      aria-valuenow={percent}
      aria-valuemin={15}
      aria-valuemax={85}
      aria-valuetext={t("files.splitValue", { percent })}
      className="nd-files-handle"
      data-files-handle=""
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        dragging.current = true;
        event.currentTarget.setPointerCapture?.(event.pointerId);
        event.preventDefault();
      }}
      onPointerMove={(event) => {
        if (dragging.current) onPreview(fromEvent(event));
      }}
      onPointerUp={(event) => {
        if (!dragging.current) return;
        dragging.current = false;
        const next = fromEvent(event);
        onPreview(next);
        onCommit(next);
      }}
      onPointerCancel={() => {
        if (!dragging.current) return;
        dragging.current = false;
        onCommit(latest.current);
      }}
      onDoubleClick={() => {
        onPreview(DEFAULT_FILES_SPLIT);
        onCommit(DEFAULT_FILES_SPLIT);
      }}
      onKeyDown={(event) => {
        const next = splitByKey(value, event.key);
        if (next === null) return;
        event.preventDefault();
        onPreview(next);
        onCommit(next);
      }}
    />
  );
}

function FilesBody({ projectId, source, stacked, initialPath, split, setSelection, onNoActiveTask, refreshKey, openRequest = null, onOpened }: BodyProps) {
  const { t } = useT();
  const model = useFileTree(projectId, source, refreshKey);
  const [selected, setSelected] = useState<string | null>(null);
  const [showFile, setShowFile] = useState(false);
  const [width, setWidth] = useState(split);
  useEffect(() => setWidth(split), [split]);
  const { list } = model;
  const bodyRef = useRef<HTMLDivElement | null>(null);
  // Larga: a árvore fica com a largura salva também sem arquivo aberto (o lado do visualizador mostra a orientação, 094/T08 A-07).
  const sideBySide = !stacked;

  const remember = useCallback((path: string | null) => setSelection({ filePath: path, fileSource: source }), [setSelection, source]);

  const [restoreFocus, setRestoreFocus] = useState<string | null>(null);

  // Ao fechar o arquivo o foco volta para o item dele na árvore.
  useEffect(() => {
    if (restoreFocus === null || selected !== null) return;
    const root = bodyRef.current;
    const item = root === null ? undefined : Array.from(root.querySelectorAll<HTMLElement>("[data-path]")).find((el) => el.dataset.path === restoreFocus);
    item?.focus();
    setRestoreFocus(null);
  }, [restoreFocus, selected]);
  const closeFile = (): void => {
    setRestoreFocus(selected);
    setSelected(null);
    setShowFile(false);
    remember(null);
  };

  // Arquivo aberto antes (ui-state): reabre quando a lista confirma que ele ainda existe.
  const [restorePath, setRestorePath] = useState(initialPath);
  useEffect(() => {
    if (restorePath === null || list.status === "loading") return;
    const exists = list.status === "ok" && list.entries.some((entry) => entry.path === restorePath && entry.kind === "file");
    if (exists) {
      setSelected(restorePath);
      setShowFile(true);
    } else {
      remember(null);
    }
    setRestorePath(null);
  }, [restorePath, list, remember]);
  const noActiveTask = list.status === "error" && list.reason === "no_active_task";
  useEffect(() => {
    if (source === "task" && noActiveTask) onNoActiveTask();
  }, [source, noActiveTask, onNoActiveTask]);
  const changed = list.status === "ok" ? list.changed : null;

  const open = (path: string): void => {
    setRestorePath(null);
    setSelected(path);
    setShowFile(true);
    remember(path);
  };
  // Pedido de outra janela (spec 130: arquivo criado por um programa): abre direto, sem esperar a
  // lista (o arquivo pode ser novo ou ignorado pelo git).
  const openRef = useRef(open);
  openRef.current = open;
  useEffect(() => {
    if (openRequest === null) return;
    openRef.current(openRequest.path);
    onOpened?.(openRequest.id);
  }, [openRequest, onOpened]);
  // Estreita: "Voltar" à árvore também deixa de contar como arquivo aberto.
  const backToTree = (): void => {
    setShowFile(false);
    remember(null);
  };

  const tree = (
    <div className="nd-files-tree" data-files-tree="" style={sideBySide ? { flexBasis: `${width * 100}%` } : undefined} data-split={sideBySide ? "true" : undefined}>
      <div className="nd-row nd-files-tools">
        <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" aria-pressed={model.showHidden} onClick={() => model.setShowHidden(!model.showHidden)}>
          <Icon name="eye" size={14} />
          {t("files.showHidden")}
        </button>
        {source === "task" ? (
          <button type="button" className="nd-btn nd-btn-sm" data-variant="ghost" aria-pressed={model.onlyChanged} onClick={() => model.setOnlyChanged(!model.onlyChanged)}>
            <Icon name="diff" size={14} />
            {t("files.onlyChanged")}
          </button>
        ) : null}
      </div>
      {list.status === "ok" && list.truncated ? (
        <p role="status" className="nd-strip" data-tone="amber">
          {t("files.truncated")}
        </p>
      ) : null}
      <div className="nd-files-tree-scroll nd-app-scroll">
        {list.status === "loading" ? (
          <p role="status" className="nd-file-note">
            {t("files.loading")}
          </p>
        ) : null}
        {list.status === "error" ? (
          <p role="alert" className="nd-file-note nd-field-error">
            {t("files.listError", { message: apiFailureText(list.failure) })}
          </p>
        ) : null}
        {list.status === "ok" ? <FileTree model={model} selectedPath={selected} onOpen={open} /> : null}
      </div>
    </div>
  );

  const viewer =
    selected === null ? null : (
      <FileViewer
        onClose={closeFile}
        projectId={projectId}
        source={source}
        path={selected}
        refreshKey={refreshKey}
        changed={source === "task" && changed !== null && changed.has(selected)}
      />
    );

  if (stacked) {
    if (showFile && selected !== null) {
      return (
        <div ref={bodyRef} className="nd-files-body" data-arrangement="stacked">
          <div className="nd-files-back">
            <button type="button" className="nd-btn nd-btn-sm" onClick={backToTree}>
              <Icon name="arrow-left" size={14} />
              {t("common.back")}
            </button>
          </div>
          {viewer}
        </div>
      );
    }
    return (
      <div ref={bodyRef} className="nd-files-body" data-arrangement="stacked">
        {tree}
      </div>
    );
  }
  return (
    <div ref={bodyRef} className="nd-files-body" data-arrangement="split">
      {tree}
      {viewer !== null ? (
        <>
          <SplitHandle value={width} onPreview={(v) => setWidth(clampSplit(v))} onCommit={(v) => setSelection({ filesSplit: clampSplit(v) })} containerRef={bodyRef} />
          <div className="nd-files-view">{viewer}</div>
        </>
      ) : (
        <div className="nd-files-view nd-files-placeholder" data-files-placeholder="">
          <EmptyState icon="file" message={t("files.viewerEmpty")} />
        </div>
      )}
    </div>
  );
}

interface ExplorerProps {
  projectId: string;
  size: ContainerSize;
  /** Valores da seleção salvos (só lidos na montagem do corpo de cada fonte). */
  savedSource: FileSource;
  savedPath: string | null;
  split: number;
  setSelection: SetSelection;
}

/**
 * Seletor de fonte e corpo. O corpo é remontado (key) a
 * cada troca de fonte: árvore e arquivo aberto nunca atravessam a fronteira.
 */
const FilesExplorer = memo(function FilesExplorer({ projectId, size, savedSource, savedPath, split, setSelection }: ExplorerProps) {
  const { t } = useT();
  const visible = useWindowVisibility("files");
  const { tick, refreshNow, active } = useFilesRefresh(projectId, visible);
  const [source, setSource] = useState<FileSource>(savedSource);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [probed, setProbed] = useState(false);
  // Nota curta da volta para a base (a tarefa terminou); o texto sai do dicionário na hora.
  const [note, setNote] = useState(false);
  const reasonId = useId();
  // Valores salvos na montagem: o corpo só os lê ao montar (a seleção muda a cada arquivo aberto).
  const saved = useRef({ source: savedSource, path: savedPath });
  // Arquivo pedido por outra janela (spec 130): sempre na base, onde o programa gravou.
  const openRequest = useOpenFileRequest(projectId);
  useEffect(() => {
    if (openRequest !== null) setSource("base");
  }, [openRequest]);

  // Sonda a fonte da tarefa na abertura (já visível) e a cada atualização (evento, polling ou
  // "Atualizar"). Montada invisível, só sonda quando fica visível pela primeira vez.
  useEffect(() => {
    if (!active) return;
    let live = true;
    void getProjectFiles(projectId, "task").then((result) => {
      if (!live) return;
      setProbed(true);
      if (result.ok) setTaskId(result.data.taskId ?? "");
      else if (result.error.reason === "no_active_task") setTaskId(null);
      // Falha de rede/outra: mantém o último estado conhecido.
    });
    return () => {
      live = false;
    };
  }, [projectId, tick, active]);

  const goBase = useCallback((): void => {
    setSource("base");
    setTaskId(null);
    setNote(true);
  }, []);

  const taskAvailable = taskId !== null;
  const taskLabel = taskId !== null && taskId !== "" ? t("files.task", { task: taskId }) : t("files.taskPlain");
  // Tarefa sumiu enquanto a fonte era "tarefa" (sonda ou lista): volta para a base com a nota.
  useEffect(() => {
    if (source === "task" && probed && !taskAvailable) goBase();
  }, [source, probed, taskAvailable, goBase]);

  const choose = (next: FileSource): void => {
    setNote(false);
    setSource(next);
  };
  const stacked = filesArrangement(size) === "stacked";

  return (
    <div className="nd-files" data-files="" data-size={size}>
      {/*
       * Fonte e "Atualizar" no slot da barra de título (095/T06, C-07; no celular, a linha abaixo
       * dela). Estreita, o motivo da fonte desabilitada fica só para o leitor de tela.
       */}
      <WindowTools inlineOutsideWindow>
        <div className="nd-files-toolbar" data-files-toolbar="">
          <div role="group" aria-label={t("files.source")} className="nd-segment nd-chrome nd-files-segment">
            <button type="button" className="nd-segment-item" aria-pressed={source === "base"} onClick={() => choose("base")}>
              <span>{t("files.base")}</span>
            </button>
            <button
              type="button"
              className="nd-segment-item"
              aria-pressed={source === "task"}
              disabled={!taskAvailable}
              aria-describedby={!taskAvailable ? reasonId : undefined}
              onClick={() => choose("task")}
            >
              <span>{taskLabel}</span>
            </button>
          </div>
          {!taskAvailable ? (
            <span id={reasonId} data-testid="files-source-reason" className={size === "narrow" ? "nd-sr-only" : "nd-files-reason"}>
              {t("files.noTask")}
            </span>
          ) : null}
          <button type="button" className="nd-btn nd-iconbtn nd-btn-sm" data-variant="ghost" aria-label={t("files.refresh")} title={t("files.refresh")} onClick={refreshNow}>
            <Icon name="refresh" size={14} />
          </button>
        </div>
      </WindowTools>
      {note && source === "base" ? (
        <p role="status" className="nd-strip">
          {t("files.taskEnded")}
        </p>
      ) : null}
      {active ? (
        <FilesBody
          key={`${projectId}:${source}`}
          projectId={projectId}
          source={source}
          stacked={stacked}
          initialPath={saved.current.source === source ? saved.current.path : null}
          split={split}
          setSelection={setSelection}
          onNoActiveTask={goBase}
          refreshKey={tick}
          openRequest={source === "base" ? openRequest : null}
          onOpened={consumeOpenFile}
        />
      ) : null}
    </div>
  );
});

function FilesContentView({ size }: AppContentProps) {
  const space = useSpace();
  if (space === null) return null;
  const { selection } = space;
  return (
    <FilesExplorer
      key={space.data.projectId}
      projectId={space.data.projectId}
      size={size}
      savedSource={selection.fileSource ?? "base"}
      savedPath={selection.filePath}
      split={selection.filesSplit ?? DEFAULT_FILES_SPLIT}
      setSelection={space.setSelection}
    />
  );
}

export const FilesContent = memo(FilesContentView);
