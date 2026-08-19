# systemd units

## The idea

`matrix-relay` used to be a one-shot process: a timer woke it up, it
logged into Matrix, sent one report, and exited. That worked fine
until the bot also needed to listen for chat messages continuously -
a persistent Matrix login living alongside a timer that spawns a
second, independent login on the same account isn't viable. Matrix
ties encryption and trust to a device, and one account is only
supposed to run one device for this bot; two logins fighting over the
same account's crypto store leads to session churn and trust resets.

So the shape had to change: one long-running process
(`matrix-relay.service`, running `matrix-relay serve`) owns the single
Matrix device for the lifetime of the deployment. It syncs chat
continuously and also listens on a Unix control socket. The timer no
longer starts a new `matrix-relay` process at all - it runs a tiny
`matrix-relay trigger` client that just writes a request to that
socket and reads back the result. The daemon does the actual work;
the timer just knocks.

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

