-- IF NOT EXISTS: databases made before the scripts.
CREATE TABLE IF NOT EXISTS subscriptions (id TEXT NOT NULL PRIMARY KEY, url TEXT NOT NULL, updated INT, error TEXT, title TEXT, used INT, total INT, expires INT, hue INT) STRICT;
