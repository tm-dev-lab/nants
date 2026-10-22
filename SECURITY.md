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

## Local network access

With local network access on (or any address outside loopback, such as `--host` with
`--allow-remote`), every request that does not come from a real loopback connection needs an
authorized device:

- a device joins with the single-use sign-in link printed in the terminal (10 minutes) or with a
  6-digit code shown on a device that is already authorized (changes every 60 seconds, single use,
  only while the add window is open, up to 10 minutes, 5 wrong attempts per window);
- the device keeps a long random token in an `HttpOnly`, `SameSite=Strict` cookie; Nants stores
  only its hash, in a file readable only by your user; revoking a device cuts its sessions at once;
- on WSL in NAT mode, the Windows port redirect must point to the WSL IP, never to `127.0.0.1`.
  Pointing it to `127.0.0.1` would make other devices reach Nants as loopback, without a device.
  The script shown in Nants always uses the WSL IP.

Known limits:

- traffic on the local network is plain HTTP, not encrypted; someone on the same network who
  captures packets can see the device cookie. For the most security, use a network you trust;
- the preview ports (3001 to 3010) serve the generated app, not Nants, and do not ask for a device;
- attempts to type the code are limited to one per second for everyone, because the source IP is
  not reliable. An attacker on the network who keeps trying can block the add window while they
  insist. Devices already authorized keep working, and this computer can still sign in with the
  terminal link (restart Nants for a new one).

## Release packages

Releases are built from this repository by GitHub Actions and published on the
[Releases](https://github.com/tm-dev-lab/nants/releases) page. Every package has a SHA-256 file
and a build provenance attestation, which you can check with
`gh attestation verify <file> --repo tm-dev-lab/nants`.
