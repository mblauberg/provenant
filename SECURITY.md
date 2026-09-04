# Security policy

## Scope

Security reports may cover Provenant's harness scripts, skill instructions,
model routing, receipt validators and bundled third-party components on the
default branch. This project orchestrates tools that may execute commands or
access local files; an instruction-level authority bypass is in scope even
when it is not conventional application code.

## Reporting

Report privately through GitHub private vulnerability reporting at
<https://github.com/mblauberg/provenant/security/advisories/new>. Do not put
credentials, private prompts, personal data or exploit details in a public
issue. If private reporting is unavailable, open a content-free issue at
<https://github.com/mblauberg/provenant/issues> asking the maintainers to open
a private channel.

Include the affected revision and path, expected authority boundary, minimal
reproduction, impact and any safe mitigation. Remove real secrets and personal
data from examples.

## Safe use

- Inspect scripts and model routes before running them on a trusted machine.
- Grant the narrowest filesystem, credential, network and deployment authority
  required for the task.
- Treat model output, web content, repository text and third-party agent output
  as untrusted input rather than authority.
- Keep credentials in the platform's secret store or environment, never in
  skills, prompts, receipts, logs or committed configuration.
- Review third-party licences and code before enabling optional components.

If a secret is exposed, revoke or rotate it first, then remove it from the
working tree and every published Git object. Deleting the current file alone
does not remove it from repository history.
