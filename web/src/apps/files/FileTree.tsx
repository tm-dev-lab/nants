/*
 * Árvore de arquivos da janela Arquivos (spec 093, T08; spec 066/T03): role tree/treeitem com aria-expanded, foco móvel (roving tabindex) e teclado (setas,
 * Home/End, Enter/Espaço). Nomes de arquivo entram sempre como texto. O recuo por nível vai numa
 * variável CSS (`--nd-level`).
 */
import { useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { Icon } from "../../desktop/Icon";
import { parentPath, type TreeNode } from "../../fileTree";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import type { FileTreeModel } from "../../useFileTree";

interface Props {
  model: FileTreeModel;
  selectedPath: string | null;
  onOpen: (path: string) => void;
}

const levelStyle = (level: number): CSSProperties => ({ ["--nd-level" as string]: String(level - 1) });

export function FileTree({ model, selectedPath, onOpen }: Props) {
  const { t } = useT();
  const { rows, expanded, dirStates, loaded, toggle, expand, collapse } = model;
  const changedSet = model.list.status === "ok" ? model.list.changed : null;
  const [focusPath, setFocusPath] = useState<string | null>(null);
  const items = useRef(new Map<string, HTMLElement>());

  const current = rows.find((row) => row.node.path === focusPath) ?? rows[0] ?? null;

  // Foca direto (o onFocus do item atualiza o foco móvel): nada fica armado para atualizações futuras.
  function moveTo(path: string): void {
    items.current.get(path)?.focus();
  }

  function activate(node: TreeNode): void {
    if (node.kind === "dir") toggle(node);
    else if (node.kind === "file") onOpen(node.path);
    // symlink: não abre
  }

  function onKeyDown(event: KeyboardEvent<HTMLElement>): void {
    if (current === null) return;
    const index = rows.indexOf(current);
    const node = current.node;
    let handled = true;
    switch (event.key) {
      case "ArrowDown": {
        const next = rows[Math.min(rows.length - 1, index + 1)];
        if (next !== undefined) moveTo(next.node.path);
        break;
      }
      case "ArrowUp": {
        const prev = rows[Math.max(0, index - 1)];
        if (prev !== undefined) moveTo(prev.node.path);
        break;
      }
      case "Home":
        if (rows[0] !== undefined) moveTo(rows[0].node.path);
        break;
      case "End": {
        const last = rows[rows.length - 1];
        if (last !== undefined) moveTo(last.node.path);
        break;
      }
      case "ArrowRight":
        if (node.kind === "dir") {
          if (!expanded.has(node.path)) expand(node);
          else {
            const child = rows[index + 1];
            if (child !== undefined && child.level > current.level) moveTo(child.node.path);
          }
        }
        break;
      case "ArrowLeft":
        if (node.kind === "dir" && expanded.has(node.path)) collapse(node);
        else {
          const parent = parentPath(node.path);
          if (parent !== null && rows.some((row) => row.node.path === parent)) moveTo(parent);
        }
        break;
      case "Enter":
      case " ":
        activate(node);
        break;
      default:
        handled = false;
    }
    if (handled) event.preventDefault();
  }

  if (rows.length === 0) {
    return (
      <p role="status" className="nd-file-note">
        {t("files.empty")}
      </p>
    );
  }

  return (
    <div role="tree" aria-label={t("files.tree")} onKeyDown={onKeyDown} className="nd-tree">
      {rows.map(({ node, level }) => {
        const isDir = node.kind === "dir";
        const open = isDir && expanded.has(node.path);
        const dirState = dirStates.get(node.path);
        const info = open && node.lazy ? loaded.get(node.path) : undefined;
        const focusable = current !== null && current.node.path === node.path;
        const selected = node.kind === "file" && selectedPath === node.path;
        return (
          <div key={node.path}>
            <div
              role="treeitem"
              aria-level={level}
              aria-expanded={isDir ? open : undefined}
              aria-selected={node.kind === "file" ? selected : undefined}
              aria-disabled={node.kind === "symlink" ? true : undefined}
              tabIndex={focusable ? 0 : -1}
              data-path={node.path}
              data-selected={selected ? "true" : undefined}
              ref={(el) => {
                if (el === null) items.current.delete(node.path);
                else items.current.set(node.path, el);
              }}
              onClick={() => {
                setFocusPath(node.path);
                activate(node);
              }}
              onFocus={() => setFocusPath(node.path)}
              style={levelStyle(level)}
              className="nd-tree-item"
            >
              {isDir ? (
                <Icon name="chevron-right" size={14} className="nd-tree-chevron" />
              ) : (
                <span aria-hidden="true" className="nd-tree-spacer" />
              )}
              <Icon name={isDir ? "folder" : node.kind === "symlink" ? "link" : "file"} size={14} className="nd-tree-icon" />
              <span className="nd-tree-name">{node.name}</span>
              {node.ignoredRoot ? <span className="nd-tree-tag">{t("files.ignored")}</span> : null}
              {node.kind === "symlink" ? <span className="nd-tree-tag">{t("files.symlink")}</span> : null}
              {node.kind === "file" && changedSet?.has(node.path) === true ? (
                <span className="nd-tree-changed" data-changed="">
                  <span aria-hidden="true" className="nd-tree-dot" />
                  <span className="nd-sr-only">{t("files.changed")}</span>
                </span>
              ) : null}
            </div>
            {dirState?.status === "loading" ? (
              <p role="status" style={levelStyle(level + 1)} className="nd-tree-note">
                {t("files.loadingDir")}
              </p>
            ) : null}
            {dirState?.status === "error" ? (
              <p role="alert" style={levelStyle(level + 1)} className="nd-tree-note nd-field-error">
                {apiFailureText(dirState.failure)}
              </p>
            ) : null}
            {info?.truncated === true ? (
              <p role="status" style={levelStyle(level + 1)} className="nd-tree-note">
                {t("files.dirTruncated")}
              </p>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
