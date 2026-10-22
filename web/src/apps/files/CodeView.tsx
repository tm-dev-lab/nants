/*
 * Realce de sintaxe dos Arquivos (spec 093, T08; spec 066/T04):
 * CodeMirror 6 SOMENTE LEITURA, com o tema claro ou escuro dos tokens (`desktop/theme/codemirror`).
 * Só é carregado por import dinâmico (chunk próprio); a linguagem vem do nome do arquivo via
 * @codemirror/language-data (cada gramática é outro chunk). Qualquer falha chama `onError` e o
 * visualizador mantém o texto puro.
 *
 * CSP `style-src 'self'`: o style-mod do CodeMirror injetaria `<style>` em `document.head` e o
 * navegador bloquearia. Por isso o editor vive num shadow root, onde o style-mod usa
 * `adoptedStyleSheets` (CSSStyleSheet construída, permitida pela CSP). As variáveis CSS herdam
 * para dentro do shadow; classes não, então o tema usa só `EditorView.theme`.
 * Sem suporte a `adoptedStyleSheets` o editor não é montado (onError, texto puro).
 */
import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import type { Extension } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { LanguageDescription } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { codeMirrorTheme } from "../../desktop/theme/codemirror";
import type { ThemeName } from "../../desktop/theme/tokens";

/** O shadow root só serve se aceitar folhas construídas (`adoptedStyleSheets`). */
export function shadowStylesSupported(): boolean {
  return (
    typeof Element !== "undefined" &&
    typeof Element.prototype.attachShadow === "function" &&
    typeof ShadowRoot !== "undefined" &&
    "adoptedStyleSheets" in ShadowRoot.prototype &&
    typeof CSSStyleSheet !== "undefined"
  );
}

export interface CodeViewProps {
  content: string;
  /** Nome do arquivo (caminho) para escolher a linguagem. */
  filename: string;
  wrap: boolean;
  /** Tema efetivo do desktop (claro ou escuro). */
  theme: ThemeName;
  /** Falha ao montar o editor: o chamador volta ao texto puro. */
  onError: () => void;
}

export default function CodeView({ content, filename, wrap, theme, onError }: CodeViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;

  useEffect(() => {
    const parent = host.current;
    if (parent === null) return;
    if (!shadowStylesSupported()) {
      onErrorRef.current();
      return;
    }
    const root = parent.shadowRoot ?? parent.attachShadow({ mode: "open" });
    root.replaceChildren();
    const mountPoint = document.createElement("div");
    mountPoint.style.height = "100%";
    root.appendChild(mountPoint);
    let cancelled = false;
    let view: EditorView | null = null;

    async function mount(target: HTMLElement): Promise<void> {
      let language: Extension = [];
      try {
        const description = LanguageDescription.matchFilename(languages, filename);
        if (description !== null) language = await description.load();
      } catch {
        language = []; // gramática indisponível: segue sem realce, ainda no editor
      }
      if (cancelled) return;
      const extensions: Extension[] = [
        lineNumbers(),
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
        ...codeMirrorTheme(theme),
        language,
      ];
      if (wrap) extensions.push(EditorView.lineWrapping);
      view = new EditorView({ state: EditorState.create({ doc: content, extensions }), root, parent: target });
    }

    mount(mountPoint).catch(() => {
      if (!cancelled) onErrorRef.current();
    });
    return () => {
      cancelled = true;
      view?.destroy();
    };
  }, [content, filename, wrap, theme]);

  // O host ocupa a área do visualizador (pai posicionado); a rolagem fica no `.cm-scroller`.
  return <div ref={host} data-testid="file-code-highlighted" data-code-theme={theme} className="nd-code-host" />;
}
