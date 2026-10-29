# Processo de deploy

Como o código chega à produção no Grupo Atlas.

## Janelas de publicação

Publicações em produção acontecem de segunda a quinta, das 10h às 16h, no horário de Brasília.
Sexta-feira, véspera de feriado e a última semana de dezembro são congeladas: só correções de
SEV1 ou SEV2 entram, com o aval do gerente de plantão.

## Aprovação da mudança

Toda mudança em produção precisa de um registro no quadro de mudanças com o link do PR, o plano
de retorno e o responsável. Mudança de risco alto (banco, rede, autenticação) exige duas
aprovações: a do líder técnico do time e a da equipe de plataforma.

## Implantação gradual

O pipeline publica primeiro em 5% das instâncias (canário) e espera 20 minutos olhando a taxa de
erro e a latência p95. Se a taxa de erro subir mais de 1 ponto percentual, o pipeline para sozinho.
Depois do canário vêm 50% e, por fim, 100%.

## Plano de retorno

Todo deploy tem retorno em até 15 minutos. O retorno padrão é publicar de novo a imagem anterior
pelo botão "Reverter" do pipeline. Mudança que não volta assim (migração destrutiva, por exemplo)
precisa de um plano de retorno escrito no registro da mudança.

## Migrações de banco

Migrações seguem o padrão expandir e contrair: primeiro a coluna nova entra sem remover a antiga,
o código passa a escrever nas duas, e só depois de um ciclo de publicação a antiga sai. Nenhuma
migração trava tabela com mais de 1 milhão de linhas em horário comercial.

## Chaves de funcionalidade

Funcionalidade nova entra desligada atrás de uma chave no serviço de flags. A chave tem dono e
data de remoção; chave esquecida há mais de 90 dias aparece no relatório semanal de dívida.
