#!/usr/bin/env bash
# Build-time fixtures: two sample sites wired into nginx. Runs inside the image.
set -euo pipefail

# --- Laravel-like site with HTTPS (cert expires in 10 days -> should raise a warning)
mkdir -p /var/www/shop/public /etc/nginx/ssl
echo '<?php echo "shop ok";' > /var/www/shop/public/index.php
echo '#!/usr/bin/env php' > /var/www/shop/artisan
openssl req -x509 -newkey rsa:2048 -nodes -days 10 \
  -subj "/CN=shop.test.local" \
  -keyout /etc/nginx/ssl/shop.key -out /etc/nginx/ssl/shop.crt 2>/dev/null
git -C /var/www/shop init -q -b main
git -C /var/www/shop -c user.email=ci@example.com -c user.name=ci add -A
git -C /var/www/shop -c user.email=ci@example.com -c user.name=ci commit -qm "Initial shop commit"
git -C /var/www/shop remote add origin https://github.com/example/shop.git

cat > /etc/nginx/sites-available/shop.test.local <<'EOF'
# Laravel shop
server {
    listen 80;
    server_name shop.test.local www.shop.test.local;
    return 301 https://$host$request_uri;
}
server {
    listen 443 ssl;
    server_name shop.test.local www.shop.test.local;
    root /var/www/shop/public;   # web root
    index index.php;
    ssl_certificate     /etc/nginx/ssl/shop.crt;
    ssl_certificate_key /etc/nginx/ssl/shop.key;
    location / { try_files $uri $uri/ /index.php?$query_string; }
    location ~ \.php$ {
        include snippets/fastcgi-php.conf;
        fastcgi_pass unix:/run/php/php8.3-fpm.sock;
    }
}
EOF

# --- plain OOP PHP site
mkdir -p /var/www/portal/public_html
echo '<?php echo "portal ok";' > /var/www/portal/public_html/index.php
cat > /etc/nginx/sites-available/portal.test.local <<'EOF'
server {
    listen 80;
    server_name portal.test.local;
    root /var/www/portal/public_html;
    index index.php;
    location ~ \.php$ {
        include snippets/fastcgi-php.conf;
        fastcgi_pass unix:/run/php/php8.3-fpm.sock;
    }
}
EOF

# --- reverse proxy site
cat > /etc/nginx/conf.d/ws.conf <<'EOF'
server {
    listen 80;
    server_name ws.test.local;
    location / { proxy_pass http://127.0.0.1:6001; }
}
EOF

ln -sf /etc/nginx/sites-available/shop.test.local /etc/nginx/sites-enabled/
ln -sf /etc/nginx/sites-available/portal.test.local /etc/nginx/sites-enabled/
rm -f /etc/nginx/sites-enabled/default
nginx -t
