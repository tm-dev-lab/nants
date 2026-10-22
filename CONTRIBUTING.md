# Contributing to Nants

Thank you for your interest in Nants.

## How we work

Nants is developed through an internal process that we are still consolidating. For now, we
welcome issues (bug reports and suggestions), but we don't accept pull requests. This repository
is a published mirror of the product; changes are made upstream and published here with each
release. We will revisit this as the process matures.

Pull requests opened here are closed automatically with a pointer to this file. If you have a fix
in mind, please describe it in an issue instead: the problem, the cause you found and the change
you suggest. We read every issue.

## Opening a good issue

Before opening one, search the [existing issues](https://github.com/tm-dev-lab/nants/issues) to
see if it was already reported.

### Bug reports

Use the **Bug report** template and include:

- the Nants version (`nants --version`);
- your system: Linux distribution or Windows with WSL2, and the architecture (x86_64 or aarch64);
- the output of `nants doctor`;
- the provider and model of each agent, if the problem involves the agents (never paste your API
  key);
- the steps to reproduce, what you expected and what happened;
- relevant logs or screenshots, with any secret removed.

### Suggestions

Use the **Feature request** template and describe:

- the problem you want to solve, before the solution you have in mind;
- how you work today and why it is not enough;
- any alternative you considered.

### Security

Do not report vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Nants is licensed under the [Apache License 2.0](LICENSE).
