# Security Policy

thermalwriter is a user-session daemon that talks to USB hardware, reads local sensor files, exposes a D-Bus control interface, and optionally starts Xvfb child processes for mirror mode. Please report security issues privately before opening a public issue.

## Reporting

Use this private path: send a private/direct Mastodon message to `@mgaruccio@hachyderm.io`.

Include:

- Affected version or commit.
- Platform and hardware details.
- Reproduction steps.
- Expected impact.

## Scope

Security-sensitive areas include path traversal in layout/background selection, D-Bus method behavior, child-process handling in Xvfb mode, udev/RAPL setup, and USB transport parsing.

## D-Bus and streaming trust boundary

The daemon owns the session-bus name `com.thermalwriter.Service` and accepts requests from same-user session-bus clients. Same-user processes already have the user's privileges, so the D-Bus API is not a privilege boundary.

Xvfb streaming intentionally starts local child processes as the daemon user. Generic streaming requests are session-only, never persisted as the boot default, and use structured argv with `argv[0]` required to be an absolute executable path. Built-in stream presets are resolved to absolute paths through the daemon's own `PATH`; custom GUI commands require the user to choose an executable path. The daemon logs stream starts and kills stream child process groups when streaming stops. The legacy shell-string D-Bus streaming path is rejected.

