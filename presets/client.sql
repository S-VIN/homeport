-- Client: desktop packages start a new installation with this script.
-- A local proxy, tests with automatic selection, DNS and statistics, ads
-- blocked, the local network and Russian sites directly.

INSERT INTO kernel_plugins (id, enabled) VALUES
  ('local-proxy', 1),
  ('health-checks', 1),
  ('name-filters', 1),
  ('dns', 1),
  ('statistics', 1);

INSERT INTO kernel_settings (id, json) VALUES
  ('servers.tests', json_object('check_secs', 900)),
  ('servers.auto', json_object('enabled', json('true')));

INSERT INTO http_tests (id, url, expected) VALUES
  (new_id(), 'https://web.telegram.org/', 'Below400'),
  (new_id(), 'https://youtube.com/', 'Below400'),
  (new_id(), 'https://www.google.com/generate_204', 'NoContent'),
  (new_id(), 'https://api.anthropic.com/api/hello', 'Success');

INSERT INTO name_filters (id, pattern) VALUES
  (new_id(), '*рф*'),
  (new_id(), '*россия*'),
  (new_id(), '*ru*'),
  (new_id(), '*torrent*'),
  (new_id(), '*ukraine*'),
  (new_id(), '*nigeria*'),
  (new_id(), '*albania*');

-- First match wins; everything else goes through the selected server.
INSERT INTO routing_rules (id, pattern, action) VALUES
  (new_id(), 'geosite:category-ads-all', 'Block'),
  (new_id(), 'geoip:private', 'Direct'),
  (new_id(), 'geosite:private', 'Direct'),
  (new_id(), 'geoip:ru', 'Direct');
