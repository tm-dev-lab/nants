# Padrões de engenharia Perl do Grupo Atlas

Padrões obrigatórios para todo programa Perl do Grupo Atlas, de CLIs a serviços HTTP. Cada padrão
tem uma regra objetiva, conferida na revisão.

## P01 Estrutura de pastas

Os módulos ficam em `lib/Atlas/`, com o pacote `Atlas::<Nome>` (por exemplo
`lib/Atlas/Despesas.pm` com `package Atlas::Despesas;`). Os executáveis ficam em `bin/` e os
testes em `t/`. Nada de código solto na raiz do projeto.

## P02 Cabeçalho dos arquivos

Todo arquivo Perl (`.pl`, `.pm`, `.t` e os executáveis de `bin/`) tem `use strict;` e
`use warnings;` nas dez primeiras linhas.

## P03 Versão do módulo

Cada módulo de `lib/Atlas/` declara a versão com `our $VERSION = '0.1.0';`, no formato
`MAIOR.MENOR.CORRECAO`, entre aspas simples.

## P04 Opções de linha de comando

Opções e argumentos de linha de comando são lidos só com `Getopt::Long` (`GetOptions`). Ler
`@ARGV` à mão, com `shift` ou índices, não é aceito.

## P05 Prefixo de log

Todo log vai para STDERR no formato `[atlas] NIVEL mensagem`, com NIVEL igual a `INFO`, `WARN`
ou `ERROR`. Por exemplo: `[atlas] INFO servidor ouvindo na porta 8080`.

## P06 Códigos de erro

Todo erro mostrado ao usuário ou devolvido pela API tem um código `ATL-` seguido de três dígitos,
por exemplo `ATL-404` para recurso inexistente e `ATL-400` para pedido inválido. Na API HTTP, o
corpo do erro é `{"erro":{"codigo":"ATL-404","mensagem":"..."}}`; na CLI, a linha de erro em
STDERR começa pelo código: `ATL-002: arquivo não encontrado`.

## P07 Código de saída de uso inválido

Com opção ou argumento inválido, o programa imprime o uso em STDERR e sai com o código 64
(EX_USAGE). Outros erros saem com 1.

## P08 Nome dos arquivos de teste

Os testes ficam em `t/` com o nome `NN-assunto.t`, com dois dígitos no início, por exemplo
`t/01-soma.t` e `t/02-filtro-mes.t`.

## P09 Funções de teste

Cada caso de teste é uma sub com o prefixo `test_` (por exemplo `sub test_total_por_categoria`),
chamada no fim do arquivo. O arquivo imprime `ok N` ou `not ok N` por caso e sai com código
diferente de zero se algum falhar.

## P10 Configuração pelo ambiente

Toda configuração lida do ambiente usa variáveis com o prefixo `ATLAS_`, por exemplo
`ATLAS_PORT` e `ATLAS_LOG_LEVEL`. Nenhuma outra variável de ambiente é lida pelo programa.
