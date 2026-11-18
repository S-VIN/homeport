-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS routing_rules (id TEXT NOT NULL PRIMARY KEY, pattern TEXT NOT NULL, action TEXT NOT NULL) STRICT;
