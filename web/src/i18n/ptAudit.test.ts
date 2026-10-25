// @vitest-environment node
/*
 * Auditoria dos idiomas (spec 102 T06, critério 1): nenhum texto visível em pt-BR fora do
 * dicionário no código-fonte de `web/src` (os testes ficam de fora).
 *
 * A varredura lê a AST (parser do Rolldown, como `codebase.test.ts`) e confere todo texto que pode
 * chegar à tela: texto JSX, atributos (`aria-label`, `title`, `placeholder`, `alt` e as props de
 * componentes), literais e templates (inclusive as constantes que um componente mostra). Não são
 * texto: comentários, caminhos de import, chaves do dicionário (`plan.skillStatus.criado`),
 * identificadores em maiúsculas (`TODO`) e URLs. O detector é `looksPortuguese` (`./ptLeftovers`).
 *
 * Todo o resto que parecer português precisa estar na lista de exceções abaixo, com a
 * justificativa; exceção sem uso também falha (a lista não acumula sobras).
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { parseAst } from "rolldown/parseAst";
import { describe, expect, it } from "vitest";
import { looksPortuguese } from "./ptLeftovers";

const SRC = resolve(process.cwd(), "src");

/** Arquivos que não entram, com o motivo. */
const SKIPPED_FILES: ReadonlyMap<string, string> = new Map([
  ["i18n/ptBR.ts", "o dicionário pt-BR é a fonte de todos os textos em português"],
  ["i18n/ptLeftovers.ts", "o detector guarda as palavras do português que procura"],
  ["test-setup.ts", "preparação da suíte de testes, não vai para a tela"],
  ["desktop/test-utils.ts", "apoio dos testes (dados e servidor falsos), não vai para a tela"],
  ["desktop/shell-test-utils.tsx", "apoio dos testes (dados e servidor falsos), não vai para a tela"],
  ["apps/app/app-test-utils.tsx", "apoio dos testes (dados e servidor falsos), não vai para a tela"],
]);

interface Exception {
  /** Arquivo relativo a `web/src`. */
  file: string;
  /** Texto exato, ou padrão, do literal permitido. */
  text: string | RegExp;
  why: string;
}

const SERVER_FORMAT = "marca do formato gravado pelo servidor (sempre em pt-BR): só serve para reconhecer o texto; a tela mostra a chave do dicionário";

const EXCEPTIONS: readonly Exception[] = [

  { file: "api/client.ts", text: "não modificado", why: "reserva interna do 304 da leitura de arquivo; vira `not_modified` e nunca aparece" },
  { file: "api/types.ts", text: /^(criado|atualizado|reaproveitado)$/, why: "vocabulário fixo do servidor para a situação do perfil de especialista (tipo)" },
  { file: "api/types.ts", text: "needs_login", why: "estado fixo do login pelo plano ChatGPT (`PlanStatus::NeedsLogin`, spec 116); a palavra `login` vem do dicionário pt, mas o estado não é texto de pessoa (a tela mostra `chatgpt.state.needsLogin`)" },
  { file: "apps/plan/PlanCard.tsx", text: "reaproveitado", why: "vocabulário fixo do servidor (situação do perfil); a tela mostra `plan.skillStatus.*`" },
  { file: "apps/conversation/Bubble.tsx", text: /^(aprovada automaticamente|aprovado automaticamente \(YOLO\)|\[rodada autônoma( YOLO)?\])$/, why: SERVER_FORMAT },
  { file: "apps/conversation/PausedRunCard.tsx", text: "Análise de recuperação", why: SERVER_FORMAT },
  { file: "apps/shared/activityText.ts", text: "repetindo (", why: `aviso de nova tentativa (\`llm_client::retry_notice\`); ${SERVER_FORMAT} (\`activity.retry.detail\`)` },
  { file: "copy.ts", text: "Defina a stack e o comando de testes (test_command) da spec e atualize-a para aprovação.", why: "mensagem interna enviada ao Arquiteto (`internal: true`), nunca exibida; prompts internos ficam em português (decisão 3)" },
  { file: "copy.ts", text: "provedor no limite", why: `prefixo do detalhe da falha por limite do provedor; ${SERVER_FORMAT}` },
  { file: "copy.ts", text: "provider_plan_needs_login", why: "código fixo da falha do provedor (`ProviderFailure::Plan`, spec 116); a palavra `login` vem do dicionário pt, mas o código não é texto de pessoa (a tela mostra `providerMsg.planNeedsLogin`)" },

  { file: "desktop/theme/contrast.ts", text: /^(cor inválida:|o fundo do contraste precisa ser opaco)$/, why: "erro de programação dos tokens de tema (lançado nos testes de contraste), nunca exibido" },
  { file: "escalation.ts", text: /^(comando|trecho do erro|o que foi tentado|por que nao resolveu|opcoes)$/, why: `campos do bloco "Contexto" da escalada; ${SERVER_FORMAT}` },
  { file: "i18n/en.ts", text: "Português (Brasil)", why: "nome do idioma na própria língua, no seletor de idioma (o mesmo nos dois dicionários)" },
  { file: "imageResize.ts", text: /^(canvas indisponível|falha ao converter a imagem)$/, why: "erro interno capturado pelo anexo; a tela mostra `attachments.readFailed`" },
  { file: "main.tsx", text: "elemento #root ausente", why: "falha de inicialização da página (sem `#root` não há interface para mostrar texto)" },
  {
    file: "recoveryAnalysis.ts",
    text: /^- (Causa raiz|Estratégia sugerida|Estratégia|Novo test_command|Novos checks|Novo allowed_scope|Escopo ampliado|Escopo não ampliado|Escopo recusado|Proposta|Opções descartadas|comando|código|por que não resolveu|Contexto|trecho do erro|Opções para o humano):$/,
    why: `rótulos das linhas da análise de recuperação; ${SERVER_FORMAT} (\`recoveryText.*\`)`,
  },
  {
    file: "recoveryAnalysis.ts",
    text: /^- (Decisão: (configuração de testes revisada \(REVISE_CONFIG\)|tarefa revisada \(REVISE_TASK\)|nova estratégia \(RETRY\)|escalar ao humano)|Configuração de testes corrigida e aplicada pelo servidor|Contagem: não conta como ciclo \(escalada determinística\))$/,
    why: `linhas fixas de decisão e contagem da análise de recuperação; ${SERVER_FORMAT}`,
  },
  {
    file: "recoveryAnalysis.ts",
    text: /^- (Pergunta ao humano|Tarefas novas|Apontamentos dispensados):$|^- Decisão: (tarefa dividida \(SPLIT\)|critérios ajustados ao que o ambiente prova \(ADJUST\)|pergunta ao humano \(ASK\))$|^- Ajuste não aplicado: os critérios não puderam ser gravados; a tarefa segue com a estratégia \(RETRY\)$/,
    why: `rótulos e linhas fixas do SPLIT, ADJUST e ASK (spec 124 T03, \`recovery_plan.rs\`); ${SERVER_FORMAT} (\`recoveryText.*\`)`,
  },
  { file: "recoveryAnalysis.ts", text: /^- opções: /, why: `linhas fixas de opções da escalada (\`recovery.rs\`); ${SERVER_FORMAT} (\`recoveryText.options.*\`)` },
  {
    file: "recoveryAnalysis.ts",
    text: /^(Ambiente docker ajustado: o run será relançado com o ambiente novo\.|\(sem saída registrada\)|\(citada pelo modelo\))$/,
    why: `linhas fixas avulsas da análise de recuperação; ${SERVER_FORMAT}`,
  },
  {
    file: "recoveryAnalysis.ts",
    text: /^(proposta de configuração de testes inválida|correção da configuração de testes não aprovada|falha ao gravar a configuração de testes|análise indisponível|ambiente: o comando de testes não executou|ambiente: reescrever a tarefa não resolve falha de ambiente \(a verificação não pode ser rebaixada\)|a proposta removeria verificação|a proposta repetiria o comando de testes|ambiente: limite de rodadas autônomas atingido sem resolver a falha|limite de ciclos de recuperação atingido|ajuste do ambiente docker não aplicado|aprovação do ambiente negada pelo humano|docker indisponível)$/,
    why: `causas fixas do servidor na análise de recuperação; ${SERVER_FORMAT} (\`recoveryText.cause.*\`)`,
  },
  {
    file: "reportMarkdown.ts",
    text: /^(como( testar| parar| usar| instalar)?|executar|rodar|iniciar|subir|pr[ée][ -]requisitos|requisitos|instalação|instalacao|instalar|decisões|decisoes|limitações|limitacoes|próximos passos|proximos passos)$/,
    why: "títulos de seção do relatório em pt-BR (o relatório segue o idioma de quando foi gerado), só para achar a seção de execução",
  },
  {
    file: "reviewMessage.ts",
    text: /^(Critérios:|Apontamentos:|Observações \(não bloqueiam\):|Cenário de falha:|observação \(arquivo não mudou nesta rodada\)|baixa confiança|Capturas avaliadas:|Aprovado|Pediu ajustes|Verificação final:|novo|Resolvidos:|Respostas do Desenvolvedor:|Decisões do Arquiteto:)$/,
    why: `cabeçalhos e vereditos da mensagem do Revisor; ${SERVER_FORMAT} (\`review.*\`)`,
  },
];

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) && !name.endsWith(".d.ts")) out.push(full);
  }
  return out;
}

type Node = Record<string, unknown> & { type?: string; start?: number; end?: number };

function visit(node: unknown, fn: (n: Node) => void): void {
  if (Array.isArray(node)) {
    for (const child of node) visit(child, fn);
    return;
  }
  if (typeof node !== "object" || node === null) return;
  const n = node as Node;
  if (typeof n.type === "string") fn(n);
  for (const [key, value] of Object.entries(n)) {
    if (key !== "start" && key !== "end" && key !== "type") visit(value, fn);
  }
}

/** Chave do dicionário (`a.b.c`) ou identificador em maiúsculas (`TODO`, `IN_PROGRESS`). */
const NOT_TEXT = /^(?:[a-z][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+|[A-Z][A-Z0-9_]*)$/;
const URL_RE = /\bhttps?:\/\/\S+/g;

interface Hit {
  file: string;
  line: number;
  text: string;
}

/** Textos (literais, templates e texto JSX) que parecem pt-BR, com a linha. */
function hitsOf(file: string, source: string): Hit[] {
  const ast = parseAst(source, { lang: file.endsWith(".tsx") ? "tsx" : "ts" }) as unknown;
  const moduleSources = new Set<unknown>();
  visit(ast, (n) => {
    if (n.type === "ImportDeclaration" || n.type === "ExportNamedDeclaration" || n.type === "ExportAllDeclaration") moduleSources.add(n.source);
  });
  const out: Hit[] = [];
  visit(ast, (n) => {
    if (moduleSources.has(n)) return;
    let text: string | null = null;
    if (n.type === "Literal" && typeof n.value === "string") text = n.value;
    else if (n.type === "JSXText") text = String(n.value);
    else if (n.type === "TemplateElement") {
      const value = n.value as { cooked?: string | null; raw?: string } | undefined;
      text = value?.cooked ?? value?.raw ?? "";
    }
    if (text === null) return;
    const trimmed = text.trim();
    if (trimmed === "" || NOT_TEXT.test(trimmed) || !looksPortuguese(trimmed.replace(URL_RE, " "))) return;
    out.push({ file, line: source.slice(0, n.start ?? 0).split("\n").length, text: trimmed });
  });
  return out;
}

function allowedBy(hit: Hit): Exception | undefined {
  return EXCEPTIONS.find((e) => e.file === hit.file && (typeof e.text === "string" ? e.text === hit.text : e.text.test(hit.text)));
}

describe("auditoria dos idiomas: nenhum pt-BR fora do dicionário (spec 102 T06)", () => {
  const files = walk(SRC)
    .map((full) => relative(SRC, full).split(sep).join("/"))
    .filter((file) => !SKIPPED_FILES.has(file));
  const hits = files.flatMap((file) => hitsOf(file, readFileSync(join(SRC, file), "utf8")));

  it("todo texto em pt-BR do código está no dicionário ou numa exceção justificada", () => {
    expect(files.length).toBeGreaterThan(100);
    const unexpected = hits.filter((hit) => allowedBy(hit) === undefined).map((h) => `${h.file}:${h.line}: ${h.text.slice(0, 120)}`);
    expect(unexpected).toEqual([]);
  });

  it("toda exceção tem justificativa e ainda é usada; os arquivos ignorados existem", () => {
    for (const exception of EXCEPTIONS) expect(exception.why.trim().length, String(exception.text)).toBeGreaterThan(20);
    const used = new Set(hits.map(allowedBy).filter((e) => e !== undefined));
    const stale = EXCEPTIONS.filter((e) => !used.has(e)).map((e) => `${e.file}: ${String(e.text)}`);
    expect(stale).toEqual([]);
    const all = new Set(walk(SRC).map((full) => relative(SRC, full).split(sep).join("/")));
    for (const file of SKIPPED_FILES.keys()) expect(all.has(file), file).toBe(true);
  });

  it("a varredura acha texto JSX, atributos e constantes em pt-BR", () => {
    const sample = [
      'const LABEL = "Precisa de você";',
      "export function A() {",
      '  return <div aria-label="Fechar" title={`Abrir o projeto ${x}`}><Item label="Retomar" />Carregando o pedido</div>;',
      "}",
      'const ok = "Needs you"; const key = "plan.skillStatus.criado"; const url = "https://api.openai.com/v1";',
    ].join("\n");
    expect(hitsOf("amostra.tsx", sample).map((h) => h.text)).toEqual(["Precisa de você", "Fechar", "Abrir o projeto", "Retomar", "Carregando o pedido"]);
  });

  it("o detector pega o que deve e deixa o inglês passar", () => {
    for (const pt of ["Precisa de você", "falha interna", "Abrir Plano", "Retomar", "nao foi possivel conectar", "Carregando o pedido", "por que nao resolveu"]) {
      expect(looksPortuguese(pt), pt).toBe(true);
    }
    for (const en of ["Needs you", "internal failure", "Open Plan", "Resume", "No tasks yet. They show up here once the plan is approved.", "nd-btn nd-btn-sm", "To do", "A1 B2 da"]) {
      expect(looksPortuguese(en), en).toBe(false);
    }
  });
});
