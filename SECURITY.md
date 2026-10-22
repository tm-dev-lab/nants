# Security policy

## Supported versions

Only the latest released version of Nants receives security fixes. Update with the install
command before reporting:

```sh
curl -fsSL https://nants.tmdevlab.com/install.sh | sh
```

| Version         | Supported |
| --------------- | --------- |
| Latest release  | Yes       |
| Older releases  | No        |

## Reporting a vulnerability

Please report vulnerabilities privately, never in a public issue or discussion.

1. Open the [Security tab](https://github.com/tm-dev-lab/nants/security) of this repository.
2. Click **Report a vulnerability** (GitHub Security Advisories).
3. Describe the issue, the affected version (`nants --version`), the steps to reproduce and the
   impact you expect.

Do not include real API keys or other secrets in the report.

We will acknowledge your report, keep you informed while we investigate and credit you in the
advisory if you wish. Please give us reasonable time to publish a fix before disclosing the issue.

## Scope

Nants runs locally. Examples of what is in scope:

- a way for another website, device or user to reach or control the local server without the
  user's consent;
- leaks of API keys or other secrets through the interface, logs, events or generated files;
- ways for agent commands to escape the permission checks or the project folder;
- problems in the installer or in the verification of release packages.
