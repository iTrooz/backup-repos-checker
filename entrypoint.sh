#!/bin/sh
set -eu

if [ ! -f /config/config.toml ] || [ ! -r /config/config.toml ]; then
    echo "error: mount a readable config.toml at /config/config.toml" >&2
    exit 2
fi

schedule=$CRON_SCHEDULE
healthy_schedule=$HEALTHY_CRON_SCHEDULE

umask 077
printf '%s cd /config && /usr/local/bin/backup-repos-checker report > /proc/1/fd/1 2> /proc/1/fd/2\n' \
    "$schedule" > /etc/crontabs/root
printf '%s cd /config && /usr/local/bin/backup-repos-checker report --send-even-if-healthy > /proc/1/fd/1 2> /proc/1/fd/2\n' \
    "$healthy_schedule" >> /etc/crontabs/root
echo "Scheduling backup reports: $schedule; including healthy reports: $healthy_schedule"
exec crond -f -d 8
