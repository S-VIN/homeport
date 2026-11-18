-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS servers (id TEXT NOT NULL PRIMARY KEY, source TEXT NOT NULL, name TEXT NOT NULL, protocol TEXT NOT NULL, address TEXT NOT NULL, port INT NOT NULL, options TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS server_health (id TEXT NOT NULL PRIMARY KEY, ping_ms INT, rating INT, checked INT, scores TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS selection_memory (id TEXT NOT NULL PRIMARY KEY, score REAL, quarantine_until INT NOT NULL, better_runs INT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS selection_events (id TEXT NOT NULL PRIMARY KEY, server TEXT, at INT NOT NULL) STRICT;
