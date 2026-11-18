-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS http_tests (id TEXT NOT NULL PRIMARY KEY, url TEXT NOT NULL, expected TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS test_results (id TEXT NOT NULL PRIMARY KEY, server TEXT NOT NULL, test TEXT NOT NULL, status INT, passed INT NOT NULL, delay_ms INT, failure TEXT) STRICT;
