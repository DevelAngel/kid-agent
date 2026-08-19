# systemd units

## The idea

Matrix ties encryption and trust to a device, and this bot is only
meant to run a single device for its account - two logins on the same
account fighting over the same crypto store leads to session churn
and trust resets. Since the bot needs both a continuous chat listener
and periodic report sending, both have to happen from the same
process, under the same login.

`matrix-relay.service` (running `matrix-relay serve`) is that single
long-running process: it owns the Matrix device for the lifetime of
the deployment, syncs chat continuously, and listens on a Unix
control socket. Report triggers don't start a new `matrix-relay`
process - the timer instead runs a tiny `matrix-relay trigger` client
that writes a request to that socket and reads back the result. The
daemon does the actual work; the timer just knocks.

The socket itself is owned by `matrix-relay.socket`, not by the
daemon. systemd creates `/run/matrix-relay/control.sock` with the
right permissions before the daemon ever starts, and hands the
already-bound socket over via socket activation. That sidesteps a
startup race (the timer firing before the daemon has gotten around to
binding its own socket) and means a crashed-and-restarting daemon
doesn't need to recreate the socket file at all - it just reconnects
to the same fd on the next start.

## Report types and resource URIs

Each report the daemon can send corresponds to a resource URI read
from the "generate message" MCP server, following a `kid://report/<name>`
scheme - e.g. `kid://report/daily` for the daily report. A given
report type is just a `matrix-relay-report*.service`/`.timer` pair
whose `ExecStart` names that resource and whose `OnCalendar` sets its
own schedule; the daily pair shipped here is the template for adding
weekly, quick-wins, or backlog reports alongside it, each under its
own unit name and cadence.

## Config

`matrix-relay.service` reads its Matrix credentials and MCP endpoint
config from an environment file (`matrix-relay.env.example` here is
the template) rather than inline in the unit, since those values are
secrets and differ per deployment.

