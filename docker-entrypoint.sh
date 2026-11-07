#!/bin/sh
# Takes ownership of the data directory, then runs the server without root privileges.
set -e
if [ "$(id -u)" = "0" ]; then
    mkdir -p /data
    chown -R cursor-byok:cursor-byok /data
    exec setpriv --reuid=cursor-byok --regid=cursor-byok --clear-groups /usr/local/bin/cursor-server "$@"
fi
exec /usr/local/bin/cursor-server "$@"
