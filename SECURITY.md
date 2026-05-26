# Security Policy

thermalwriter is a user-session daemon that talks to USB hardware, reads local sensor files, exposes a D-Bus control interface, and optionally starts Xvfb child processes for mirror mode. Please report security issues privately before opening a public issue.

## Reporting

Email the maintainer at the address listed on the GitHub profile for `mgaruccio`, or open a private GitHub security advisory if the repository has advisories enabled.

Include:

- Affected version or commit.
- Platform and hardware details.
- Reproduction steps.
- Expected impact.

## Scope

Security-sensitive areas include path traversal in layout/background selection, D-Bus method behavior, child-process handling in Xvfb mode, udev/RAPL setup, and USB transport parsing.

