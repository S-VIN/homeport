-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS name_filters (id TEXT NOT NULL PRIMARY KEY, pattern TEXT NOT NULL) STRICT;
