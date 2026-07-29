# Security Policy

FURSOY Mail handles Gmail data locally on users' devices. Please report suspected security vulnerabilities privately so they can be investigated before details become public.

## Supported versions

Security fixes are provided for the latest published FURSOY Mail release only. Older releases are not supported and may use APIs, dependencies, or security controls that are no longer current.

| Version | Supported |
| --- | --- |
| Latest published release | Yes |
| All earlier releases | No |

Please confirm that an issue still affects the latest release before reporting it when possible.

## Reporting a vulnerability

Do not report suspected vulnerabilities in a public GitHub issue, discussion, pull request, or other public channel.

Use either of these private channels:

- [GitHub Private Vulnerability Reporting](https://github.com/FURSOY/FURSOY-Mail/security/advisories/new) (preferred); or
- email [support@fursoy.com](mailto:support@fursoy.com) with a clear subject such as `Security report: brief description`.

Include the affected FURSOY Mail version, Windows version, reproduction steps, expected impact, and any suggested mitigation. Use test accounts and redact email addresses, OAuth tokens, message contents, credentials, and other personal data. Never send Google passwords or active access tokens.

You should receive an initial response within three business days. This is an acknowledgment target, not a guaranteed resolution time. Fix and disclosure timing will depend on severity, complexity, and whether upstream projects are involved.

Please allow a reasonable period for investigation and remediation before publishing details. FURSOY will coordinate disclosure with the reporter when practical and will credit reporters who request attribution.

## Scope

Reports about the FURSOY Mail application, installer, update mechanism, OAuth flow, local data handling, or project-controlled website are in scope. Vulnerabilities in Google, GitHub, Microsoft WebView2, or other third-party services should be reported to the relevant provider unless the issue is caused by FURSOY Mail's integration.
