# Security policy

## Reporting a vulnerability

Do not open a public issue. Report it privately on GitHub: the repository's **Security** tab, **Report a vulnerability** (GitHub private vulnerability reporting). Only the maintainers see the report. Include what you found, how to reproduce it, the version (`--version` of the binary or the plugin manifest) and the impact you see. Do not include real secrets or personal data: describe them.

You get an answer within seven days. A confirmed problem is fixed in the latest minor version first, and the disclosure is coordinated with you: the fix and an advisory are published together, crediting you if you wish.

## Supported versions

The latest minor version receives fixes. Older minors get a fix only for problems that expose secrets or run commands the user did not ask for.

## What counts

This project runs commands on your machine and guards tool calls of an AI agent. Reports about a guard bypass, a secret reaching a log, a request leaving the machine to a host that is not on the network list, or the local server accepting requests it should refuse are in scope.
