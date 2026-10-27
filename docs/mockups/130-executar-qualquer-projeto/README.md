# Mockup da spec 130: execução de qualquer projeto

Aprovado pelo usuário em 2026-10-27 ("Gostei do mockup. Pode mandar ver."). Publicado como artefato privado em https://claude.ai/artifact/EGgg46W96jWzjXN3s5aYjn.

Os arquivos `*.dc.html` são a fonte das sete telas, e o `canvas.json` dá a disposição delas:

1. `Main.dc.html`: abre uma porta web, e a janela mostra a página.
2. `Rodando.dc.html`: fica rodando sem página, com o log ao vivo.
3. `Terminou.dc.html`: roda e termina, com a saída e os arquivos gerados.
4. `Console.dc.html`: o padrão, o Console do projeto.
5. `Falha.dc.html`: o ambiente não subiu.
6. `Sandbox.dc.html`: o projeto não roda no sandbox.
7. `Celular.dc.html`: o Console no celular.

É só referência visual. A implementação usa os tokens e as classes da interface (`web/src/apps/app/`).
