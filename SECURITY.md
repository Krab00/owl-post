# Security policy

## Supported versions

Only the latest release of `owl` is supported. If you are on an older version, run `owl update`
before reporting anything.

## Reporting a vulnerability

Report it privately, not in a public issue. Use GitHub's "Report a vulnerability" form —
the Security tab, then Advisories:

https://github.com/Krab00/owl-post/security/advisories/new

Tell us what you found, how to reproduce it and what an attacker gets out of it. You can expect
an acknowledgement within a few days.

## Threat model

What owlpost trusts and what it does not — keys and fingerprints, mutual TLS, consent per peer,
the read-only answering agent, and the limits on what a peer can see — is described in the
"Security" section of the [README](README.md#security).
