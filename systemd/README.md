# systemd units

Replaces the old timer-spawns-a-fresh-process setup: `matrix-relay
serve` now runs continuously as a single Matrix device, and report
triggers reach it over a control socket instead of starting a second
client.

## Install

```sh
sudo cp matrix-relay.socket matrix-relay.service \
        matrix-relay-report.service matrix-relay-report.timer \
        /etc/systemd/system/

sudo mkdir -p /etc/matrix-relay
sudo cp matrix-relay.env.example /etc/matrix-relay/matrix-relay.env
sudo chmod 0600 /etc/matrix-relay/matrix-relay.env
# edit /etc/matrix-relay/matrix-relay.env with real values

sudo systemctl daemon-reload
sudo systemctl enable --now matrix-relay.socket
sudo systemctl enable --now matrix-relay.service
sudo systemctl enable --now matrix-relay-report.timer
```

`matrix-relay.socket` owns creation and permissions of
`/run/matrix-relay/control.sock`; `matrix-relay.service` adopts it via
socket activation (`LISTEN_FDS`) rather than binding it itself.

## Multiple report types

`matrix-relay-report.service`/`.timer` as shipped here trigger the
daily report on a fixed schedule. For additional report types (e.g.
weekly, quick wins, backlog), copy the pair under new names and adjust
`--resource` and `OnCalendar`, e.g.:

```sh
sudo cp matrix-relay-report.service /etc/systemd/system/matrix-relay-report-weekly.service
sudo cp matrix-relay-report.timer /etc/systemd/system/matrix-relay-report-weekly.timer
# edit the .service's ExecStart to use --resource kid://weekly_report
# edit the .timer's OnCalendar for the desired schedule
sudo systemctl daemon-reload
sudo systemctl enable --now matrix-relay-report-weekly.timer
```
