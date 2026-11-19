-- Server: the Docker image starts a new installation with this script.
-- VLESS for the clients; everything else leaves the server directly.

INSERT INTO kernel_plugins (id, enabled) VALUES
  ('vless-inbound', 1),
  ('logs', 1),
  ('statistics', 1);

-- docker/compose.yaml: VLESS_DOMAIN, by default the panel's domain.
-- Caddy owns port 80, so no HTTP redirect.
INSERT INTO kernel_settings (id, json) VALUES
  ('vless-inbound.settings', json_object(
    'enabled', json('true'),
    'domain', env('HOMEPORT_VLESS_DOMAIN'),
    'port', 443,
    'redirect_http', json('false')));

-- First match wins. Clients must not reach this machine's own services:
-- the panel listens on localhost without a login.
INSERT INTO routing_rules (id, pattern, action) VALUES
  (new_id(), 'geosite:category-ads-all', 'Block'),
  (new_id(), 'geoip:private', 'Block'),
  (new_id(), 'geosite:private', 'Block'),
  (new_id(), '*', 'Direct');
