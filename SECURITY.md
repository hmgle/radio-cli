# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| Latest published release | ✅ |
| Older releases | ❌ |

## Reporting a Vulnerability

Please **do not** open a public issue for security problems.

Report privately through
[GitHub Security Advisories](https://github.com/hmgle/radio-cli/security/advisories/new)
("Report a vulnerability" on the repository's Security tab).

Include the affected version, reproduction steps, and impact. You can expect an
acknowledgement within a few days; fixes are released as patch versions when
practical. This project is maintained on a best-effort basis and makes no
response-time guarantee.

## Scope Notes

radio-cli executes external programs (`mpv`, `ffmpeg`, `ffprobe`) and connects to
third-party radio streams. Media parsing and network input are outside this
project's control; issues originating in those backends should be reported to
their respective maintainers.
