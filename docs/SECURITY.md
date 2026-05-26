# Security Policy

## Supported Versions
We currently provide security updates for the following versions:

| Version | Supported          |
| ------- | ------------------ |
| 1.0.x   | :white_check_mark: |
| < 1.0   | :x:                |

## Reporting a Vulnerability

Because Beacon & Pulse are designed for local network access and often involve unattended host control, security is of utmost importance.

If you discover a vulnerability, **do not open a public issue.**

Instead, please email the maintainers directly or use GitHub's private vulnerability reporting feature. We will acknowledge your report within 48 hours and work with you on a patch before public disclosure.

### Scope of Security Concerns
- Authentication bypass (connecting without permission).
- Remote Code Execution (RCE) via custom protocol flaws.
- Privilege escalation via the Watchdog service.
