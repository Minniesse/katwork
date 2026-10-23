# 0004. Operator accounts and invites work without SMTP

- Status: Accepted (2026-10-23)
- Spec: §1.1 (goal 9), §13 (accounts), §19

## Context

Many self-hosted installs start without a mail relay. Password reset needs SMTP: without it, a reset request returns the same 202 and sends nothing, and the demo console mailer prints only the demo account's mail. Outside demo mode there was also no way to create an account at all, because creating a workspace needs a signed-in user. A fresh install without SMTP therefore had no first account and no way to recover a lost password, and admins had no way to add colleagues or change their roles.

## Decision

- Two operator commands run on the server: `katwork-server create-user --email <e> --workspace <name|id> --role <owner|admin|member> [--space-policy <local|hosted>]` and `katwork-server reset-link --email <e>`. Each prints a one-time set-password link to stdout and needs neither SMTP nor demo mode.
- `create-user` prints an invite link, valid 72 hours, because an admin often passes it to a colleague who opens it much later. `reset-link` prints a reset link, valid 30 minutes, with the same token kind, format and confirm route as a mailed reset. Both are single use, and using either ends the account's other sessions and pending tokens.
- `create-user` creates the account with a random password hash that nobody knows, and refuses an email that already has an account; `reset-link` serves existing accounts. Naming a workspace that does not exist creates it, but only for an owner, and only with `--space-policy`: the new workspace gets a first Space, `General`, that is local-only (`local`) or allows hosted providers (`hosted`), and that Space's default team, so the workspace can chat at once.
- Workspace owners and admins use a Members page to invite by email (which needs SMTP) or by a link they copy, and to change roles. Only an owner grants or changes the owner role, and the last owner cannot be demoted.
- Invite links are their own token kind, valid 72 hours and single use, with their own accept route. A reset token never works as an invite, and an invite token never works as a reset.
- The web issues a link only for an account that the invite itself created. Inviting an email that already has an account adds the membership and nothing else.
- A link appears only on the command's stdout or in the invite response, and never in a log.

## Consequences

- Whoever can run commands on the server can take over any account with `reset-link`. That person can already read the database, so this adds no new power; the README states it.
- A first run without SMTP works: `create-user` for the first owner, then invites by link.
- An invite link that expires unused is recovered through password reset (with SMTP) or `reset-link`. The admin cannot issue a new link, because the account now exists.
- A workspace that `create-user` creates starts with one Space and its default team, so the operator chooses its residency at the start (ADR-0005 covers local-only Spaces). The default team arrives once the default roster is on main; until then the Space is empty.
- The `password_resets` table gains a `kind` column, and the reset routes accept only reset tokens.
- Reset and invite mails are bilingual, in the user's locale order.
