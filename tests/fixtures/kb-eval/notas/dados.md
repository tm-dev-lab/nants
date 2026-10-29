# Retenção e privacidade de dados

Como guardamos e apagamos dados pessoais, seguindo a LGPD.

## Classificação

Os dados são públicos, internos, confidenciais ou restritos. CPF, endereço, dados de saúde e de
cartão são restritos: só ficam em bancos com criptografia em repouso e acesso registrado.

## Prazos de retenção

Logs de aplicação ficam 30 dias. Registros de acesso ficam 6 meses, como pede o Marco Civil.
Notas fiscais e dados contábeis ficam 5 anos. Conta de cliente encerrada é anonimizada em 90 dias.

## Anonimização

Anonimizar é trocar nome, e-mail, telefone e documento por valores sem volta (hash com sal por
ambiente) e apagar campos livres. Mascarar na tela não é anonimizar.

## Pedidos do titular

Pedido de acesso, correção ou exclusão do titular chega pelo canal do encarregado de dados e tem
resposta em até 15 dias. A exclusão vale também para as cópias em ambientes de teste.

## Cópias de segurança

O backup do banco principal roda a cada 6 horas e fica 35 dias. Uma restauração de teste por mês
prova que o backup funciona; o resultado vai para o relatório de confiabilidade.

## Dados em ambiente de teste

Ambiente de teste nunca recebe cópia de produção sem anonimização. Para testes, use o gerador de
massa sintética do time de dados.
