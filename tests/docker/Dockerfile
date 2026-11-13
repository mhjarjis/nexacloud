# A throwaway "VPS" for end-to-end tests: Ubuntu 24.04 with systemd, nginx,
# PHP-FPM, MySQL, PostgreSQL and two sample sites (one Laravel, one plain PHP).
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      systemd systemd-sysv dbus ca-certificates curl git openssl \
      nginx php8.3-fpm php8.3-cli mysql-server postgresql openssh-server \
      php8.3-mbstring php8.3-xml php8.3-curl php8.3-zip php8.3-sqlite3 php8.3-mysql php8.3-pgsql php8.3-intl php8.3-bcmath \
      unzip python3-minimal supervisor rclone \
 && apt-get clean && rm -rf /var/lib/apt/lists/* \
 # no need for these in a container
 && systemctl mask systemd-logind.service getty.target console-getty.service \
      systemd-udevd.service systemd-udev-trigger.service

# a fake "Synology NAS" reachable over SFTP (user nas) for backup tests
RUN useradd -m -s /bin/bash nas && install -d -o nas -g nas -m 700 /home/nas/.ssh /home/nas/backups

# FrankenPHP binary cached in the image (177 MB) so e2e runs don't depend on download speed
RUN curl -fsSL --retry 5 -o /usr/local/bin/frankenphp https://github.com/php/frankenphp/releases/download/v1.13.5/frankenphp-linux-x86_64 && chmod 755 /usr/local/bin/frankenphp

# a real Laravel app (with vendor/) for release, worker, scheduler and overview tests
RUN curl -fsSL --retry 5 https://getcomposer.org/installer | php -- --install-dir=/usr/local/bin --filename=composer \
 && COMPOSER_ALLOW_SUPERUSER=1 composer create-project --no-interaction --prefer-dist --no-progress laravel/laravel /opt/laravel-skel \
 && rm -rf /root/.cache/composer

COPY fixtures/ /fixtures/
RUN bash /fixtures/setup-sites.sh

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
