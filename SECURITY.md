# Security policy

minfm manages files, archives, storage devices, partitions, filesystems, LUKS
volumes, mounts, persistent system configuration, network shares, and
credentials. Please report vulnerabilities privately when disclosure could put
users or data at risk.

## What to report privately

Examples include:

- unintended deletion, overwrite, path traversal, or extraction outside the
  selected destination;
- bypasses of system/root-device protection, target revalidation, confirmation,
  read-only behavior, or privilege boundaries;
- command or argument injection involving paths, labels, mount options, share
  addresses, or other user-controlled input;
- exposure or persistence of passphrases, credentials, keys, or secrets;
- unsafe archive handling, update/install verification, temporary-file use, or
  persistent `/etc/fstab` and `/etc/crypttab` changes.

Ordinary functional bugs without a security impact should use the public bug
report form instead.

## Reporting

Use GitHub's
[private vulnerability report](https://github.com/shukzi/minfm/security/advisories/new).
Do not open a public issue containing exploit details or sensitive data.

Include, where available:

- the affected minfm version or commit;
- Linux distribution, kernel, and relevant tool versions;
- a clear impact statement and the smallest reproducible sequence;
- sanitized logs, configuration, archive structure, or device topology;
- whether the problem reproduces in `--read-only` mode;
- a suggested mitigation or patch, if you have one.

Use disposable files, archives, shares, and storage devices for reproduction.
Never submit real passwords, credentials, encryption keys, recovery keys,
private hostnames, private network details, or data you do not intend to share.
Replace usernames, paths, UUIDs, labels, and device identifiers when their exact
values are not essential.

The current `main` branch and latest release are the useful starting points for
verification. This project does not publish separate long-term-support or
response-time commitments.
