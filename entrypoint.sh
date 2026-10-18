#!/bin/sh
set -eu

# Compose passes optional credentials as empty strings when unset in .env.
if [ -z "${AWS_SESSION_TOKEN:-}" ]; then
    unset AWS_SESSION_TOKEN
fi

if [ "${1:-cron}" != "cron" ]; then
    exec /usr/local/bin/backup-repos-checker "$@"
fi
if [ "$#" -gt 1 ]; then
    echo "error: cron does not accept additional arguments" >&2
    exit 2
fi
if [ ! -f /config/config.toml ] || [ ! -r /config/config.toml ]; then
    echo "error: mount a readable config.toml at /config/config.toml" >&2
    exit 2
fi

# Non-overlapping defaults run exactly one scan daily, including healthy mail on the 1st.
schedule=${CRON_SCHEDULE:-5 0 2-31 * *}
healthy_schedule=${HEALTHY_CRON_SCHEDULE:-5 0 1 * *}
for value in "$schedule" "$healthy_schedule"; do
    if ! printf '%s\n' "$value" | awk 'NR != 1 || NF != 5 || /[^a-zA-Z0-9*,\/ -]/ { exit 1 }'; then
        echo "error: CRON_SCHEDULE and HEALTHY_CRON_SCHEDULE must each be a single five-field cron expression" >&2
        exit 2
    fi
done

umask 077
printf '%s cd /config && /usr/local/bin/backup-repos-checker report > /proc/1/fd/1 2> /proc/1/fd/2\n' \
    "$schedule" > /etc/crontabs/root
printf '%s cd /config && /usr/local/bin/backup-repos-checker report --send-even-if-healthy > /proc/1/fd/1 2> /proc/1/fd/2\n' \
    "$healthy_schedule" >> /etc/crontabs/root
echo "Scheduling backup reports: $schedule; including healthy reports: $healthy_schedule (TZ=${TZ:-Etc/UTC})"
exec crond -f -d 8
