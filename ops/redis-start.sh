#!/bin/sh
set -eu
umask 077
# Focused fixture tests use the same validation path without starting Redis.
case "$#:${1:-}" in
  0:) validate_only=false ;;
  1:--validate-only) validate_only=true ;;
  *) printf '%s\n' 'Unsupported Redis startup argument' >&2; exit 1 ;;
esac
# Keep the credential out of server argv and container environment. Only the
# generated hex password format is supported, avoiding config-file injection.
test -f /run/secrets/redis_password || { printf '%s\n' 'Redis secret must be a regular file' >&2; exit 1; }
secret_bytes=$(wc -c < /run/secrets/redis_password | tr -d '[:space:]')
case "$secret_bytes" in
  64|65) ;;
  *) printf '%s\n' 'Redis secret must contain exactly one 64-character hex password' >&2; exit 1 ;;
esac
# Shell variables cannot represent NUL; BusyBox read silently drops that byte.
# Validate every file byte before reading into a variable, rather than relying
# on the parsed string's length/content. The size gate already bounded the file.
invalid_bytes=$(LC_ALL=C tr -d '0-9a-f\n' < /run/secrets/redis_password | wc -c | tr -d '[:space:]')
test "$invalid_bytes" = 0 || { printf '%s\n' 'Redis secret contains unsupported bytes' >&2; exit 1; }
redis_password=
IFS= read -r redis_password < /run/secrets/redis_password || test -n "$redis_password"
case "$redis_password" in
  *[!0-9a-f]*|'') printf '%s\n' 'Redis secret must be 64 lowercase hex characters' >&2; exit 1 ;;
esac
test "${#redis_password}" -eq 64 || { printf '%s\n' 'Redis secret must be 64 lowercase hex characters' >&2; exit 1; }
if "$validate_only"; then
  unset redis_password
  exit 0
fi
mkdir -p /run/redis-config
printf 'appendonly yes\nappendfsync everysec\nmaxmemory 128mb\nmaxmemory-policy noeviction\nrequirepass %s\n' "$redis_password" > /run/redis-config/redis.conf
chown redis:redis /run/redis-config/redis.conf
chmod 0400 /run/redis-config/redis.conf
unset redis_password
exec /usr/local/bin/docker-entrypoint.sh redis-server /run/redis-config/redis.conf
