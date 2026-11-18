-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS chain_exits (id TEXT NOT NULL PRIMARY KEY, name TEXT NOT NULL, link TEXT NOT NULL) STRICT;
