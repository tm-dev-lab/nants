/*
 * Markdown restrito (ver `reportMarkdown.ts`):
 * todo texto entra como texto React, sem HTML bruto, imagens nem links clicáveis. Estilos pelos
 * tokens (`.nd-md`).
 */
import { useMemo } from "react";
import { parseReportMarkdown, type Block, type Inline, type ListBlock } from "../../reportMarkdown";

export function ReportMarkdown({ blocks }: { blocks: readonly Block[] }) {
  return (
    <div className="nd-md">
      {blocks.map((block, index) => (
        <BlockView key={index} block={block} />
      ))}
    </div>
  );
}

/** Texto do Arquiteto ou dos agentes em Markdown restrito. */
export function MarkdownText({ text, keepLineBreaks = false }: { text: string; keepLineBreaks?: boolean }) {
  const blocks = useMemo(() => parseReportMarkdown(text, { keepLineBreaks }), [text, keepLineBreaks]);
  return <ReportMarkdown blocks={blocks} />;
}

function BlockView({ block }: { block: Block }) {
  switch (block.kind) {
    case "heading":
      return <Heading level={block.level} inlines={block.inlines} />;
    case "paragraph":
      return (
        <p className="nd-md-p">
          <Inlines inlines={block.inlines} />
        </p>
      );
    case "code":
      return (
        <pre className="nd-md-pre">
          <code data-lang={block.lang === "" ? undefined : block.lang}>{block.text}</code>
        </pre>
      );
    case "list":
      return <List list={block} />;
  }
}

function Heading({ level, inlines }: { level: 1 | 2 | 3; inlines: readonly Inline[] }) {
  if (level === 1) {
    return (
      <h4 className="nd-md-h1">
        <Inlines inlines={inlines} />
      </h4>
    );
  }
  if (level === 2) {
    return (
      <h5 className="nd-md-h2">
        <Inlines inlines={inlines} />
      </h5>
    );
  }
  return (
    <h6 className="nd-md-h3">
      <Inlines inlines={inlines} />
    </h6>
  );
}

function List({ list }: { list: ListBlock }) {
  const items = list.items.map((item, index) => (
    <li key={index} value={list.ordered ? item.number : undefined}>
      {item.parts.map((part, partIndex) => {
        if (part.kind === "inlines") return <Inlines key={partIndex} inlines={part.inlines} />;
        if (part.kind === "code") {
          return (
            <pre key={partIndex} className="nd-md-pre">
              <code data-lang={part.block.lang === "" ? undefined : part.block.lang}>{part.block.text}</code>
            </pre>
          );
        }
        return <List key={partIndex} list={part.list} />;
      })}
    </li>
  ));
  return list.ordered ? <ol className="nd-md-list">{items}</ol> : <ul className="nd-md-list">{items}</ul>;
}

function Inlines({ inlines }: { inlines: readonly Inline[] }) {
  return (
    <>
      {inlines.map((inline, index) => {
        switch (inline.kind) {
          case "strong":
            return <strong key={index}>{inline.text}</strong>;
          case "em":
            return <em key={index}>{inline.text}</em>;
          case "code":
            return (
              <code key={index} className="nd-md-code">
                {inline.text}
              </code>
            );
          case "break":
            return <br key={index} />;
          case "text":
            return <span key={index}>{inline.text}</span>;
        }
      })}
    </>
  );
}
