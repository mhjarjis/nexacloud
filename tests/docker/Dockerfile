# A throwaway "VPS" for end-to-end tests: Ubuntu 24.04 with systemd, nginx,
# PHP-FPM, MySQL, PostgreSQL and two sample sites (one Laravel, one plain PHP).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      systemd systemd-sysv dbus ca-certificates curl git openssl \
      nginx php8.3-fpm php8.3-cli mysql-server postgresql \
 && apt-get clean && rm -rf /var/lib/apt/lists/* \
 # no need for these in a container
 && systemctl mask systemd-logind.service getty.target console-getty.service \
      systemd-udevd.service systemd-udev-trigger.service

COPY fixtures/ /fixtures/
RUN bash /fixtures/setup-sites.sh

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
