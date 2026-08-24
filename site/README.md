# neuton.baxtergroup.io

The public site. Static, single file, no build step and no external requests.

## Deploying

The origin is an `nginx:alpine` container on the admin server bound to the
directory this file lives in.

```sh
scp site/index.html server:/tmp/neuton-index.html
ssh server 'sudo cp /tmp/neuton-index.html /home/admin/services/neuton-site/site/index.html'
```

No restart is needed: the directory is bind-mounted, so the next request serves
the new file.

| Piece | Where |
| --- | --- |
| Container | `/home/admin/services/neuton-site` on `192.168.1.85`, port `8738` |
| Vhost | `/home/admin/services/nginx/config/neuton.conf` |
| Reload nginx | `/home/admin/services/nginx/config/reload.sh` |
| DNS | Covered by the existing proxied wildcard on `baxtergroup.io` |

TLS is terminated by Cloudflare. The vhost answers on both `:80` and `:443` so
the site cannot fall through to another vhost if the SSL mode changes.

## What must stay on the page

- **The Mojang disclaimer in the footer.** Required for third-party projects:
  "NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG
  OR MICROSOFT."
- **The account and privacy section.** It is what the application review reads to
  see how sign-in and player data are handled, and it has to keep matching what
  the client actually does.
- **An honest status section.** No download links until there is something worth
  downloading.
