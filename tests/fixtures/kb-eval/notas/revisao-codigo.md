# Revisão de código

Regras dos pull requests nos repositórios do Grupo Atlas.

## Tamanho do PR

O PR ideal tem até 400 linhas alteradas, sem contar arquivos gerados. Acima de 800 linhas o PR
precisa de justificativa na descrição ou deve ser quebrado em partes.

## Aprovações

Um PR precisa de uma aprovação de quem não escreveu o código. Arquivos sob `infra/` e
`seguranca/` pedem também a aprovação do dono listado no arquivo CODEOWNERS.

## Prazo de resposta

A primeira revisão sai em até um dia útil. PR parado há mais de três dias úteis sem resposta pode
ser levado ao líder do time na reunião diária.

## Nome das branches

Branches seguem `tipo/ticket-descricao-curta`, em minúsculas, por exemplo
`feat/atl-123-filtro-por-data` ou `fix/atl-88-timeout-login`. Tipos aceitos: feat, fix, chore,
docs, refactor.

## Mensagens de commit

Mensagens no formato de commits convencionais, em português, no imperativo: `fix: corrige o
timeout do login`. O ticket vai no rodapé como `Refs: ATL-88`.

## Lista de conferência

Quem revisa confere: testes cobrindo o caso novo, logs sem dados pessoais, nenhuma credencial no
código, nomes claros e a documentação atualizada quando muda comportamento visível.
